//! One tokio task per source, and the handle the UI drives them through.
//!
//! The loop is deliberately boring: poll, publish, wait, repeat. Everything subtle is
//! in the three rules it obeys.
//!
//! 1. **A committed `last_error` counts as a failure.** See [`PollReport::fault`]. This
//!    is the rule that stops a restart loop against a Range-ignoring proxy from
//!    re-downloading the whole log once a minute for a week without a single network
//!    error to show for it.
//! 2. **`poll_interval_secs` is read inside the loop**, not captured at spawn. It is
//!    why `ConfigStore` owns its lock in `brewcore` rather than in the Tauri shell: a
//!    cadence the user changed has to take effect on the next tick, not the next
//!    launch.
//! 3. **Resync and Rebuild return as soon as the command is queued.** A rebuild of a
//!    week's backlog is one legitimately huge request, and holding a Tauri command open
//!    across it would freeze the button that started it. The result arrives as a
//!    `sync://status` event like every other transition.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    Error, Result,
    config::ConfigStore,
    sync::{
        PollReport, ReadingSource, SourceStatus, StatusSink, SyncPhase,
        backoff::{Jitter, backoff},
    },
    units::Timestamp,
};

/// Depth of a source's command queue.
///
/// Small on purpose. Queueing twenty Resyncs because someone leant on the button would
/// run twenty polls; four is enough that a Resync arriving during a long fetch is not
/// dropped, and small enough that the excess is refused rather than stored.
const COMMAND_QUEUE: usize = 4;

enum Command {
    /// Poll now, incrementally.
    Resync,
    /// Reset to the beginning, then poll.
    Rebuild,
    Shutdown,
}

struct Handle {
    commands: mpsc::Sender<Command>,
    /// Shared with the task. Read by [`SyncManager::statuses`] so a window opening
    /// mid-poll renders the real state instead of waiting for the next event.
    status: Arc<Mutex<SourceStatus>>,
    task: JoinHandle<()>,
}

/// Owns every polling task.
///
/// Cheap to clone; the tasks live behind the `Arc`. Must be constructed and spawned
/// into from inside a tokio runtime — the Tauri shell does this from
/// `tauri::async_runtime`.
#[derive(Clone)]
pub struct SyncManager {
    inner: Arc<Inner>,
}

struct Inner {
    config: ConfigStore,
    sink: Arc<dyn StatusSink>,
    sources: Mutex<HashMap<i64, Handle>>,
}

impl SyncManager {
    pub fn new(config: ConfigStore, sink: Arc<dyn StatusSink>) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                sink,
                sources: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Start polling `source`, immediately and then on the configured interval.
    ///
    /// Re-spawning an id replaces the running task: 2f's `upsert_sync_state` can
    /// repoint a source at a different log, and two tasks writing one `sync_state` row
    /// would fight over the offset.
    pub fn spawn(&self, source: Box<dyn ReadingSource>) {
        let source_id = source.source_id();
        let status = Arc::new(Mutex::new(SourceStatus::new(source_id)));
        let (commands, receiver) = mpsc::channel(COMMAND_QUEUE);

        let task = tokio::spawn(run(
            source,
            receiver,
            self.inner.config.clone(),
            Arc::clone(&self.inner.sink),
            Arc::clone(&status),
        ));

        let replaced = self.lock().insert(
            source_id,
            Handle {
                commands,
                status,
                task,
            },
        );
        if let Some(old) = replaced {
            tracing::info!(source_id, "replacing the polling task for this source");
            old.task.abort();
        }
        tracing::info!(source_id, "polling started");
    }

    /// Poll now rather than at the next tick.
    pub fn resync(&self, source_id: i64) -> Result<()> {
        self.send(source_id, Command::Resync)
    }

    /// Delete this source's readings and re-derive them from the beginning.
    ///
    /// Destructive to Tier 1 only, which is derived data — that is the entire reason
    /// this is safe to offer as a button.
    pub fn rebuild(&self, source_id: i64) -> Result<()> {
        self.send(source_id, Command::Rebuild)
    }

