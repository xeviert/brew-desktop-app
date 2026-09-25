//! Phase 2d's `_full.csv` side. The byte-offset machinery moved to
//! [`crate::journal::ingest`](mod@crate::journal::ingest) in Va; what is left is what Fermentrack's bytes mean,
//! as a [`Dialect`], and the Phase 2 names kept as aliases so no caller changed.
//!
//! The tests below stayed: they drive the shared machinery with real `_full.csv`
//! bytes, and passing unchanged is the proof the hoist changed nothing.

use crate::{
    Error, Result,
    db::{Metric, tier1::AnnotationChannel},
    fermentrack::{LogRow, ParsedChunk, parse_with_header},
    journal::{self, Dialect},
    units::Timestamp,
};

pub use crate::journal::{
    FetchOutcome, MAX_PARTIAL_RECORD_BYTES, Restart, RestartReason, SyncFault, SyncState,
};

pub type SyncStep = journal::SyncStep<LogRow>;
pub type Advance = journal::Advance<LogRow>;

/// Fermentrack's `_full.csv`.
pub struct FullCsv;

impl Dialect for FullCsv {
    type Row = LogRow;

    fn parse(name: &str, header: &str, body: &[u8]) -> Result<ParsedChunk> {
        parse_with_header(name, header, body)
    }

    fn ts(row: &LogRow) -> Timestamp {
        row.ts
    }

    fn readings(row: &LogRow) -> impl Iterator<Item = (&str, Metric, f64)> {
        // `Item` is invariant, so the `'static` column names are narrowed explicitly.
        row.readings()
            .map(|(column, metric, value)| (column as &str, metric, value))
    }

    /// Free text a human typed into Fermentrack. `beer_ann` is about the liquid and
    /// `fridge_ann` about the chamber around it.
    fn annotations(row: &LogRow) -> impl Iterator<Item = (AnnotationChannel, &str)> {
        let cells: [(AnnotationChannel, Option<&str>); 2] = [
            (AnnotationChannel::Vessel, row.beer_ann.as_deref()),
            (AnnotationChannel::Chamber, row.fridge_ann.as_deref()),
        ];
        cells
            .into_iter()
            .filter_map(|(channel, text)| text.map(|text| (channel, text)))
    }

    /// A `_full.csv` header is ASCII column names; anything else means the URL points
    /// at a `_graph.csv`, a gravity log, or not a log at all.
    fn not_a_log(detail: String) -> Error {
        Error::NotAFermentrackLog { detail }
    }
}

/// [`journal::ingest()`] for a `_full.csv`.
pub fn ingest(state: &SyncState, outcome: FetchOutcome) -> Result<SyncStep> {
    journal::ingest::<FullCsv>(state, outcome)
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

    /// 2g's Rebuild goes through the same machinery as a truncation, and must come out
    /// the other side looking healthy: same offset reset, same cleared header, but no
    /// `last_error` — the scheduler backs off anything that has one.
    #[test]
    fn a_requested_rebuild_resets_the_offset_without_recording_an_error() {
        let mut before = state();
        before.byte_offset = 5_000;
        before.header = Some(HEADER.to_string());
        before.last_ts = Some(Timestamp::from_secs(1_000));
        before.last_error = Some("the fault being rebuilt away".to_string());

        SyncStep::Restart(Restart {
            reason: RestartReason::Requested,
        })
        .apply_to(&mut before, Timestamp::from_secs(9_999));

        assert_eq!(before.byte_offset, 0);
        assert_eq!(before.header, None);
        assert_eq!(before.last_ts, None);
        assert_eq!(before.last_error, None);
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
