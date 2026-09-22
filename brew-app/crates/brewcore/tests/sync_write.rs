//! Phase 2e end to end: a real socket, a real SQLite file, a log that grows.
//!
//! `fermentrack_sync.rs` is the same loop with `SyncState` in a local variable and no
//! database — it proves the offset arithmetic. This file proves the other half: that
//! what 2d decided actually lands, lands once, and survives the process going away.
//!
//! The growing-file test below is the one docs/BUILDOUT.md calls "the single most
//! valuable test in the project". It was deferred from 2a for exactly one reason —
//! there was no write path to assert against.

use brewcore::{
    db::{Db, Metric, SourceKind, tier1},
    fermentrack::{Client, ClientSettings, SyncState, SyncStep, ingest, log_url, store},
    units::Timestamp,
};
use brewfixture::{FixtureServer, Kind, LogSpec, RangeMode, Scenario};

/// A source, its `sync_state` row, and a temp dir that keeps the file alive.
struct Rig {
    db: Db,
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    source_id: i64,
}

impl Rig {
    fn new(remote_path: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("brewery.db");
        let db = Db::open(&path).expect("open the database");

        let source_id = db
            .write(|conn| {
                tier1::create_source(conn, SourceKind::FermentrackBeerLog, "Fermenter 1", None)
            })
            .expect("create the source");
        store::upsert_sync_state(&db, source_id, remote_path).expect("create the sync_state row");

        Self {
            db,
            _dir: dir,
            path,
            source_id,
        }
    }

    fn state(&self) -> SyncState {
        store::load_sync_state(&self.db, self.source_id)
            .expect("read the sync_state row")
            .expect("the row exists")
    }

    /// Close and reopen the file. Everything the next poll needs must come back off
    /// disk; nothing may live only in memory.
    fn reopen(&mut self) {
        self.db = Db::open(&self.path).expect("reopen the database");
    }

