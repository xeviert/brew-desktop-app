//! Phase 2d. Where a byte offset moves to, and what a half-written line costs.
//!
//! Pure: bytes and a stored offset in, a decision out. No HTTP, no database, no clock —
//! [`ingest`] is a function of its arguments and nothing else, which is why every rule
//! below is tested against a `Vec<u8>` with no server and no `Db`. Phase 2e turns one
//! [`SyncStep`] into one transaction.
//!
//! There is no remainder buffer, and its absence is the design. `byte_offset` only ever
//! advances past a complete, terminated record, so the next `Range: bytes=<offset>-`
//! re-requests the partial tail for free — at most one record, correct across a process
//! restart, and with no schema column to hold it. What that costs is a rule with no
//! exceptions: **split before you parse**, spelled out on
//! [`split_complete_lines`](crate::fermentrack::split_complete_lines).
//!
//! Two arithmetic rules carry the rest of the weight:
//!
//! 1. **Every offset derives from `FetchOutcome::Fresh::offset`**, never from
//!    `SyncState::byte_offset`, and `offset == 0` is the only discriminator between a
//!    first fetch and a resume. After a [`Restart`] the caller fetches from 0 while the
//!    stored offset is still stale, so a base taken from state would claim a range that
//!    was never parsed.
//! 2. **Never compute an offset from a buffer length.** Two truncations stack — the
//!    split cuts the tail, `stopped_early` cuts inside the complete portion — and only
//!    `ParseReport::bytes_consumed` survives both.

use crate::{
    Error, Result,
    fermentrack::{
        FetchOutcome, LogRow, ParseReport, csv_log::split_first_record, parse_with_header,
        preview_bytes, split_complete_lines,
    },
    units::Timestamp,
};

/// The `sync_state` row.
///
/// [`ingest`] reads only `remote_path` (for error messages), `byte_offset` (advisory —
/// see the module docs) and `header`. The rest is carried so that Phase 2e can load and
/// store the row whole.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncState {
    pub source_id: i64,
    pub remote_path: String,
    pub byte_offset: u64,
    /// The header line as fetched, **without its terminator** — a CRLF that became an
    /// LF somewhere in transit is not a column change.
    pub header: Option<String>,
    pub last_ts: Option<Timestamp>,
    pub last_error: Option<String>,
    pub synced_at: Timestamp,
}

/// An incomplete record larger than this is damage, not Fermentrack mid-`writerow`.
///
/// Without a cap, one stray `"` puts `split_complete_lines` inside a quoted field for
/// the rest of the file: every poll finds no complete record, writes nothing, reports
/// nothing, and the source stops **silently** — the one outcome 2d exists to prevent. A
/// real record is 60–120 bytes, so this is roughly 500× headroom; at the default 30 s
/// cadence a stuck offset crosses it in about four hours. The trade is deliberate: a
/// cap set too low wedges a healthy source, while one set high only delays noticing a
/// source that is already broken.
pub const MAX_PARTIAL_RECORD_BYTES: usize = 64 * 1024;

/// What one fetch means for persisted state.
///
/// One variant per distinct write, the same rule
/// [`FetchOutcome`](crate::fermentrack::FetchOutcome) follows: a persisted-state change
/// is a variant, and anything that leaves state alone except `last_error` is an `Err`.
/// That is why restarting is a variant here rather than an error — an error would force
/// every caller to special-case one error as "reset and retry".
#[derive(Debug, Clone, PartialEq)]
pub enum SyncStep {
    /// Nothing to write but `synced_at`. The file did not grow, or it grew by less than
    /// one complete record. `last_error` is left alone: if it was true before this poll
    /// it is still true.
    Nothing,
    Advance(Advance),
    Restart(Restart),
}

