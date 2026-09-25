//! Phase 2g. What drives a source, on a timer, and what it reports while doing it.
//!
//! **No `fermentrack` import anywhere in this module** — the same ownership rule
//! `db/tier1.rs` follows. The scheduler must never learn what a byte offset is: that is
//! precisely the coupling that would make v2's `brewd` source a rewrite instead of a
//! new impl of [`ReadingSource`]. Everything Fermentrack-shaped lives behind that trait, in
//! `fermentrack::source`.
//!
//! The trait sits at the level of *readings*, not bytes, and it arrives now rather than
//! in 2c because only now are there two callers: the timer and the Resync command.
//!
//! One rule here is worth more than the rest. **A poll that returned `Ok` but committed
//! a `last_error` is a failure.** A restart loop against a lying proxy raises no network
//! error at all — every request succeeds, every poll re-downloads the whole log, and a
//! scheduler that only counted `Err` would hammer it at the healthy interval forever.
//! See [`PollReport::fault`].

pub mod backoff;
mod scheduler;

pub use backoff::{BACKOFF_CEILING_SECS, JITTER_PERCENT};
pub use scheduler::SyncManager;

use std::sync::{Mutex, PoisonError};

use crate::{Error, Result, units::Timestamp};

/// Anything that can be asked for new readings.
///
/// Object-safe on purpose: [`SyncManager`] holds `Box<dyn ReadingSource>` so that the
/// day a second impl exists it is a new file and not a new scheduler. That is what
/// `async_trait` buys — native async-fn-in-trait cannot be made into a trait object.
#[async_trait::async_trait]
pub trait ReadingSource: Send + Sync {
    fn source_id(&self) -> i64;

    /// One incremental round: ask for whatever is new, store it, report what landed.
    ///
    /// Must be idempotent — the scheduler calls it on launch, on a timer, and whenever
    /// the user presses Resync, and two of those can be milliseconds apart.
    async fn poll(&mut self) -> Result<PollReport>;

    /// Throw away what we derived and derive it again from the beginning.
    ///
    /// The escape hatch for a parser bug that wrote bad rows. Only safe because Tier 1
    /// is derived data and the raw log it came from — Fermentrack's CSV, or `brewd`'s
    /// journal under v2 — is still there to derive it from again. Nothing here may ever
    /// touch Tier 2.
    async fn rebuild(&mut self) -> Result<PollReport>;
}

/// What one poll did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollReport {
    pub rows_added: usize,
    /// Already present, so skipped. Large and steady means the server is ignoring
    /// `Range` and re-sending the whole file every poll.
    pub rows_ignored: usize,
    /// Whatever was committed to `last_error`, if anything.
    ///
    /// `Some` here means this poll **failed**, even though it returned `Ok`: the rows
    /// that could be read were stored, and the condition that stopped the rest was
    /// recorded rather than thrown. The scheduler counts it as a failure and backs off,
    /// which is the whole reason the field is not a bool.
    pub fault: Option<String>,
    /// Newest row actually stored, for the footer's data age.
    pub newest_reading: Option<Timestamp>,
}

impl PollReport {
    pub fn failed(&self) -> bool {
        self.fault.is_some()
    }
}

/// Where a source is right now. Rendered as a dot in the sources pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    /// Waiting for the next poll, last one clean.
    Idle,
    /// A request is in flight.
    Syncing,
    /// The last poll failed, by either definition. The source is still scheduled — it
    /// is simply being asked less often.
    Error,
    /// The last poll could not reach the host at all. A failure like [`Error`](Self::Error)
    /// and backed off the same way, but split out because "the Pi is off" is not a
    /// problem with the source, and the footer says so.
    Unreachable,
}

/// One source's live state, emitted on every transition.
///
/// Carries enough that the UI never has to query the database to render a row: a
/// `sync://status` event replaces the row wholesale.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceStatus {
    pub source_id: i64,
    pub phase: SyncPhase,
    /// The last poll that finished with no fault. Not the last poll that *ran*.
    pub last_success: Option<Timestamp>,
    pub last_error: Option<String>,
    /// [`Error::code`](crate::Error::code) of the last failed poll, so the UI branches on
    /// a code rather than on message text. `None` for a fault that was committed to
    /// `last_error` rather than returned, and after a clean poll.
    pub last_error_code: Option<String>,
    /// Rows stored by the most recent poll.
    pub rows_added: usize,
    pub newest_reading: Option<Timestamp>,
    /// Drives the backoff, and worth showing: "failing since the third attempt" is a
    /// different problem from "failed once just now".
    pub consecutive_failures: u32,
    /// `None` while a poll is in flight.
    pub next_poll_at: Option<Timestamp>,
}

impl SourceStatus {
    pub fn new(source_id: i64) -> Self {
        Self {
            source_id,
            phase: SyncPhase::Idle,
            last_success: None,
            last_error: None,
            last_error_code: None,
            rows_added: 0,
            newest_reading: None,
            consecutive_failures: 0,
            next_poll_at: None,
        }
    }
}

/// Where a [`SourceStatus`] goes.
///
/// A trait because `brewcore` has no `tauri` dependency and never will
/// (`tests/no_tauri.rs`). The shell implements it with `app.emit("sync://status", …)`;
/// tests implement it with a `Vec`.
pub trait StatusSink: Send + Sync + 'static {
    fn emit(&self, status: &SourceStatus);
}

/// For a `SyncManager` driven by tests or by a headless tool.
pub struct NoStatus;

impl StatusSink for NoStatus {
    fn emit(&self, _status: &SourceStatus) {}
}

/// Every status ever emitted, in order. The scheduler's transitions are the thing
/// under test as much as the rows are.
#[derive(Default)]
pub struct RecordingSink {
    seen: Mutex<Vec<SourceStatus>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn statuses(&self) -> Vec<SourceStatus> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn phases(&self) -> Vec<SyncPhase> {
        self.statuses().iter().map(|s| s.phase).collect()
    }
}

impl StatusSink for RecordingSink {
    fn emit(&self, status: &SourceStatus) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(status.clone());
    }
}

/// `spawn_blocking`, with a panic turned into an error.
///
/// A panicking commit must back the source off like any other failure. Left as a
/// `JoinError` it would be unwrapped into a panic inside the scheduler's task, and the
/// source would stop being polled with nothing said about it.
pub(crate) async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(err) => Err(Error::BackgroundTask {
            detail: err.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panic_on_the_blocking_pool_becomes_an_error() {
        let err = blocking(|| -> Result<()> { panic!("commit exploded") })
            .await
            .expect_err("a panic must not escape as a panic");
        assert_eq!(err.code(), "sync.task");
        assert!(err.to_string().contains("panicked"), "{err}");
    }

    #[tokio::test]
    async fn results_pass_through_unchanged() {
        assert_eq!(blocking(|| Ok(7)).await.unwrap(), 7);

        let err = blocking(|| -> Result<()> { Err(Error::MissingSyncState { source_id: 3 }) })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::MissingSyncState { source_id: 3 }));
    }
}