    fn readings(&self) -> Vec<(Metric, Timestamp, f64)> {
        self.db
            .read(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT s.metric, r.ts, r.value FROM reading r \
                     JOIN stream s ON s.id = r.stream_id \
                     WHERE s.source_id = ? ORDER BY s.metric, r.ts",
                )?;
                let rows = stmt.query_map([self.source_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .expect("read the readings back")
    }

    fn external_ids(&self) -> Vec<(String, Metric)> {
        self.db
            .read(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT external_id, metric FROM stream WHERE source_id = ? ORDER BY metric",
                )?;
                let rows =
                    stmt.query_map([self.source_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .expect("read the streams back")
    }
}

fn client() -> Client {
    Client::with_settings(ClientSettings::default()).expect("build the client")
}

fn spec(rows: usize) -> LogSpec {
    LogSpec {
        rows,
        ..Scenario::Active.spec()
    }
}

/// One poll, whole: fetch from the stored offset, decide, write. Phase 2g's scheduler
/// is this plus a timer.
async fn poll(rig: &mut Rig, client: &Client, url: &str) -> SyncStep {
    let mut state = rig.state();
    let outcome = client
        .fetch(url, state.byte_offset)
        .await
        .expect("the fixture server always answers");
    let step = ingest(&state, outcome).expect("the fixture is a real _full.csv");
    store::commit(&rig.db, &mut state, &step, Timestamp::now()).expect("commit the step");
    step
}

// ---------------------------------------------------------------------------
// The growing file
// ---------------------------------------------------------------------------

/// Serve N bytes, sync, serve N+M, sync again. No duplicates, no gaps.
///
/// Returns the readings so the caller can compare one `RangeMode` against the other.
async fn growing_file(mode: RangeMode) -> Vec<(Metric, Timestamp, f64)> {
    let spec = spec(40);
    let body = spec.full_csv();
    let remote_path = spec.remote_path(Kind::Full);

    // Two thirds of the file, cut to a record boundary so the first poll is a clean
    // resume rather than also exercising the partial-line path (that is the next test).
    let first_len = body[..body.len() * 2 / 3]
        .rfind("\r\n")
        .map(|at| at + 2)
        .expect("a boundary");

    let server = FixtureServer::start(&remote_path, &body[..first_len]).await;
    server.set_mode(mode);
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let mut rig = Rig::new(&remote_path);
    let client = client();

    poll(&mut rig, &client, &url).await;
    let after_first = rig.readings();
    assert!(!after_first.is_empty(), "the first poll wrote nothing");
    assert_eq!(rig.state().byte_offset, first_len as u64);

    server.append(&body[first_len..]);
    poll(&mut rig, &client, &url).await;

    let all = rig.readings();
    assert_eq!(
        rig.state().byte_offset,
        body.len() as u64,
        "the whole file should have been consumed"
    );

    // No duplicates. The primary key would have rejected a second write outright, so
    // what this really asserts is that nothing was dropped to avoid one.
    let mut keys: Vec<_> = all.iter().map(|(m, ts, _)| (*m, *ts)).collect();
    let before_dedup = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        before_dedup,
        "a (metric, ts) pair appears twice"
    );

    // No gaps: one reading per row per metric the file carries, and the first poll's
    // rows are still there unchanged.
    let rows = spec.generate().len();
    for metric in [Metric::TempC, Metric::SetpointC] {
        let count = all.iter().filter(|(m, _, _)| *m == metric).count();
        assert_eq!(count, rows, "{metric} is missing rows");
    }
    for earlier in &after_first {
        assert!(all.contains(earlier), "the second poll lost {earlier:?}");
    }
    all
}

/// The one docs/BUILDOUT.md 2a deferred until there was a write path to assert
/// against, and it is run twice.
///
/// A Range-ignoring proxy answers every poll with the whole file, so the second poll
/// re-receives every byte the first one already stored. `Client` strips that prefix
/// locally — the duplicate never reaches the write path at all — and the point of
/// comparing the two databases is precisely that: whether the server honoured `Range`
/// is invisible by the time anything is stored. `ON CONFLICT DO NOTHING` is the
/// backstop underneath that, exercised directly in `db::tier1`'s own tests.
#[tokio::test]
async fn a_growing_log_is_ingested_the_same_way_whether_or_not_range_is_honoured() {
    let honoured = growing_file(RangeMode::Honour).await;
    let ignored = growing_file(RangeMode::Ignore).await;
    assert_eq!(honoured, ignored);
}

/// Fermentrack's `writerow` is not atomic over the network. The half-written row must
/// reach the database exactly once — written by the *second* poll, at its real values.
#[tokio::test]
async fn a_row_split_across_two_polls_is_written_once_at_its_real_values() {
    let spec = spec(12);
    let body = spec.full_csv();
    let cut = body.len() - 20; // mid-way through the last row

    let remote_path = spec.remote_path(Kind::Full);
    let server = FixtureServer::start(&remote_path, &body[..cut]).await;
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let mut rig = Rig::new(&remote_path);
    let client = client();

    poll(&mut rig, &client, &url).await;
    let partial = rig.readings();

    server.append(&body[cut..]);
    poll(&mut rig, &client, &url).await;
    let whole = rig.readings();

    assert_eq!(rig.state().byte_offset, body.len() as u64);
    assert!(whole.len() > partial.len(), "the cut row never arrived");

    // The row that was cut is present exactly once, and every value the first poll
    // wrote is untouched — a truncated `18.` must never have landed to be corrected.
    let last_ts = whole.iter().map(|(_, ts, _)| *ts).max().expect("a row");
    assert!(
        !partial.iter().any(|(_, ts, _)| *ts == last_ts),
        "the half-written row was committed by the first poll"
    );
    for earlier in &partial {
        assert!(whole.contains(earlier), "{earlier:?} changed value");
    }
}

// ---------------------------------------------------------------------------
// Across a restart
// ---------------------------------------------------------------------------

/// Close the database between polls. The resume carries no in-memory state at all —
/// offset and header both come back off disk, which is what makes "app closed for a
/// week" a resume rather than a re-download.
#[tokio::test]
async fn a_resume_after_the_app_is_closed_needs_nothing_from_memory() {
    let spec = spec(30);
    let body = spec.full_csv();
    let first_len = body[..body.len() / 2]
        .rfind("\r\n")
        .map(|at| at + 2)
        .expect("a boundary");

    let remote_path = spec.remote_path(Kind::Full);
    let server = FixtureServer::start(&remote_path, &body[..first_len]).await;
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let mut rig = Rig::new(&remote_path);
    let client = client();

    poll(&mut rig, &client, &url).await;
    let before = rig.readings();
    let offset = rig.state().byte_offset;

    rig.reopen();
    assert_eq!(
        rig.state().byte_offset,
        offset,
        "the offset did not persist"
    );
    assert!(rig.state().header.is_some(), "the header did not persist");

    server.append(&body[first_len..]);
    poll(&mut rig, &client, &url).await;

    let after = rig.readings();
    assert_eq!(rig.state().byte_offset, body.len() as u64);
    assert!(after.len() > before.len(), "the resume added nothing");
    for earlier in &before {
        assert!(after.contains(earlier));
    }
    // The resume asked for a range, not the whole file.
    assert_eq!(
        server.last_range().as_deref(),
        Some(format!("bytes={offset}-").as_str())
    );
}

// ---------------------------------------------------------------------------
// The transaction boundary
// ---------------------------------------------------------------------------

/// The whole of 2e's crash-safety argument, asserted directly: when the transaction
/// cannot commit, neither the rows nor the offset move — and in particular the
/// caller's in-memory offset does not either. An offset that advanced past rows that
/// were rolled back is a permanent hole, because no later poll would refetch them.
#[tokio::test]
async fn a_failed_commit_moves_neither_the_rows_nor_the_offset() {
    let spec = spec(10);
    let body = spec.full_csv();
    let remote_path = spec.remote_path(Kind::Full);
    let server = FixtureServer::start(&remote_path, body.clone()).await;
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let rig = Rig::new(&remote_path);

    let mut state = rig.state();
    let outcome = client()
        .fetch(&url, state.byte_offset)
        .await
        .expect("the fixture server answers");
    let step = ingest(&state, outcome).expect("a real _full.csv");
    assert!(matches!(step, SyncStep::Advance(_)));

    // Delete the `sync_state` row out from under the commit: the readings insert
    // succeeds, the UPDATE matches nothing, and the row count check fails the txn.
    rig.db
        .write(|conn| {
            conn.execute(
                "DELETE FROM sync_state WHERE source_id = ?",
                [rig.source_id],
            )?;
            Ok(())
        })
        .expect("delete the row");

    let before = state.clone();
    let result = store::commit(&rig.db, &mut state, &step, Timestamp::now());
    assert!(result.is_err(), "a missing sync_state row must not commit");
    assert_eq!(
        state, before,
        "the in-memory state moved on a failed commit"
    );
    assert!(
        rig.readings().is_empty(),
        "rolled-back rows reached the file"
    );
}

// ---------------------------------------------------------------------------
// Restart and rebuild
// ---------------------------------------------------------------------------

/// A log deleted and recreated shorter than our offset. The readings for that source
/// go, because they belong to a different brew that happened to have the same name.
#[tokio::test]
async fn a_truncated_log_drops_its_readings_and_refetches_from_zero() {
    let long = spec(30);
    let body = long.full_csv();
    let remote_path = long.remote_path(Kind::Full);
    let server = FixtureServer::start(&remote_path, body.clone()).await;
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let mut rig = Rig::new(&remote_path);
    let client = client();

    poll(&mut rig, &client, &url).await;
    assert!(!rig.readings().is_empty());

    let shorter = spec(5).full_csv();
    server.set_body(shorter.clone());
    let step = poll(&mut rig, &client, &url).await;
    assert!(matches!(step, SyncStep::Restart(_)), "{step:?}");

    let state = rig.state();
    assert_eq!(state.byte_offset, 0);
    assert_eq!(state.header, None);
    assert_eq!(state.last_ts, None);
    assert!(state.last_error.is_some(), "a restart must be visible");
    assert!(
        rig.readings().is_empty(),
        "the previous log's readings survived the restart"
    );

    // And the next poll repopulates from the top.
    poll(&mut rig, &client, &url).await;
    assert!(!rig.readings().is_empty());
    assert_eq!(rig.state().byte_offset, shorter.len() as u64);
}

// ---------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------

/// `external_id` is the CSV column name and `metric` is ours. An absent sensor gets
/// no stream at all, so the UI has no empty series to draw.
#[tokio::test]
async fn streams_are_created_per_column_and_only_for_sensors_that_reported() {
    let mut spec = Scenario::Ugly.spec();
    spec.rows = 10;
    let body = spec.full_csv();
    let remote_path = spec.remote_path(Kind::Full);
    let server = FixtureServer::start(&remote_path, body).await;
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let mut rig = Rig::new(&remote_path);
    let client = client();

    let step = poll(&mut rig, &client, &url).await;
    let streams = rig.external_ids();

    assert!(streams.contains(&("beer_temp".to_string(), Metric::TempC)));
    assert!(streams.contains(&("beer_set".to_string(), Metric::SetpointC)));
    // `Ugly` declares `room_temp` and `gravity` absent: the columns exist and every
    // cell is a bare `0`, which is the trap Phase 2b exists to avoid.
    assert!(
        !streams.iter().any(|(_, m)| *m == Metric::RoomTempC),
        "an absent room probe grew a stream: {streams:?}"
    );
    assert!(
        !streams.iter().any(|(_, m)| *m == Metric::Sg),
        "an absent gravity sensor grew a stream: {streams:?}"
    );

    // A second poll over the same source creates no further streams.
    let created = match &step {
        SyncStep::Advance(_) => streams.len(),
        other => panic!("expected an advance, got {other:?}"),
    };
    assert!(created > 0);
    poll(&mut rig, &client, &url).await;
    assert_eq!(rig.external_ids().len(), created);
}

/// A poll that found nothing still moves `synced_at` — the footer's "last checked" —
/// and does not declare a previous failure resolved.
#[tokio::test]
async fn an_idle_poll_records_that_it_happened_without_touching_anything_else() {
    let spec = spec(8);
    let body = spec.full_csv();
    let remote_path = spec.remote_path(Kind::Full);
    let server = FixtureServer::start(&remote_path, body.clone()).await;
    let url = log_url(&server.base_url(), &remote_path).expect("a usable URL");
    let mut rig = Rig::new(&remote_path);
    let client = client();

    poll(&mut rig, &client, &url).await;
    let before = rig.state();

    let mut state = rig.state();
    let outcome = client
        .fetch(&url, state.byte_offset)
        .await
        .expect("the server answers");
    let step = ingest(&state, outcome).expect("a real _full.csv");
    assert_eq!(step, SyncStep::Nothing, "the file did not grow");

    let at = Timestamp::from_secs(before.synced_at.as_secs() + 3_600);
    let report = store::commit(&rig.db, &mut state, &step, at).expect("commit the no-op");
    assert_eq!(report.rows_inserted, 0);

    let after = rig.state();
    assert_eq!(after.synced_at, at);
    assert_eq!(after.byte_offset, before.byte_offset);
    assert_eq!(after.header, before.header);
    assert_eq!(after.last_ts, before.last_ts);
}
