//! Phase 2g's other half: the real [`FermentrackSource`] against a real socket.
//!
//! `sync_scheduler.rs` proves *when* a source is polled, with a scripted source and no
//! I/O. This file proves the source itself — that `poll` is 2c–2e wired correctly, that
//! `rebuild` deletes and re-derives, and above all that the two ways a poll can fail
//! come out of it distinguishably. The scheduler's backoff rule is only worth anything
//! if `PollReport::fault` is actually set by the case it was written for.
//!
//! No live Fermentrack: `brewfixture::FixtureServer` serves a growing CSV over HTTP.

use std::sync::Arc;

use brewcore::{
    Result,
    config::{Config, ConfigStore, SyncConfig},
    db::{Db, Metric, SourceKind, tier1},
    fermentrack::{
        Client, ClientSettings, SyncState, build_source, discovery::SourceConfig, store,
    },
    sync::{NoStatus, PollReport, ReadingSource, RecordingSink, SyncManager, SyncPhase},
    units::Timestamp,
};
use brewfixture::{Fault, FixtureServer, Kind, LogSpec, Scenario};

/// A database with one fully-formed Fermentrack source in it, pointed at `server`.
struct Rig {
    db: Db,
    client: Client,
    source_id: i64,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn new(server: &FixtureServer) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let db = Db::open(dir.path().join("brewery.db")).expect("open the database");

        // 2f writes this JSON when a URL is pasted; `build_source` reads `base_url`
        // back out of it. Hand-rolling it here would let the two drift apart.
        let config = serde_json::to_string(&SourceConfig {
            base_url: server.base_url(),
            device_id: None,
            log_id: None,
        })
        .expect("serialise the config");

        let source_id = db
            .write(|conn| {
                tier1::create_source(
                    conn,
                    SourceKind::FermentrackBeerLog,
                    "Fermenter 1",
                    Some(&config),
                )
            })
            .expect("create the source");
        store::upsert_sync_state(&db, source_id, server.path()).expect("create the sync_state row");

        Self {
            db,
            client: Client::with_settings(ClientSettings::default()).expect("build the client"),
            source_id,
            _dir: dir,
        }
    }

    fn source(&self) -> impl ReadingSource + use<> {
        build_source(&self.db, &self.client, self.source_id).expect("the source is complete")
    }

    fn state(&self) -> SyncState {
        store::load_sync_state(&self.db, self.source_id)
            .expect("read the sync_state row")
            .expect("the row exists")
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
}

fn spec(rows: usize) -> LogSpec {
    LogSpec {
        rows,
        ..Scenario::Active.spec()
    }
}

/// A `ConfigStore` on a tempdir, at an interval long enough that no timer fires during
/// a test — every poll below is driven by a command or by the launch poll.
fn config() -> (ConfigStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = ConfigStore::open(dir.path().join("config.toml")).expect("open the config");
    store
        .set(Config {
            sync: SyncConfig {
                poll_interval_secs: 3600,
            },
            ..Config::default()
        })
        .expect("store the interval");
    (store, dir)
}

