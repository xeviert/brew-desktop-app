//! Phase 2d, hoisted in Va. Where a byte offset moves to, and what a half-written line
//! costs.
//!
//! Pure: bytes and a stored offset in, a decision out. No HTTP, no database, no clock —
//! [`ingest`] is a function of its arguments and nothing else, which is why every rule
//! below is tested against a `Vec<u8>` with no server and no `Db`.
//! [`commit`](super::commit) turns one [`SyncStep`] into one transaction.
//!
//! There is no remainder buffer, and its absence is the design. `byte_offset` only ever
//! advances past a complete, terminated record, so the next `Range: bytes=<offset>-`
//! re-requests the partial tail for free — at most one record, correct across a process
//! restart, and with no schema column to hold it. What that costs is a rule with no
//! exceptions: **split before you parse**, spelled out on
//! [`split_complete_lines`].
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
    db::{Metric, tier1::AnnotationChannel},
    journal::{FetchOutcome, preview_bytes, split_complete_lines, split_first_record},
    units::Timestamp,
};

/// What the bytes of one log format mean. The only thing a new source of journals
/// has to write; everything else in this module is shared.
///
/// Every log this module reads has a header line, and every record after it is
/// parsed against that header — at byte 0 it is lifted off the chunk, on a resume it
/// comes from `sync_state.header`.
pub trait Dialect {
    type Row;

    /// Parse complete records against a header. `body` holds no header line and ends
    /// on a record boundary. `name` is the remote path, for error messages only.
    ///
    /// File-level problems are `Err`; a bad record is skipped and counted in the
    /// [`ParseReport`], and a reader error sets `stopped_early` rather than failing.
    fn parse(name: &str, header: &str, body: &[u8]) -> Result<ParsedChunk<Self::Row>>;

    fn ts(row: &Self::Row) -> Timestamp;

    /// `(source column, metric, value)` for every value the row carries. Absent
    /// values are skipped here, not yielded as `None`, so a stream is only ever created
    /// for a metric that has data.
    fn readings(row: &Self::Row) -> impl Iterator<Item = (&str, Metric, f64)>;

    /// Free-text notes the row carries, stored as `annotation` rows.
    fn annotations(row: &Self::Row) -> impl Iterator<Item = (AnnotationChannel, &str)>;

    /// The error for a header that is not text at all: the URL points at the wrong
    /// thing. Each dialect keeps its own error code.
    fn not_a_log(detail: String) -> Error;
}

/// What a chunk cost to parse. Phase 2g renders this; a sync that is quietly dropping
/// half its values should be visible without reading the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ParseReport {
    pub rows_parsed: usize,
    /// Rows with the wrong cell count, or an unusable timestamp or unit.
    pub rows_skipped: usize,
    /// Cells that were present but unreadable or implausible. Not counted for a
    /// genuinely absent sensor — that is data, not damage.
    pub values_dropped: usize,
    pub unknown_columns: Vec<String>,
    /// Bytes of the input taken up by records that were *read* — parsed or
    /// deliberately skipped. Relative to the buffer that was passed in, so a parse
    /// that included the header line counts it and [`Dialect::parse`] does not.
    /// [`ingest`] advances `byte_offset` by exactly this and never by the length of
    /// what it fetched.
    pub bytes_consumed: usize,
    /// A reader error ended the read before the end of the input. The bytes from
    /// `bytes_consumed` onwards were never looked at, so `ingest` must not claim them:
    /// advancing past a fault it did not parse is a permanent hole in `reading`.
    pub stopped_early: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedChunk<R> {
    pub rows: Vec<R>,
    pub report: ParseReport,
}

/// The `sync_state` row.
///
/// [`ingest`] reads only `remote_path` (for error messages), `byte_offset` (advisory —
/// see the module docs) and `header`. The rest is carried so that
/// [`commit`](super::commit) can load and store the row whole.
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