/// Rows to write, and an offset to move to.
#[derive(Debug, Clone, PartialEq)]
pub struct Advance {
    /// `Fresh::offset + consumed`, never the length of what was fetched.
    ///
    /// **May equal the offset that was fetched.** A fault in the chunk's very first
    /// record leaves nothing to move past, but the fault still has to reach
    /// `last_error` — that is the difference between a source that wedges visibly and
    /// one that silently stops. Do not collapse this case into [`SyncStep::Nothing`].
    pub next_offset: u64,
    pub rows: Vec<LogRow>,
    /// `Some` only when this step read a header, i.e. when the fetch began at 0. `None`
    /// means "leave `sync_state.header` alone", not "clear it".
    pub header: Option<String>,
    /// `true` when `header` differs from the one already stored. Informational only: a
    /// header change can be *observed* solely at offset 0, and at offset 0 the offset is
    /// already where a change would put it.
    pub header_changed: bool,
    /// The newest `ts` in this chunk, maxed against `SyncState::last_ts` — the newest
    /// *seen*, not the last row in the buffer, because a clock step can put an older row
    /// after a newer one. `None` means "leave it alone".
    pub last_ts: Option<Timestamp>,
    pub report: ParseReport,
    /// Written to `last_error`. `None` **clears** it, so a resolved fault does not
    /// outlive the condition in the status footer.
    pub fault: Option<SyncFault>,
}

/// Start the file again: offset 0, no header, no `last_ts`.
///
/// Clearing `last_ts` is not cosmetic. A recreated file is a different brew under the
/// same name and its rows can predate everything we have; a stale `last_ts` would make
/// every one of them look like evidence of a bad offset.
#[derive(Debug, Clone, PartialEq)]
pub struct Restart {
    pub reason: RestartReason,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RestartReason {
    /// The remote file is shorter than our offset. Straight from [`FetchOutcome`].
    Truncated { total: Option<u64> },
    /// The resume chunk's first record is not a record, which proves `byte_offset` is
    /// not where we think it is.
    ///
    /// This is also how a changed header gets noticed without spending a request on a
    /// prefix probe. A log deleted and recreated is either shorter than our offset —
    /// `Truncated` — or at least as long, in which case the resume lands mid-record and
    /// arrives here. Both end at offset 0, where the header is read and compared.
    NotOnARecordBoundary,
    /// `byte_offset > 0` with no stored header: a torn write or a hand-edited database.
    /// There is nothing to parse the continuation against.
    NoStoredHeader,
}

/// Something worth `last_error` that is not worth refusing the rows that did parse.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncFault {
    /// `ParseReport::stopped_early`. The next poll refetches the faulty bytes and fails
    /// identically, so the source stops with an error in the footer instead of skipping
    /// rows nobody will notice are missing.
    StoppedEarly { at_offset: u64 },
    /// Every record in the chunk was skipped. Fermentrack's `column_headers` and
    /// `data_point` both read `gravity_enabled` at write time, so toggling it on a live
    /// `Beer` appends rows wider than the header the file was created with — 100 % row
    /// loss with no error anywhere else.
    EveryRecordSkipped { skipped: usize },
    /// See [`MAX_PARTIAL_RECORD_BYTES`].
    PartialRecordTooLong { bytes: usize },
}

impl std::fmt::Display for RestartReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { total: Some(total) } => {
                write!(f, "log truncated or recreated: it is now {total} bytes")
            }
            Self::Truncated { total: None } => write!(f, "log truncated or recreated"),
            Self::NotOnARecordBoundary => {
                write!(
                    f,
                    "resume did not begin on a record boundary; refetching whole"
                )
            }
            Self::NoStoredHeader => write!(f, "no stored header to resume against"),
        }
    }
}

impl std::fmt::Display for SyncFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StoppedEarly { at_offset } => {
                write!(
                    f,
                    "unreadable record at byte {at_offset}; sync stopped there"
                )
            }
            Self::EveryRecordSkipped { skipped } => write!(
                f,
                "all {skipped} records in this chunk were skipped; the header may no longer \
                 match the rows"
            ),
            Self::PartialRecordTooLong { bytes } => write!(
                f,
                "{bytes} bytes without a complete record; the log looks damaged"
            ),
        }
    }
}

