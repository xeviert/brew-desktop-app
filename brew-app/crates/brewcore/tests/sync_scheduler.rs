//! Phase 2g's scheduler, against a scripted source.
//!
//! No HTTP and no database here on purpose. Everything below is about *when* a source
//! is polled and what is said about it, and a real `FermentrackSource` would only add
//! two moving parts to tests whose subject is the timer. `sync_source.rs` is the other
//! half: the real thing against a real socket.
//!
//! Time is paused. The backoff ceiling is fifteen minutes and a test that actually
//! waited for it would be the slowest thing in the workspace; with `start_paused` tokio
//! advances the clock to the next deadline whenever the runtime is idle, so a
//! `sleep(one hour)` in a test is instant and the source still observes real intervals
//! between its polls.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use brewcore::{
    Error, Result,
    config::{Config, ConfigStore, SyncConfig},
    sync::{PollReport, ReadingSource, RecordingSink, SourceStatus, SyncManager, SyncPhase},
    units::Timestamp,
};

const SOURCE_ID: i64 = 7;
const INTERVAL: u64 = 60;
/// Mirrors `sync::BACKOFF_CEILING_SECS`, restated so a test failure names the number.
const CEILING: u64 = 15 * 60;

/// What the next poll should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// A clean poll storing `n` rows.
    Stored(usize),
    /// The poll succeeded, stored what it could, and committed a `last_error`. The
    /// request was fine; the data was not. This is the case a scheduler that only
    /// counted `Err` would poll at full speed forever.
    Fault,
    /// The request itself failed.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallKind {
    Poll,
    Rebuild,
}

#[derive(Debug, Clone, Copy)]
struct Call {
    kind: CallKind,
    at: tokio::time::Instant,
}

/// A `ReadingSource` that does nothing but record when it was asked and answer however
/// the test currently says to.
#[derive(Clone)]
struct Script {
    calls: Arc<Mutex<Vec<Call>>>,
    answer: Arc<Mutex<Answer>>,
}

impl Script {
    fn new(answer: Answer) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            answer: Arc::new(Mutex::new(answer)),
        }
    }

    fn set(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn kinds(&self) -> Vec<CallKind> {
        self.calls().iter().map(|call| call.kind).collect()
    }

    /// Seconds between consecutive polls, as the source experienced them.
    fn gaps(&self) -> Vec<u64> {
        self.calls()
            .windows(2)
            .map(|pair| (pair[1].at - pair[0].at).as_secs())
            .collect()
    }

    fn record(&self, kind: CallKind) -> Result<PollReport> {
        self.calls.lock().unwrap().push(Call {
            kind,
            at: tokio::time::Instant::now(),
        });
        match *self.answer.lock().unwrap() {
            Answer::Stored(rows) => Ok(PollReport {
                rows_added: rows,
                rows_ignored: 0,
                fault: None,
                newest_reading: Some(Timestamp::from_secs(1_000)),
            }),
            Answer::Fault => Ok(PollReport {
                rows_added: 0,
                rows_ignored: 0,
                fault: Some("all 3 records in this chunk were skipped".to_owned()),
                newest_reading: None,
            }),
            Answer::Failed => Err(Error::LogMissing {
                url: "http://fermentrack.local/data/gone_full.csv".to_owned(),
            }),
        }
    }
}

#[async_trait::async_trait]
impl ReadingSource for Script {
    fn source_id(&self) -> i64 {
        SOURCE_ID
    }

    async fn poll(&mut self) -> Result<PollReport> {
        self.record(CallKind::Poll)
    }

    async fn rebuild(&mut self) -> Result<PollReport> {
        self.record(CallKind::Rebuild)
    }
}

/// A `ConfigStore` on a tempdir, at `INTERVAL`.
fn config() -> (ConfigStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = ConfigStore::open(dir.path().join("config.toml")).expect("open the config");
    store
        .set(Config {
            sync: SyncConfig {
                poll_interval_secs: INTERVAL,
            },
            ..Config::default()
        })
        .expect("store the interval");
    (store, dir)
}

struct Rig {
    manager: SyncManager,
    script: Script,
    sink: Arc<RecordingSink>,
    _config_dir: tempfile::TempDir,
}

impl Rig {
    fn start(answer: Answer) -> Self {
        let (config, dir) = config();
        let sink = Arc::new(RecordingSink::new());
        let manager = SyncManager::new(config, Arc::clone(&sink) as _);
        let script = Script::new(answer);
        manager.spawn(Box::new(script.clone()));
        Self {
            manager,
            script,
            sink,
            _config_dir: dir,
        }
    }

    /// Let the clock run for `secs` of *scheduler* time. Instant in wall-clock terms:
    /// the runtime has nothing to do but fire timers.
    async fn run_for(&self, secs: u64) {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }

    /// Give the task a chance to reach its next `await` without moving the clock.
    async fn settle(&self) {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    fn status(&self) -> SourceStatus {
        self.manager
            .statuses()
            .into_iter()
            .find(|status| status.source_id == SOURCE_ID)
            .expect("the source is scheduled")
    }
}

// ---------------------------------------------------------------------------
// The timer
// ---------------------------------------------------------------------------

/// A source that is added must not wait a minute to show its first reading, and the
/// launch poll is also what fills an empty database on first run.
#[tokio::test(start_paused = true)]
async fn a_source_is_polled_on_launch_and_then_on_the_interval() {
    let rig = Rig::start(Answer::Stored(3));
    rig.settle().await;
    assert_eq!(rig.script.calls().len(), 1, "no poll on launch");

    rig.run_for(10 * INTERVAL).await;
    let calls = rig.script.calls().len();
    // Ten intervals, ±20% jitter, so eight to twelve polls plus the launch one.
    assert!(
        (9..=13).contains(&calls),
        "{calls} polls in ten intervals is not a 60 s cadence"
    );
    assert!(rig.script.kinds().iter().all(|k| *k == CallKind::Poll));

    rig.manager.shutdown().await;
}

/// The jitter exists so that three fermenters added together do not poll together. It
/// is only doing that if the gaps actually differ.
#[tokio::test(start_paused = true)]
async fn the_interval_is_jittered_rather_than_exact() {
    let rig = Rig::start(Answer::Stored(1));
    rig.run_for(20 * INTERVAL).await;
    rig.manager.shutdown().await;

    let gaps = rig.script.gaps();
    assert!(gaps.len() > 10, "not enough polls to judge: {gaps:?}");
    for gap in &gaps {
        assert!(
            (48..=72).contains(gap),
            "a {gap} s gap is outside ±20% of {INTERVAL} s"
        );
    }
    assert!(
        gaps.iter().any(|gap| *gap != gaps[0]),
        "every gap identical — the jitter is not applied: {gaps:?}"
    );
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_failing_source_is_asked_less_and_less_often() {
    let rig = Rig::start(Answer::Failed);
    rig.run_for(60 * 60).await;
    let status = rig.status();
    rig.manager.shutdown().await;

    let gaps = rig.script.gaps();
    assert!(gaps.len() >= 5, "too few polls to see a curve: {gaps:?}");
    // The launch poll already failed, so the first wait is 2×, not 1×: 120, 240, 480,
    // then the 900 s ceiling — each within ±20%.
    for (n, gap) in gaps.iter().take(4).enumerate() {
        let expected = (INTERVAL << (n + 1)).min(CEILING);
        let slack = expected / 5;
        assert!(
            (expected - slack..=expected + slack).contains(gap),
            "failure {n} waited {gap} s, expected about {expected} s: {gaps:?}"
        );
    }
    assert!(
        gaps.iter().all(|gap| *gap <= CEILING + CEILING / 5),
        "a delay ran past the ceiling: {gaps:?}"
    );
    assert_eq!(status.phase, SyncPhase::Error);
    assert_eq!(
        status.last_error_code.as_deref(),
        Some("fermentrack.log_missing")
    );
}

/// **The rule this phase turns on.** A restart loop against a Range-ignoring proxy in
/// front of a rotated log answers `200` to every request: nothing fails, and yet every
/// poll re-downloads the whole file and stores none of it. Backing off only on `Err`
/// would hammer it at the healthy cadence indefinitely.
#[tokio::test(start_paused = true)]
async fn a_committed_last_error_backs_off_even_though_the_request_succeeded() {
    let rig = Rig::start(Answer::Fault);
    rig.run_for(60 * 60).await;
    let status = rig.status();
    rig.manager.shutdown().await;

    let gaps = rig.script.gaps();
    assert!(gaps.len() >= 4, "too few polls: {gaps:?}");
    assert!(
        gaps[1] > gaps[0] && gaps[2] > gaps[1],
        "a successful request with a committed fault did not back off: {gaps:?}"
    );

    assert_eq!(status.phase, SyncPhase::Error);
    assert!(status.consecutive_failures >= 3);
    assert_eq!(
        status.last_error.as_deref(),
        Some("all 3 records in this chunk were skipped")
    );
    // And it is genuinely the fault path, not a disguised error: the poll returned
    // `Ok`, so a `last_success` would have been recorded if the fault were ignored.
    assert_eq!(status.last_success, None);
}

#[tokio::test(start_paused = true)]
async fn one_clean_poll_returns_a_failing_source_to_the_normal_cadence() {
    let rig = Rig::start(Answer::Failed);
    rig.run_for(10 * 60).await;
    let backed_off = *rig.script.gaps().last().expect("some gaps");
    assert!(backed_off > 2 * INTERVAL, "never backed off: {backed_off}");

    rig.script.set(Answer::Stored(5));
    // Long enough to cover the backed-off wait plus several healthy intervals.
    rig.run_for(20 * 60).await;
    let status = rig.status();
    rig.manager.shutdown().await;

    let gaps = rig.script.gaps();
    let recovered = &gaps[gaps.len() - 3..];
    for gap in recovered {
        assert!(
            (48..=72).contains(gap),
            "still backed off after a success: {gaps:?}"
        );
    }

    assert_eq!(status.phase, SyncPhase::Idle);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.last_error, None);
    assert_eq!(status.last_error_code, None);
    assert_eq!(status.rows_added, 5);
    assert!(status.last_success.is_some());
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn resync_polls_immediately_instead_of_waiting_for_the_tick() {
    let rig = Rig::start(Answer::Stored(2));
    rig.settle().await;
    assert_eq!(rig.script.calls().len(), 1);

    rig.manager
        .resync(SOURCE_ID)
        .expect("the source is running");
    rig.settle().await;
    assert_eq!(rig.script.calls().len(), 2, "resync did not cut the sleep");
    assert!(rig.script.kinds().iter().all(|k| *k == CallKind::Poll));

    rig.manager.shutdown().await;
}

/// Rebuild is one reset followed by ordinary polling — not a mode the source stays in.
#[tokio::test(start_paused = true)]
async fn rebuild_runs_once_and_then_goes_back_to_polling() {
    let rig = Rig::start(Answer::Stored(1));
    rig.settle().await;

    rig.manager
        .rebuild(SOURCE_ID)
        .expect("the source is running");
    rig.settle().await;
    assert_eq!(rig.script.kinds(), vec![CallKind::Poll, CallKind::Rebuild]);

    rig.run_for(3 * INTERVAL).await;
    rig.manager.shutdown().await;

    let kinds = rig.script.kinds();
    assert_eq!(
        kinds.iter().filter(|k| **k == CallKind::Rebuild).count(),
        1,
        "the source stayed in rebuild mode: {kinds:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_command_for_an_unscheduled_source_is_an_error_rather_than_a_silent_no_op() {
    let rig = Rig::start(Answer::Stored(1));
    let err = rig.manager.resync(SOURCE_ID + 1).unwrap_err();
    assert_eq!(err.code(), "sync.not_scheduled");
    let err = rig.manager.rebuild(SOURCE_ID + 1).unwrap_err();
    assert_eq!(err.code(), "sync.not_scheduled");
    rig.manager.shutdown().await;
}

/// 2f can repoint a source at a different log. Two tasks writing one `sync_state` row
/// would fight over the byte offset, and the loser's rows would be stored under the
/// winner's.
#[tokio::test(start_paused = true)]
async fn re_spawning_a_source_replaces_its_task_rather_than_adding_a_second() {
    let rig = Rig::start(Answer::Stored(1));
    rig.settle().await;

    let replacement = Script::new(Answer::Stored(9));
    rig.manager.spawn(Box::new(replacement.clone()));
    rig.run_for(5 * INTERVAL).await;

    let before = rig.script.calls().len();
    rig.run_for(5 * INTERVAL).await;
    assert_eq!(
        rig.script.calls().len(),
        before,
        "the replaced task is still polling"
    );
    assert!(!replacement.calls().is_empty(), "the replacement never ran");
    assert_eq!(rig.manager.statuses().len(), 1);

    rig.manager.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_every_task() {
    let rig = Rig::start(Answer::Stored(1));
    rig.run_for(2 * INTERVAL).await;
    rig.manager.shutdown().await;

    let after_shutdown = rig.script.calls().len();
    rig.run_for(20 * INTERVAL).await;
    assert_eq!(
        rig.script.calls().len(),
        after_shutdown,
        "a task outlived shutdown"
    );
    assert!(rig.manager.statuses().is_empty());
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn every_poll_emits_syncing_and_then_its_outcome() {
    let rig = Rig::start(Answer::Stored(4));
    rig.settle().await;

    let phases = rig.sink.phases();
    assert_eq!(
        &phases[..3],
        &[SyncPhase::Syncing, SyncPhase::Idle, SyncPhase::Idle],
        "expected Syncing, the outcome, then the scheduled next poll"
    );

    rig.script.set(Answer::Failed);
    rig.manager.resync(SOURCE_ID).expect("running");
    rig.settle().await;
    assert!(rig.sink.phases().contains(&SyncPhase::Error));

    // The final emit of each round carries the deadline, which is what the pane counts
    // down to; the in-flight one deliberately does not.
    let last = rig.sink.statuses().pop().expect("some statuses");
    assert!(last.next_poll_at.is_some());
    let syncing = rig
        .sink
        .statuses()
        .into_iter()
        .find(|s| s.phase == SyncPhase::Syncing)
        .expect("a syncing status");
    assert_eq!(syncing.next_poll_at, None);

    rig.manager.shutdown().await;
}

/// The window can open mid-poll, before any event has been emitted into it. Without a
/// snapshot the pane would render "never" for a source that is working fine.
#[tokio::test(start_paused = true)]
async fn the_status_snapshot_matches_the_last_event_emitted() {
    let rig = Rig::start(Answer::Stored(6));
    rig.settle().await;

    let snapshot = rig.status();
    let emitted = rig.sink.statuses().pop().expect("some statuses");
    assert_eq!(snapshot, emitted);
    assert_eq!(snapshot.rows_added, 6);
    assert_eq!(snapshot.newest_reading, Some(Timestamp::from_secs(1_000)));

    rig.manager.shutdown().await;
}
