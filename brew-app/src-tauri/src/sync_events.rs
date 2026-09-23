//! Phase 2g. The one place `brewcore::sync` meets `tauri`.
//!
//! `brewcore` has no `tauri` dependency and never will (`tests/no_tauri.rs`), so the
//! scheduler writes to a [`StatusSink`] and this file is the impl that turns a status
//! into an event. Nothing else in the shell knows the event name.

use brewcore::sync::{SourceStatus, StatusSink};
use tauri::{AppHandle, Emitter};

/// The event the frontend listens on. Prefixed rather than bare so that a second
/// channel later (`sync://progress`) is an obvious sibling.
pub const SYNC_STATUS_EVENT: &str = "sync://status";

pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl StatusSink for TauriSink {
    fn emit(&self, status: &SourceStatus) {
        // Logged, not propagated. A failed emit means the window has gone away, and a
        // poll must not fail because nobody was listening to it.
        if let Err(err) = self.app.emit(SYNC_STATUS_EVENT, status) {
            tracing::warn!(source_id = status.source_id, %err, "could not emit a sync status");
        }
    }
}