/// Decide what one fetch means.
///
/// Errors here are file-level only, and all mean the URL points at the wrong thing: a
/// `_graph.csv`, a gravity log, or a header that is not a header. Everything that is
/// merely *damaged* comes back as a [`SyncFault`] on an [`Advance`].
pub fn ingest(state: &SyncState, outcome: FetchOutcome) -> Result<SyncStep> {
    let (bytes, offset) = match outcome {
        FetchOutcome::Unchanged { .. } => return Ok(SyncStep::Nothing),
        FetchOutcome::Truncated { total } => {
            return Ok(SyncStep::Restart(Restart {
                reason: RestartReason::Truncated { total },
            }));
        }
        FetchOutcome::Fresh { bytes, offset, .. } => (bytes, offset),
    };

    let (complete, _tail) = split_complete_lines(&bytes);
    if complete.is_empty() {
        return Ok(partial_only(offset, bytes.len()));
    }

    // At offset 0 the header is in the chunk; on a resume it is in `sync_state`. Lifting
    // it off here rather than calling `parse_full_log` means one parser covers both, so
    // `bytes_consumed` needs no per-caller adjustment. The two lengths stay distinct:
    // `header_bytes.len()` (terminator included) is arithmetic, `header_text`
    // (terminator stripped) is storage.
    let (header_bytes, body, header_text) = if offset == 0 {
        let (header_bytes, body) = split_first_record(complete).expect("a boundary exists");
        (
            header_bytes,
            body,
            header_line(&state.remote_path, header_bytes)?,
        )
    } else {
        match state.header.clone() {
            Some(header) => (&[][..], complete, header),
            None => {
                return Ok(SyncStep::Restart(Restart {
                    reason: RestartReason::NoStoredHeader,
                }));
            }
        }
    };

    let chunk = parse_with_header(&state.remote_path, &header_text, body)?;

    // Proof that the offset was where we thought. A mid-record resume usually fails on
    // cell count rather than on `log_time`, so the predicate is "the first record did
    // not become a row", for any reason at all. The cost of a false positive is one
    // unnecessary full refetch, after which the same row sits mid-chunk, is skipped
    // normally, and the source converges.
    if offset > 0 && !first_record_is_a_row(&state.remote_path, &header_text, body) {
        return Ok(SyncStep::Restart(Restart {
            reason: RestartReason::NotOnARecordBoundary,
        }));
    }

    let consumed = (header_bytes.len() + chunk.report.bytes_consumed) as u64;
    let next_offset = offset + consumed;

    let fault = if chunk.report.stopped_early {
        Some(SyncFault::StoppedEarly {
            at_offset: next_offset,
        })
    } else if chunk.report.rows_parsed == 0 && chunk.report.rows_skipped > 0 {
        Some(SyncFault::EveryRecordSkipped {
            skipped: chunk.report.rows_skipped,
        })
    } else {
        None
    };

    let header_changed = state
        .header
        .as_deref()
        .is_some_and(|stored| stored != header_text);
    let last_ts = chunk
        .rows
        .iter()
        .map(|row| row.ts)
        .chain(state.last_ts)
        .max();

    Ok(SyncStep::Advance(Advance {
        next_offset,
        rows: chunk.rows,
        header: (offset == 0).then_some(header_text),
        header_changed,
        last_ts,
        report: chunk.report,
        fault,
    }))
}

/// A chunk holding no complete record at all. Normally Fermentrack caught mid-`writerow`
/// and the only honest answer is to wait; past the cap it is damage, and saying so is
/// what keeps the source from stalling in silence.
fn partial_only(offset: u64, len: usize) -> SyncStep {
    if len <= MAX_PARTIAL_RECORD_BYTES {
        return SyncStep::Nothing;
    }
    SyncStep::Advance(Advance {
        next_offset: offset,
        rows: Vec::new(),
        header: None,
        header_changed: false,
        last_ts: None,
        report: ParseReport::default(),
        fault: Some(SyncFault::PartialRecordTooLong { bytes: len }),
    })
}

/// The header line as stored: valid UTF-8, terminator stripped.
fn header_line(name: &str, header_bytes: &[u8]) -> Result<String> {
    match std::str::from_utf8(header_bytes) {
        Ok(text) => Ok(text.trim_end_matches(['\r', '\n']).to_string()),
        // A `_full.csv` header is ASCII column names. Anything else is not one, and the
        // preview is what tells the user which file they actually pointed at.
        Err(_) => Err(Error::NotAFermentrackLog {
            detail: format!("{name}: {}", preview_bytes(header_bytes)),
        }),
    }
}