/// An incomplete record larger than this is damage, not a writer mid-record.
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
/// One variant per distinct write, the same rule [`FetchOutcome`] follows: a
/// persisted-state change is a variant, and anything that leaves state alone except
/// `last_error` is an `Err`. That is why restarting is a variant here rather than an
/// error — an error would force every caller to special-case one error as "reset and
/// retry".
#[derive(Debug, Clone, PartialEq)]
pub enum SyncStep<R> {
    /// Nothing to write but `synced_at`. The file did not grow, or it grew by less than
    /// one complete record. `last_error` is left alone: if it was true before this poll
    /// it is still true.
    Nothing,
    Advance(Advance<R>),
    Restart(Restart),
}

/// Rows to write, and an offset to move to.
#[derive(Debug, Clone, PartialEq)]
pub struct Advance<R> {
    /// `Fresh::offset + consumed`, never the length of what was fetched.
    ///
    /// **May equal the offset that was fetched.** A fault in the chunk's very first
    /// record leaves nothing to move past, but the fault still has to reach
    /// `last_error` — that is the difference between a source that wedges visibly and
    /// one that silently stops. Do not collapse this case into [`SyncStep::Nothing`].
    pub next_offset: u64,
    pub rows: Vec<R>,
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
    /// Phase 2g's "Rebuild from scratch". The only reason here that is not a fault, and
    /// the only one [`SyncStep::apply_to`] must *clear* `last_error` for: a button the
    /// user pressed is not a condition to report, and 2g's scheduler backs off any
    /// source whose `last_error` is set.
    Requested,
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
            Self::Requested => write!(f, "rebuilding from the beginning"),
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
/// Errors here are file-level only, and all mean the URL points at the wrong thing.
/// Everything that is merely *damaged* comes back as a [`SyncFault`] on an [`Advance`].
pub fn ingest<D: Dialect>(state: &SyncState, outcome: FetchOutcome) -> Result<SyncStep<D::Row>> {
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
    // it off here means one parser covers both, so `bytes_consumed` needs no per-caller
    // adjustment. The two lengths stay distinct: `header_bytes.len()` (terminator
    // included) is arithmetic, `header_text` (terminator stripped) is storage.
    let (header_bytes, body, header_text) = if offset == 0 {
        let (header_bytes, body) = split_first_record(complete).expect("a boundary exists");
        (
            header_bytes,
            body,
            header_line::<D>(&state.remote_path, header_bytes)?,
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

    let chunk = D::parse(&state.remote_path, &header_text, body)?;

    // Proof that the offset was where we thought. A mid-record resume usually fails on
    // cell count rather than on the timestamp, so the predicate is "the first record did
    // not become a row", for any reason at all. The cost of a false positive is one
    // unnecessary full refetch, after which the same row sits mid-chunk, is skipped
    // normally, and the source converges.
    if offset > 0 && !first_record_is_a_row::<D>(&state.remote_path, &header_text, body) {
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
    let last_ts = chunk.rows.iter().map(D::ts).chain(state.last_ts).max();

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

/// A chunk holding no complete record at all. Normally the writer caught mid-record
/// and the only honest answer is to wait; past the cap it is damage, and saying so is
/// what keeps the source from stalling in silence.
fn partial_only<R>(offset: u64, len: usize) -> SyncStep<R> {
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
fn header_line<D: Dialect>(name: &str, header_bytes: &[u8]) -> Result<String> {
    match std::str::from_utf8(header_bytes) {
        Ok(text) => Ok(text.trim_end_matches(['\r', '\n']).to_string()),
        // A header is column names. Anything else is not one, and the preview is what
        // tells the user which file they actually pointed at.
        Err(_) => Err(D::not_a_log(format!(
            "{name}: {}",
            preview_bytes(header_bytes)
        ))),
    }
}

fn first_record_is_a_row<D: Dialect>(name: &str, header: &str, body: &[u8]) -> bool {
    let Some((first, _)) = split_first_record(body) else {
        return false;
    };
    D::parse(name, header, first).is_ok_and(|chunk| chunk.report.rows_parsed == 1)
}

impl<R> SyncStep<R> {
    /// Fold this step into the state [`commit`](super::commit) is about to write.
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
                // A requested rebuild clears the error instead of writing one. It is the
                // escape hatch *from* a fault, so leaving the old text in place would
                // keep 2g's scheduler backing the source off for a condition the user
                // has just resolved.
                state.last_error = match restart.reason {
                    RestartReason::Requested => None,
                    ref reason => Some(reason.to_string()),
                };
            }
        }
    }
}

/// The `_full.csv` tests in `fermentrack::sync` cover every rule above against real
/// Fermentrack bytes. These are the proof that none of it needs Fermentrack: a
/// two-column dialect with nothing in common but a header line.
#[cfg(test)]
mod tests {
    use super::*;

    /// `ts,temp_c`, one integer-second timestamp and one number per line.
    struct Toy;

    impl Dialect for Toy {
        type Row = (Timestamp, f64);

        fn parse(_name: &str, header: &str, body: &[u8]) -> Result<ParsedChunk<Self::Row>> {
            if header != "ts,temp_c" {
                return Err(Error::NotAFermentrackLog {
                    detail: header.to_owned(),
                });
            }
            let mut chunk = ParsedChunk {
                rows: Vec::new(),
                report: ParseReport::default(),
            };
            let (mut rest, mut at) = (body, 0);
            while let Some((line, tail)) = split_first_record(rest) {
                let text = std::str::from_utf8(line).unwrap_or("").trim_end();
                let row = text.split_once(',').and_then(|(ts, value)| {
                    Some((Timestamp::from_secs(ts.parse().ok()?), value.parse().ok()?))
                });
                match row {
                    Some(row) => {
                        chunk.rows.push(row);
                        chunk.report.rows_parsed += 1;
                    }
                    None => chunk.report.rows_skipped += 1,
                }
                at += line.len();
                chunk.report.bytes_consumed = at;
                rest = tail;
            }
            Ok(chunk)
        }

        fn ts(row: &Self::Row) -> Timestamp {
            row.0
        }

        fn readings(row: &Self::Row) -> impl Iterator<Item = (&str, Metric, f64)> {
            std::iter::once(("temp_c", Metric::TempC, row.1))
        }

        fn annotations(_: &Self::Row) -> impl Iterator<Item = (AnnotationChannel, &str)> {
            std::iter::empty()
        }

        fn not_a_log(detail: String) -> Error {
            Error::NotAFermentrackLog { detail }
        }
    }

    const LOG: &[u8] = b"ts,temp_c\n100,18.0\n160,18.5\n220,19.0\n";

    fn state() -> SyncState {
        SyncState {
            source_id: 1,
            remote_path: "journals/esp32-1.log".to_owned(),
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

    #[test]
    fn another_dialect_splits_resumes_and_restarts_the_same_way() {
        let mut live = state();
        let mut rows = Vec::new();

        // Once with the last record half-written, then again once it is whole.
        for visible in [LOG.len() - 4, LOG.len()] {
            let from = live.byte_offset as usize;
            let step = ingest::<Toy>(&live, fresh(&LOG[from..visible], from as u64)).unwrap();
            if let SyncStep::Advance(advance) = &step {
                rows.extend(advance.rows.iter().copied());
            }
            step.apply_to(&mut live, Timestamp::from_secs(0));
        }

        assert_eq!(live.byte_offset, LOG.len() as u64);
        assert_eq!(live.header.as_deref(), Some("ts,temp_c"));
        assert_eq!(live.last_ts, Some(Timestamp::from_secs(220)));
        assert_eq!(rows.len(), 3);

        // Three bytes into the first record: `,18.0` is not a row.
        let mut mid = live.clone();
        mid.byte_offset = 13;
        let step = ingest::<Toy>(&mid, fresh(&LOG[13..], 13)).unwrap();
        assert_eq!(
            step,
            SyncStep::Restart(Restart {
                reason: RestartReason::NotOnARecordBoundary
            })
        );
    }
}