    /// Every source's current status, by id. What `sync_status` returns on first paint.
    pub fn statuses(&self) -> Vec<SourceStatus> {
        let mut out: Vec<SourceStatus> = self
            .lock()
            .values()
            .map(|handle| {
                handle
                    .status
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .collect();
        out.sort_by_key(|status| status.source_id);
        out
    }

    /// Stop every task and wait for the in-flight polls to finish.
    ///
    /// Awaiting matters: a poll aborted mid-`commit` would leave the transaction to be
    /// rolled back by SQLite on close rather than by us, and a test that returned
    /// before its tasks did would race the tempdir it is about to delete.
    pub async fn shutdown(&self) {
        let handles: Vec<Handle> = self.lock().drain().map(|(_, handle)| handle).collect();
        for handle in &handles {
            // Full queue means the task is busy and will see the closed channel when it
            // next reads; either way it stops.
            let _ = handle.commands.try_send(Command::Shutdown);
        }
        for handle in handles {
            // The sender is dropped with the handle, so a task blocked on `recv` wakes
            // even if `try_send` above found the queue full.
            let _ = handle.task.await;
        }
    }

    fn send(&self, source_id: i64, command: Command) -> Result<()> {
        let sender = {
            let sources = self.lock();
            let handle = sources
                .get(&source_id)
                .ok_or(Error::SourceNotScheduled { source_id })?;
            handle.commands.clone()
        };
        // `try_send`, not `send`: this is called from a Tauri command, and blocking it
        // on a queue that is full because a poll is slow is the freeze this whole
        // design avoids. A full queue already holds the same request.
        match sender.try_send(command) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(Error::SourceNotScheduled { source_id })
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Handle>> {
        self.inner
            .sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for SyncManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncManager")
            .field("sources", &self.lock().len())
            .finish()
    }
}

/// One source's whole life: poll, publish, wait, repeat.
async fn run(
    mut source: Box<dyn ReadingSource>,
    mut commands: mpsc::Receiver<Command>,
    config: ConfigStore,
    sink: Arc<dyn StatusSink>,
    shared: Arc<Mutex<SourceStatus>>,
) {
    let source_id = source.source_id();
    let mut jitter = Jitter::for_source(source_id);
    let mut rebuilding = false;

    loop {
        publish(&shared, sink.as_ref(), |status| {
            status.phase = SyncPhase::Syncing;
            status.next_poll_at = None;
        });

        let outcome = if rebuilding {
            source.rebuild().await
        } else {
            source.poll().await
        };
        rebuilding = false;

        let failures = publish(&shared, sink.as_ref(), |status| {
            record(status, outcome, source_id);
        })
        .consecutive_failures;

        // Rule 2: read the cadence now, not at spawn.
        let interval = config.get().sync.poll_interval_secs;
        let delay = jitter.apply(backoff(failures, interval));
        let deadline = tokio::time::Instant::now() + delay;

        publish(&shared, sink.as_ref(), |status| {
            status.next_poll_at =
                Some(Timestamp::now().saturating_add_secs(delay.as_secs() as i64));
        });

        // Wait out the delay, unless a command arrives first.
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {}
            command = commands.recv() => match command {
                Some(Command::Resync) => {}
                Some(Command::Rebuild) => rebuilding = true,
                // A closed channel means the manager dropped the handle, which is
                // shutdown by another name.
                Some(Command::Shutdown) | None => {
                    tracing::info!(source_id, "polling stopped");
                    return;
                }
            },
        }
    }
}

/// Fold one poll's outcome into the status.
///
/// The two failure paths are deliberately identical below the first line: an `Err` and
/// a committed fault differ in where the text came from, not in what they mean.
fn record(status: &mut SourceStatus, outcome: Result<PollReport>, source_id: i64) {
    let (fault, rows_added, newest) = match outcome {
        Ok(report) => (report.fault, report.rows_added, report.newest_reading),
        Err(err) => {
            tracing::warn!(source_id, %err, "poll failed");
            (Some(err.to_string()), 0, status.newest_reading)
        }
    };

    status.rows_added = rows_added;
    status.newest_reading = newest;

    match fault {
        None => {
            status.consecutive_failures = 0;
            status.last_success = Some(Timestamp::now());
            status.last_error = None;
            status.phase = SyncPhase::Idle;
        }
        Some(error) => {
            // Rule 1. Saturating because a source left failing for a very long time
            // must not wrap its counter back to "healthy".
            status.consecutive_failures = status.consecutive_failures.saturating_add(1);
            status.last_error = Some(error);
            status.phase = SyncPhase::Error;
        }
    }
}

/// Mutate the shared status, then emit a snapshot of it.
///
/// The emit happens **outside** the lock. A sink that reached back into
/// [`SyncManager::statuses`] — which the Tauri one effectively could, via a command
/// handled on the same thread — would otherwise deadlock against itself.
fn publish(
    shared: &Mutex<SourceStatus>,
    sink: &dyn StatusSink,
    f: impl FnOnce(&mut SourceStatus),
) -> SourceStatus {
    let snapshot = {
        let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut guard);
        guard.clone()
    };
    sink.emit(&snapshot);
    snapshot
}
