//! Phase 6. The alert round on a timer: watches out to `brewd`, its alert log back in.
//!
//! Beside the reading sync rather than inside it — the log is not a source, and a
//! `brewd` refusing a token must not back off the readings. Same interval, read from
//! the live config each round. A change the user makes (a batch's band, an ack) wakes
//! it early, so `brewd` hears within a second rather than a poll.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use brewcore::{
    brewd::alerts::{AlertSyncReport, sync_alerts},
    config::ConfigStore,
    db::Db,
    fermentrack::Client,
    units::Timestamp,
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

/// Emitted when a round changed something the screens show: alerts applied, watches
/// pushed, or the round started or stopped failing.
pub const ALERTS_EVENT: &str = "alerts://changed";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct AlertSyncStatus {
    pub at: Option<Timestamp>,
    pub report: Option<AlertSyncReport>,
    /// The last round's error code and message; `None` when it went through.
    pub error_code: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct AlertSync {
    wake: Notify,
    status: Mutex<AlertSyncStatus>,
}

impl AlertSync {
    /// Run a round now rather than at the next tick.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn status(&self) -> AlertSyncStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, status: AlertSyncStatus) -> AlertSyncStatus {
        std::mem::replace(
            &mut self.status.lock().unwrap_or_else(PoisonError::into_inner),
            status,
        )
    }
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Arc<AlertSync>>().inner().clone();
        loop {
            round(&app, &state).await;
            let secs = app.state::<ConfigStore>().get().sync.poll_interval_secs;
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(secs)) => {}
                () = state.wake.notified() => {}
            }
        }
    });
}

async fn round(app: &AppHandle, state: &AlertSync) {
    let config = app.state::<ConfigStore>().get();
    let db = app.state::<Db>().inner().clone();
    let client = app.state::<Client>().inner().clone();
    let result = sync_alerts(&db, &client, &config).await;

    let mut status = AlertSyncStatus {
        at: Some(Timestamp::now()),
        ..AlertSyncStatus::default()
    };
    let changed = match result {
        Ok(report) => {
            let changed =
                report.applied > 0 || report.watches == brewcore::brewd::alerts::WatchPush::Pushed;
            status.report = Some(report);
            changed
        }
        Err(err) => {
            // Unconfigured is a state, not a fault; anything else is worth a line.
            if !matches!(err, brewcore::Error::BrewdUnconfigured) {
                tracing::warn!(%err, "alert round failed");
            }
            status.error_code = Some(err.code().to_owned());
            status.error = Some(err.to_string());
            false
        }
    };
    let previous = state.set(status.clone());
    if changed || previous.error_code != status.error_code {
        if let Err(err) = app.emit(ALERTS_EVENT, &status) {
            tracing::warn!(%err, "could not emit an alerts event");
        }
    }
}