async fn poll(source: &mut impl ReadingSource) -> Result<PollReport> {
    source.poll().await
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

/// The nine-step loop, twice, over a file that grew in between.
#[tokio::test]
async fn a_source_polls_incrementally_and_reports_what_landed() {
    let spec = spec(30);
    let body = spec.full_csv();
    let cut = body[..body.len() * 2 / 3]
        .rfind("\r\n")
        .map(|at| at + 2)
        .expect("a record boundary");

    let server = FixtureServer::start(spec.remote_path(Kind::Full), &body[..cut]).await;
    let rig = Rig::new(&server);
    let mut source = rig.source();

    let first = poll(&mut source).await.expect("the fixture answers");
    assert!(first.rows_added > 0, "the first poll stored nothing");
    assert_eq!(first.fault, None);
    assert_eq!(first.newest_reading, rig.readings().last().map(|r| r.1));
    assert_eq!(rig.state().byte_offset, cut as u64);
    let after_first = rig.readings();

    server.append(&body[cut..]);
    let second = poll(&mut source).await.expect("the fixture answers");
    assert!(second.rows_added > 0, "the second poll stored nothing");
    assert_eq!(second.fault, None);
    assert_eq!(rig.state().byte_offset, body.len() as u64);

    let all = rig.readings();
    assert_eq!(all.len(), after_first.len() + second.rows_added);
    for earlier in &after_first {
        assert!(all.contains(earlier), "the second poll lost {earlier:?}");
    }
}

/// Polling a file that has not moved must be free and silent — this is the state a
/// healthy source spends almost all its life in.
#[tokio::test]
async fn an_unchanged_log_adds_nothing_and_reports_no_fault() {
    let spec = spec(10);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let rig = Rig::new(&server);
    let mut source = rig.source();

    poll(&mut source).await.expect("the first poll");
    let before = rig.readings();

    let idle = poll(&mut source).await.expect("the second poll");
    assert_eq!(idle.rows_added, 0);
    assert_eq!(idle.fault, None);
    assert_eq!(rig.readings(), before);
}

// ---------------------------------------------------------------------------
// The two kinds of failure
// ---------------------------------------------------------------------------

/// A dead server is an `Err`: nothing was committed, so there is nothing to report.
#[tokio::test]
async fn a_failed_request_comes_back_as_an_error() {
    let spec = spec(10);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let rig = Rig::new(&server);
    let mut source = rig.source();

    server.set_fault(Fault::ServerError);
    let err = poll(&mut source).await.expect_err("500 is not a poll");
    assert_eq!(err.code(), "fermentrack.http_status");
    assert!(rig.readings().is_empty());
    assert_eq!(
        rig.state().byte_offset,
        0,
        "a failed fetch moved the offset"
    );
}

/// **The case Phase 2g's backoff rule exists for.** Toggling `gravity_enabled` on a
/// live beer widens every row without rewriting the header. The HTTP request is
/// perfect — `200`, correct `Content-Type`, real CSV — and yet not one row is storable.
/// `poll` returns `Ok`, and the fault must be in the report or the scheduler will ask
/// again in sixty seconds, forever.
#[tokio::test]
async fn rows_that_no_longer_match_the_header_are_a_fault_on_a_successful_request() {
    let spec = spec(1);
    let header = spec.header(Kind::Full).join(",");
    let mut body = format!("{header}\r\n");
    for minute in 0..3 {
        // Two cells where the header wants ten or more: every record fails on cell
        // count, which is `SyncFault::EveryRecordSkipped`.
        body.push_str(&format!("2025/09/18 00:0{minute}:00Z,18.0\r\n"));
    }

    let server = FixtureServer::start(spec.remote_path(Kind::Full), body).await;
    let rig = Rig::new(&server);
    let mut source = rig.source();

    let report = poll(&mut source)
        .await
        .expect("the request itself succeeded");
    assert_eq!(report.rows_added, 0);
    assert!(
        report.failed(),
        "a chunk in which every record was skipped was reported as healthy"
    );
    assert!(
        report
            .fault
            .as_deref()
            .is_some_and(|text| text.contains("skipped")),
        "unexpected fault text: {:?}",
        report.fault
    );
    // And it is on disk, not only in the report: the footer reads it from there after
    // a restart.
    assert_eq!(rig.state().last_error, report.fault);
    assert_eq!(server.request_count(), 1);
}

// ---------------------------------------------------------------------------
// Rebuild
// ---------------------------------------------------------------------------

/// Rebuild is only safe because Tier 1 is derived: the same bytes must produce the same
/// rows. If this ever stops being true, the button is a data-loss bug.
#[tokio::test]
async fn rebuild_deletes_everything_and_derives_the_identical_set_again() {
    let spec = spec(20);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let rig = Rig::new(&server);
    let mut source = rig.source();

    poll(&mut source).await.expect("the first poll");
    let before = rig.readings();
    assert!(!before.is_empty());
    let offset_before = rig.state().byte_offset;

    let report = source.rebuild().await.expect("the rebuild");

    assert_eq!(rig.readings(), before, "the rebuild changed the readings");
    assert_eq!(rig.state().byte_offset, offset_before);
    assert_eq!(
        report.rows_added,
        before.len(),
        "the report must describe the refetch, not the reset that stored nothing"
    );
    // A user-pressed button is not a condition to report. A `last_error` here would
    // paint a healthy source red and back it off for something already resolved.
    assert_eq!(report.fault, None);
    assert_eq!(rig.state().last_error, None);
}

/// The reason Rebuild deletes rather than re-inserting over the top: `ON CONFLICT DO
/// NOTHING` would preserve precisely the bad rows it exists to remove.
#[tokio::test]
async fn rebuild_removes_rows_that_the_log_no_longer_supports() {
    let spec = spec(20);
    let body = spec.full_csv();
    let server = FixtureServer::start(spec.remote_path(Kind::Full), &body[..]).await;
    let rig = Rig::new(&server);
    let mut source = rig.source();

    poll(&mut source).await.expect("the first poll");
    let full = rig.readings().len();

    // The log is rotated to a shorter one, as if a parser bug's output had to go.
    let shorter = LogSpec {
        rows: 5,
        ..spec.clone()
    }
    .full_csv();
    server.set_body(shorter);
    source.rebuild().await.expect("the rebuild");

    let after = rig.readings().len();
    assert!(
        after < full,
        "rebuild kept {after} rows from a log that now holds fewer than {full}"
    );
    assert!(after > 0, "rebuild left nothing at all");
}

// ---------------------------------------------------------------------------
// Through the scheduler
// ---------------------------------------------------------------------------

/// The whole phase in one test: a real source, on the real manager, with rows on disk
/// that nothing in the test wrote directly.
#[tokio::test]
async fn the_manager_polls_a_real_source_and_resync_picks_up_new_rows() {
    let spec = spec(20);
    let body = spec.full_csv();
    let cut = body[..body.len() / 2]
        .rfind("\r\n")
        .map(|at| at + 2)
        .expect("a record boundary");

    let server = FixtureServer::start(spec.remote_path(Kind::Full), &body[..cut]).await;
    let rig = Rig::new(&server);
    let (config, _config_dir) = config();
    let sink = Arc::new(RecordingSink::new());
    let manager = SyncManager::new(config, Arc::clone(&sink) as _);

    manager.spawn(Box::new(rig.source()));
    wait_for(|| !rig.readings().is_empty()).await;
    let after_launch = rig.readings().len();

    server.append(&body[cut..]);
    manager
        .resync(rig.source_id)
        .expect("the source is running");
    wait_for(|| rig.readings().len() > after_launch).await;

    manager.shutdown().await;

    assert_eq!(rig.state().byte_offset, body.len() as u64);
    let phases = sink.phases();
    assert_eq!(phases[0], SyncPhase::Syncing);
    assert!(phases.contains(&SyncPhase::Idle));
    assert!(!phases.contains(&SyncPhase::Error), "{phases:?}");
}

/// A source with no `sync_state` row is refused when it is built, not once a minute at
/// three in the morning.
#[tokio::test]
async fn a_half_written_source_is_refused_before_a_task_is_spawned() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let db = Db::open(dir.path().join("brewery.db")).expect("open the database");
    let client = Client::with_settings(ClientSettings::default()).expect("build the client");

    let source_id = db
        .write(|conn| tier1::create_source(conn, SourceKind::FermentrackBeerLog, "Orphan", None))
        .expect("create the source");

    let err = build_source(&db, &client, source_id)
        .err()
        .expect("no config, no source");
    assert_eq!(err.code(), "sync.missing_state");

    // And the manager is still usable afterwards: a bad row must not take the app down.
    let (config, _config_dir) = config();
    let manager = SyncManager::new(config, Arc::new(NoStatus));
    assert!(manager.statuses().is_empty());
    manager.shutdown().await;
}

/// Real polls involve a real socket and a real `spawn_blocking`, so there is no paused
/// clock to lean on here. Spin on the condition rather than sleeping a guessed amount.
async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..600 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the condition never became true within six seconds");
}