fn first_record_is_a_row(name: &str, header: &str, body: &[u8]) -> bool {
    let Some((first, _)) = split_first_record(body) else {
        return false;
    };
    parse_with_header(name, header, first).is_ok_and(|chunk| chunk.report.rows_parsed == 1)
}

impl SyncStep {
    /// Fold this step into the state Phase 2e is about to write.
    ///
    /// Pure, and the clock is an argument, so a test can compare whole [`SyncState`]s by
    /// value.
    pub fn apply_to(&self, state: &mut SyncState, at: Timestamp) {
        state.synced_at = at;
        match self {
            Self::Nothing => {}
            Self::Advance(advance) => {
                state.byte_offset = advance.next_offset;
                if let Some(header) = &advance.header {
                    state.header = Some(header.clone());
                }
                if advance.last_ts.is_some() {
                    state.last_ts = advance.last_ts;
                }
                state.last_error = advance.fault.as_ref().map(ToString::to_string);
            }
            Self::Restart(restart) => {
                state.byte_offset = 0;
                state.header = None;
                state.last_ts = None;
                state.last_error = Some(restart.reason.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "log_time,beer_temp,beer_set,beer_ann,fridge_temp,fridge_set,\
                          fridge_ann,room_temp,state,temp_format,associated_beer_id";

    fn row(minute: u32) -> String {
        format!("2025/09/18 00:{minute:02}:00Z,18.06,18.0,,17.4,17.5,,21.0,0,C,17")
    }

    /// Header plus `n` rows, CRLF throughout, exactly as Fermentrack writes it.
    fn log(n: u32) -> Vec<u8> {
        let mut out = format!("{HEADER}\r\n");
        for minute in 0..n {
            out.push_str(&row(minute));
            out.push_str("\r\n");
        }
        out.into_bytes()
    }

    fn state() -> SyncState {
        SyncState {
            source_id: 1,
            remote_path: "/data/Device 1 - B17 - Seltzer 17_full.csv".to_string(),
            byte_offset: 0,
            header: None,
            last_ts: None,
            last_error: None,
            synced_at: Timestamp::from_secs(0),
        }
    }

    fn fresh(bytes: &[u8], offset: u64) -> FetchOutcome {
        FetchOutcome::Fresh {
            bytes: bytes.to_vec(),
            offset,
            total: Some(offset + bytes.len() as u64),
        }
    }

    fn advanced(state: &SyncState, outcome: FetchOutcome) -> Advance {
        match ingest(state, outcome).expect("should ingest") {
            SyncStep::Advance(advance) => advance,
            other => panic!("expected an advance, got {other:?}"),
        }
    }

    fn restart(state: &SyncState, outcome: FetchOutcome) -> RestartReason {
        match ingest(state, outcome).expect("should ingest") {
            SyncStep::Restart(restart) => restart.reason,
            other => panic!("expected a restart, got {other:?}"),
        }
    }

    // -- the no-op paths ----------------------------------------------------

    #[test]
    fn an_unchanged_poll_leaves_every_stored_field_alone() {
        let mut before = state();
        before.byte_offset = 400;
        before.header = Some(HEADER.to_string());
        before.last_ts = Some(Timestamp::from_secs(1_000));
        before.last_error = Some("something was already wrong".to_string());

        let step = ingest(&before, FetchOutcome::Unchanged { total: 400 }).unwrap();
        assert_eq!(step, SyncStep::Nothing);

        let mut after = before.clone();
        step.apply_to(&mut after, Timestamp::from_secs(9_999));

        // Only the clock moved. In particular a poll that did nothing does not get to
        // declare the previous failure resolved.
        assert_eq!(after.synced_at, Timestamp::from_secs(9_999));
        assert_eq!(after.byte_offset, before.byte_offset);
        assert_eq!(after.header, before.header);
        assert_eq!(after.last_ts, before.last_ts);
        assert_eq!(after.last_error, before.last_error);
    }

    #[test]
    fn a_truncated_log_restarts_from_zero_and_forgets_the_header_and_the_last_timestamp() {
        let mut before = state();
        before.byte_offset = 5_000;
        before.header = Some(HEADER.to_string());
        before.last_ts = Some(Timestamp::from_secs(1_000));

        let step = ingest(&before, FetchOutcome::Truncated { total: Some(120) }).unwrap();
        assert_eq!(
            step,
            SyncStep::Restart(Restart {
                reason: RestartReason::Truncated { total: Some(120) },
            })
        );

        step.apply_to(&mut before, Timestamp::from_secs(9_999));
        assert_eq!(before.byte_offset, 0);
        assert_eq!(before.header, None);
        assert_eq!(before.last_ts, None);
        assert!(before.last_error.is_some());
    }

    // -- offsets ------------------------------------------------------------

    /// The restart path fetches from 0 while the stored offset is still whatever it was.
    /// An arithmetic base taken from state would claim a range that was never parsed.
    #[test]
    fn the_offset_comes_from_the_fetch_not_from_the_stored_state() {
        let mut stale = state();
        stale.byte_offset = 9_999;
        stale.header = Some(HEADER.to_string());

        let body = log(3);
        let advance = advanced(&stale, fresh(&body, 0));
        assert_eq!(advance.next_offset, body.len() as u64);
        assert_eq!(advance.rows.len(), 3);
    }

    #[test]
    fn a_half_written_last_row_is_left_for_the_next_poll() {
        let mut body = log(3);
        body.extend_from_slice(b"2025/09/18 00:03:00Z,18.2,18.");

        let advance = advanced(&state(), fresh(&body, 0));
        assert_eq!(advance.rows.len(), 3);
        assert_eq!(advance.next_offset, log(3).len() as u64);
        // `18.` parses as 18.0 — the value that must never reach the database.
        assert!(!advance.rows.iter().any(|row| row.beer_set
            == Some(crate::units::Celsius::new(18.0).unwrap())
            && row.beer_temp == Some(crate::units::Celsius::new(18.2).unwrap())));
    }

    /// The one 2d exists for. Cut after `,0,C,1` and the fragment has the right cell
    /// count *and* a valid `log_time`, so without the split it commits as a real reading
    /// with `associated_beer_id = 1`, and the genuine row's `7\r\n` arrives next poll as
    /// a one-cell record and is skipped. One wrong reading written, one real one lost,
    /// and nothing downstream could ever tell.
    #[test]
    fn a_half_written_row_that_still_has_the_right_cell_count_is_not_committed() {
        let mut body = log(3);
        body.extend_from_slice(b"2025/09/18 00:03:00Z,18.06,18.0,,17.4,17.5,,21.0,0,C,1");

        let advance = advanced(&state(), fresh(&body, 0));
        assert_eq!(advance.rows.len(), 3, "{:?}", advance.report);
        assert_eq!(advance.next_offset, log(3).len() as u64);
        assert!(
            !advance
                .rows
                .iter()
                .any(|row| row.associated_beer_id == Some(1))
        );
    }

    #[test]
    fn a_chunk_that_is_entirely_one_partial_record_writes_nothing() {
        let mut before = state();
        before.byte_offset = 400;
        before.header = Some(HEADER.to_string());

        let step = ingest(&before, fresh(b"2025/09/18 00:03:00Z,18.0", 400)).unwrap();
        assert_eq!(step, SyncStep::Nothing);
    }

    #[test]
    fn a_partial_record_larger_than_the_cap_stops_the_source_visibly() {
        let mut before = state();
        before.byte_offset = 400;
        before.header = Some(HEADER.to_string());

        let bytes = vec![b'x'; MAX_PARTIAL_RECORD_BYTES + 1];
        let advance = advanced(&before, fresh(&bytes, 400));
        assert_eq!(
            advance.next_offset, 400,
            "a fault must not advance past itself"
        );
        assert!(advance.rows.is_empty());
        assert_eq!(
            advance.fault,
            Some(SyncFault::PartialRecordTooLong {
                bytes: MAX_PARTIAL_RECORD_BYTES + 1
            })
        );
    }

    // -- headers ------------------------------------------------------------

    /// Fermentrack opens the file with `'w'`, writes the header, then appends. A first
    /// fetch landing inside that write sees a header fragment — and a fragment cut after
    /// `room_temp` has `log_time`, `beer_temp` and `beer_set` but no `temp_format`, which
    /// `Columns::from_header` classifies as a `_graph.csv`. Telling the user to go find a
    /// `_full.csv` they are already pointed at, over a condition that resolves itself in
    /// milliseconds, is the wrong answer.
    #[test]
    fn a_first_fetch_that_lands_mid_header_is_not_mistaken_for_a_graph_log() {
        let cut = HEADER.find(",room_temp").unwrap();
        let step = ingest(&state(), fresh(&HEADER.as_bytes()[..cut], 0)).unwrap();
        assert_eq!(step, SyncStep::Nothing);
    }

    #[test]
    fn the_stored_header_is_the_header_line_without_its_terminator() {
        let advance = advanced(&state(), fresh(&log(2), 0));
        assert_eq!(advance.header.as_deref(), Some(HEADER));
        assert!(!advance.header_changed);

        // Already stored and unchanged: still not a change.
        let mut known = state();
        known.header = Some(HEADER.to_string());
        assert!(!advanced(&known, fresh(&log(2), 0)).header_changed);

        // A different column set is.
        let mut other = state();
        other.header = Some(format!("{HEADER},gravity,grav_temp"));
        assert!(advanced(&other, fresh(&log(2), 0)).header_changed);
    }

    #[test]
    fn a_file_that_is_only_a_header_advances_past_it_and_writes_no_rows() {
        let body = format!("{HEADER}\r\n").into_bytes();
        let advance = advanced(&state(), fresh(&body, 0));
        assert_eq!(advance.next_offset, body.len() as u64);
        assert!(advance.rows.is_empty());
        assert_eq!(advance.header.as_deref(), Some(HEADER));
        assert_eq!(advance.fault, None);
    }

    // -- resumes ------------------------------------------------------------

    #[test]
    fn a_resume_that_does_not_begin_on_a_record_boundary_restarts_from_zero() {
        let full = log(4);
        // Three bytes into a row: what a stored offset that was one record short looks
        // like from the server's side.
        let offset = log(2).len() as u64 + 3;

        let mut before = state();
        before.byte_offset = offset;
        before.header = Some(HEADER.to_string());

        let reason = restart(&before, fresh(&full[offset as usize..], offset));
        assert_eq!(reason, RestartReason::NotOnARecordBoundary);
    }

    #[test]
    fn a_resume_with_no_stored_header_restarts_from_zero() {
        let mut before = state();
        before.byte_offset = 400;
        before.header = None;

        let reason = restart(&before, fresh(&log(2), 400));
        assert_eq!(reason, RestartReason::NoStoredHeader);
    }

    #[test]
    fn a_resume_continues_the_numbering_from_the_offset_it_was_given() {
        let first = log(2);
        let full = log(4);
        let offset = first.len() as u64;

        let mut before = state();
        before.byte_offset = offset;
        before.header = Some(HEADER.to_string());

        let advance = advanced(&before, fresh(&full[offset as usize..], offset));
        assert_eq!(advance.rows.len(), 2);
        assert_eq!(advance.next_offset, full.len() as u64);
        // A resume read no header, so it must not touch the stored one.
        assert_eq!(advance.header, None);
    }

    // -- faults -------------------------------------------------------------

    #[test]
    fn the_offset_advances_by_what_parsed_and_not_by_what_was_fetched() {
        let good = log(1);
        let mut body = good.clone();
        // An invalid UTF-8 byte mid-record: a `csv::Error`, not a skipped row.
        body.extend_from_slice(b"2025/09/18 00:01:00Z,\xff\xfe,18.0,,17.4,17.5,,21.0,0,C,17\r\n");
        body.extend_from_slice(row(2).as_bytes());
        body.extend_from_slice(b"\r\n");

        let advance = advanced(&state(), fresh(&body, 0));
        assert!(advance.report.stopped_early);
        assert_eq!(advance.next_offset, good.len() as u64);
        assert_eq!(
            advance.fault,
            Some(SyncFault::StoppedEarly {
                at_offset: good.len() as u64
            })
        );
    }

    #[test]
    fn a_fault_in_the_first_row_of_a_new_log_advances_only_past_the_header() {
        let header_len = format!("{HEADER}\r\n").len() as u64;
        let mut body = format!("{HEADER}\r\n").into_bytes();
        body.extend_from_slice(b"2025/09/18 00:00:00Z,\xff\xfe,18.0,,17.4,17.5,,21.0,0,C,17\r\n");

        let advance = advanced(&state(), fresh(&body, 0));
        assert_eq!(advance.next_offset, header_len);
        assert!(advance.rows.is_empty());
        assert!(matches!(
            advance.fault,
            Some(SyncFault::StoppedEarly { .. })
        ));
    }

    /// Toggling `gravity_enabled` on a live `Beer` widens the rows without rewriting the
    /// header. Every row then fails on cell count, and `rows_skipped` climbing is
    /// invisible everywhere else.
    #[test]
    fn a_chunk_whose_rows_are_all_skipped_is_reported_rather_than_silently_ignored() {
        let mut body = format!("{HEADER}\r\n").into_bytes();
        for minute in 0..3 {
            body.extend_from_slice(format!("{},1.0374,18.0\r\n", row(minute)).as_bytes());
        }

        let advance = advanced(&state(), fresh(&body, 0));
        assert!(advance.rows.is_empty());
        assert_eq!(
            advance.fault,
            Some(SyncFault::EveryRecordSkipped { skipped: 3 })
        );
        // The offset still advances: refetching rows we have already decided we cannot
        // read would wedge without adding information.
        assert_eq!(advance.next_offset, body.len() as u64);
    }

    #[test]
    fn a_clean_advance_clears_a_stale_last_error() {
        let mut before = state();
        before.last_error = Some("yesterday's problem".to_string());

        let step = ingest(&before, fresh(&log(2), 0)).unwrap();
        step.apply_to(&mut before, Timestamp::now());
        assert_eq!(before.last_error, None);
    }

    // -- timestamps ---------------------------------------------------------

    #[test]
    fn the_newest_timestamp_is_the_newest_seen_not_the_last_row_in_the_buffer() {
        let mut body = format!("{HEADER}\r\n").into_bytes();
        body.extend_from_slice(row(5).as_bytes());
        body.extend_from_slice(b"\r\n");
        // A clock step back: an older row written after a newer one.
        body.extend_from_slice(row(1).as_bytes());
        body.extend_from_slice(b"\r\n");

        let advance = advanced(&state(), fresh(&body, 0));
        assert_eq!(advance.last_ts, advance.rows.iter().map(|r| r.ts).max());

        // A chunk entirely older than what we have does not lower it.
        let mut ahead = state();
        ahead.last_ts = Some(Timestamp::from_secs(i64::MAX));
        assert_eq!(
            advanced(&ahead, fresh(&body, 0)).last_ts,
            Some(Timestamp::from_secs(i64::MAX))
        );
    }

    // -- the whole contract at once -----------------------------------------

    /// Feed the same log split at every possible byte. Every split, boundary and
    /// arithmetic bug in the module fails here.
    #[test]
    fn ingesting_a_log_split_at_every_byte_yields_the_same_rows_as_one_pass() {
        let full = log(6);
        let one_pass = advanced(&state(), fresh(&full, 0));

        for cut in 0..full.len() {
            let mut live = state();
            let mut rows = Vec::new();

            // Poll once while only `cut` bytes have been written, then again once the
            // file is whole.
            for visible in [cut, full.len()] {
                let offset = live.byte_offset as usize;
                if offset >= visible {
                    continue;
                }
                let step = ingest(&live, fresh(&full[offset..visible], offset as u64))
                    .unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
                if let SyncStep::Advance(a) = &step {
                    rows.extend(a.rows.iter().cloned());
                }
                step.apply_to(&mut live, Timestamp::from_secs(0));
            }

            assert_eq!(live.byte_offset, full.len() as u64, "cut at {cut}");
            assert_eq!(rows, one_pass.rows, "cut at {cut}");
            assert_eq!(live.last_error, None, "cut at {cut}");
        }
    }

    #[test]
    fn ingesting_the_same_chunk_twice_gives_the_same_step() {
        let before = state();
        let body = log(3);
        assert_eq!(
            ingest(&before, fresh(&body, 0)).unwrap(),
            ingest(&before, fresh(&body, 0)).unwrap()
        );
    }
}
