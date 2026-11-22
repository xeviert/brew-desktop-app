//! Phase 6 on the Pi: `brewcore::alert` run on every burst, with its state kept.
//!
//! [`Alerts`] owns the in-memory buffers, the watches the desktop pushed, the alert
//! book, and the alert log. It sits behind one mutex shared by the ingest loop (rows
//! in), the HTTP side (watches, ack, snooze) and [`run`] (the minute tick and the
//! notifications). Nothing holds the lock across an `await`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use brewcore::alert::{
    self, Alert, AlertBook, Buffers, DeviceBuffer, LOG_COLUMNS, LogEvent, LogRecord, WatchSet,
};
use brewcore::journal::{JOURNAL_EXTENSION, rotated_stem};
use brewcore::units::Timestamp;
use tokio::sync::{Notify, watch};

use crate::config::Device;
use crate::journal::Journal;
use crate::notify::Ntfy;
use crate::state;

/// Silence and snooze expiry happen with no row arriving, so something has to look.
const TICK: Duration = Duration::from_secs(60);
/// A failed push is retried after this, not on every tick.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

pub const WATCHES_FILE: &str = "watches.json";
pub const BOOK_FILE: &str = "alerts.json";
/// `<state_dir>/alerts.csv`, written through [`Journal`].
pub const LOG_NAME: &str = "alerts";

pub type Shared = Arc<Mutex<Alerts>>;

/// A poisoned lock still holds consistent state: every mutation below completes
/// before anything that can panic.
pub fn lock(shared: &Shared) -> MutexGuard<'_, Alerts> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Alerts {
    state_dir: PathBuf,
    /// Device index → name, as `burst::Row::device` numbers them.
    names: Vec<String>,
    buffers: Buffers,
    watches: WatchSet,
    book: AlertBook,
    log: Journal,
}

impl Alerts {
    /// Loads state and fills each device's buffer from its journals, so a restart
    /// judges a ferment on the week behind it rather than on nothing.
    pub fn open(
        state_dir: &Path,
        journal_dir: &Path,
        devices: &[Device],
        now: Timestamp,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let watches = state::load(&state_dir.join(WATCHES_FILE))?.unwrap_or_default();
        let book = state::load(&state_dir.join(BOOK_FILE))?.unwrap_or_else(|| AlertBook::new(now));
        let log = Journal::open(state_dir, LOG_NAME, &LOG_COLUMNS, now)?;

        let mut buffers = Buffers::new();
        for device in devices {
            let columns = device.columns.iter().map(|c| c.column.clone()).collect();
            let mut buffer = DeviceBuffer::new(columns);
            for path in journals_of(journal_dir, &device.name, now)? {
                let loaded = std::fs::File::open(&path)
                    .map_err(csv::Error::from)
                    .and_then(|f| buffer.load_csv(std::io::BufReader::new(f)));
                if let Err(e) = loaded {
                    tracing::warn!(journal = %path.display(), error = %e, "cannot preload journal");
                }
            }
            buffer.trim(now);
            buffers.insert(device.name.clone(), buffer);
        }
        Ok(Self {
            state_dir: state_dir.to_owned(),
            names: devices.iter().map(|d| d.name.clone()).collect(),
            buffers,
            watches,
            book,
            log,
        })
    }

    /// One journalled row. Evaluates the watches that read this device; `true` when
    /// something was raised and a notification may be owed.
    pub fn on_row(&mut self, device: usize, time: Timestamp, cells: &[Option<String>]) -> bool {
        let Some(name) = self.names.get(device).cloned() else {
            return false;
        };
        if let Some(buffer) = self.buffers.get_mut(&name) {
            buffer.push(time, cells);
            buffer.trim(time);
        }
        self.evaluate(|w| w.reads(&name), time)
    }

    /// Every watch, for what changes with no row: silence, mostly.
    pub fn tick(&mut self, now: Timestamp) -> bool {
        self.evaluate(|_| true, now)
    }

    fn evaluate(&mut self, which: impl Fn(&alert::Watch) -> bool, now: Timestamp) -> bool {
        let mut records = Vec::new();
        for watch in self.watches.watches.iter().filter(|w| which(w)) {
            let conditions = alert::evaluate(watch, &self.buffers, now);
            records.extend(self.book.step(watch, conditions, now));
        }
        let raised = records.iter().any(|r| r.event == LogEvent::Raised);
        self.record(&records);
        raised
    }

    /// Replaces the watch set, clears the alerts of batches no longer in it, and
    /// judges the new set at once.
    pub fn set_watches(&mut self, set: WatchSet, now: Timestamp) -> std::io::Result<bool> {
        state::save(&self.state_dir.join(WATCHES_FILE), &set)?;
        self.watches = set;
        let ids: HashSet<i64> = self.watches.watches.iter().map(|w| w.batch_id).collect();
        let cleared = self.book.retain(&ids, now);
        self.record(&cleared);
        Ok(self.tick(now))
    }

    pub fn watches(&self) -> &WatchSet {
        &self.watches
    }

    pub fn alerts(&self) -> &[Alert] {
        self.book.alerts()
    }

    pub fn ack(&mut self, id: u64, now: Timestamp) -> bool {
        let record = self.book.ack(id, now);
        self.record_one(record)
    }

    pub fn snooze(&mut self, id: u64, until: Timestamp, now: Timestamp) -> bool {
        let record = self.book.snooze(id, until, now);
        self.record_one(record)
    }

    pub fn due(&self, now: Timestamp) -> Vec<Alert> {
        self.book.due(now)
    }

    pub fn notified(&mut self, id: u64, now: Timestamp) {
        let record = self.book.notified(id, now);
        self.record_one(record);
    }

    fn record_one(&mut self, record: Option<LogRecord>) -> bool {
        let found = record.is_some();
        self.record(&record.into_iter().collect::<Vec<_>>());
        found
    }

    /// Logs the transitions and saves the book. Failures are logged, not returned:
    /// the alert has happened whether or not the disk took it, and the next change
    /// saves the book again.
    fn record(&mut self, records: &[LogRecord]) {
        if records.is_empty() {
            return;
        }
        for r in records {
            tracing::info!(
                alert = r.alert_id,
                batch = r.batch_id,
                rule = r.rule.as_str(),
                device = r.device.as_deref().unwrap_or(""),
                event = r.event.as_str(),
                detail = %r.detail,
                "alert"
            );
            if let Err(e) = self.log.append(r.time, &r.cells()) {
                tracing::error!(error = %e, "cannot append to the alert log");
            }
        }
        if let Err(e) = state::save(&self.state_dir.join(BOOK_FILE), &self.book) {
            tracing::error!(error = %e, "cannot save alert state");
        }
    }
}

/// A device's rotated journals still inside the buffer's retention, oldest first,
/// then its live one.
fn journals_of(dir: &Path, device: &str, now: Timestamp) -> std::io::Result<Vec<PathBuf>> {
    let oldest = now.as_secs() - alert::buffer::RETENTION_SECS;
    let mut rotated = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some((d, epoch)) = rotated_stem(name) {
            // A rotated journal ends when it was rotated; one before the window holds
            // nothing the buffer would keep.
            if d == device && epoch >= oldest {
                rotated.push((epoch, dir.join(name)));
            }
        }
    }
    rotated.sort();
    let mut out: Vec<PathBuf> = rotated.into_iter().map(|(_, p)| p).collect();
    let live = dir.join(format!("{device}.{JOURNAL_EXTENSION}"));
    if live.exists() {
        out.push(live);
    }
    Ok(out)
}

/// The minute tick and the notifications, until `shutdown` fires. `wake` is the
/// ingest loop or the HTTP side saying something was just raised.
pub async fn run(
    shared: Shared,
    ntfy: Option<Ntfy>,
    wake: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(TICK);
    let mut retry_at: HashMap<u64, Instant> = HashMap::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tick.tick() => {
                lock(&shared).tick(Timestamp::now());
            }
            () = wake.notified() => {}
        }
        let Some(ntfy) = &ntfy else { continue };
        let due = lock(&shared).due(Timestamp::now());
        let now = Instant::now();
        retry_at.retain(|id, _| due.iter().any(|a| a.id == *id));
        for alert in due {
            if retry_at.get(&alert.id).is_some_and(|&at| at > now) {
                continue;
            }
            match ntfy.send_alert(&alert).await {
                Ok(()) => {
                    retry_at.remove(&alert.id);
                    lock(&shared).notified(alert.id, Timestamp::now());
                }
                Err(e) => {
                    tracing::warn!(alert = alert.id, error = %e, "cannot push alert, will retry");
                    retry_at.insert(alert.id, now + RETRY_AFTER);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::Column;
    use brewcore::alert::{Feed, Rule, Watch};
    use brewcore::calc::{health::HealthParams, stability::StabilityParams};

    const T0: Timestamp = Timestamp::from_secs(1_790_000_000);

    fn probe() -> Device {
        Device {
            name: "esp32-1".into(),
            columns: vec![Column {
                topic: "t".into(),
                column: "vessel_temp".into(),
            }],
        }
    }

    pub(crate) fn watch() -> Watch {
        Watch {
            batch_id: 7,
            code: "SELTZER-007".into(),
            gravity: None,
            temp: Some(Feed {
                device: "esp32-1".into(),
                column: "vessel_temp".into(),
                since: T0,
            }),
            devices: vec!["esp32-1".into()],
            og: None,
            expected_fg: None,
            target_sg: None,
            temp_min_c: None,
            temp_max_c: Some(20.0),
            band_since: None,
            step: None,
            stability: StabilityParams::default(),
            health: HealthParams::default(),
            params: Default::default(),
        }
    }

    fn temp(alerts: &mut Alerts, secs: i64, value: &str) -> bool {
        alerts.on_row(0, T0.saturating_add_secs(secs), &[Some(value.into())])
    }

    #[test]
    fn rows_raise_an_alert_that_is_logged_saved_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (state, journals) = (dir.path().join("state"), dir.path().join("journals"));
        let mut alerts = Alerts::open(&state, &journals, &[probe()], T0).unwrap();
        alerts
            .set_watches(WatchSet::new(vec![watch()]), T0)
            .unwrap();

        let raised: Vec<bool> = (0..=20)
            .map(|i| temp(&mut alerts, i * 30, "21.0"))
            .collect();
        assert_eq!(raised.iter().filter(|r| **r).count(), 1, "raised once");
        assert_eq!(alerts.alerts()[0].rule, Rule::TempHigh);
        let id = alerts.alerts()[0].id;
        assert!(alerts.ack(id, T0.saturating_add_secs(700)));
        assert!(!alerts.ack(id + 1, T0));

        let log = std::fs::read_to_string(state.join("alerts.csv")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines[0], alert::LOG_HEADER);
        assert!(lines[1].contains(",temp_high,,raised,,"), "{log}");
        assert!(lines[2].contains(",acked,"), "{log}");

        drop(alerts);
        let reopened = Alerts::open(&state, &journals, &[probe()], T0).unwrap();
        assert_eq!(reopened.watches().watches.len(), 1);
        assert_eq!(reopened.alerts()[0].id, id);
        assert!(reopened.alerts()[0].acked_at.is_some());
    }

    #[test]
    fn a_batch_dropped_from_the_watches_has_its_alerts_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let mut alerts = Alerts::open(dir.path(), dir.path(), &[probe()], T0).unwrap();
        alerts
            .set_watches(WatchSet::new(vec![watch()]), T0)
            .unwrap();
        for i in 0..=20 {
            temp(&mut alerts, i * 30, "21.0");
        }
        assert_eq!(alerts.alerts().len(), 1);
        alerts.set_watches(WatchSet::new(vec![]), T0).unwrap();
        assert!(alerts.alerts().is_empty());
        let log = std::fs::read_to_string(dir.path().join("alerts.csv")).unwrap();
        assert!(log.contains("cleared,,batch no longer watched"), "{log}");
    }

    #[test]
    fn the_buffer_is_preloaded_from_live_and_rotated_journals() {
        let dir = tempfile::tempdir().unwrap();
        let journals = dir.path().join("journals");
        std::fs::create_dir(&journals).unwrap();
        let rotated_at = T0.as_secs() + 300;
        std::fs::write(
            journals.join(format!("esp32-1.{rotated_at}.csv")),
            format!(
                "time,vessel_temp\n{},21.0\n{},21.0\n",
                T0.as_secs(),
                T0.as_secs() + 300
            ),
        )
        .unwrap();
        std::fs::write(
            journals.join("esp32-1.csv"),
            format!("time,vessel_temp\n{},21.0\n", T0.as_secs() + 600),
        )
        .unwrap();
        // Long rotated away: outside the retention, never read.
        std::fs::write(
            journals.join("esp32-1.1000.csv"),
            "time,vessel_temp\n1000,99\n",
        )
        .unwrap();

        let now = T0.saturating_add_secs(600);
        let mut alerts =
            Alerts::open(&dir.path().join("state"), &journals, &[probe()], now).unwrap();
        // Ten minutes over the band are already behind it: the first tick raises.
        assert!(
            alerts
                .set_watches(WatchSet::new(vec![watch()]), now)
                .unwrap()
        );
    }

    /// `run` against a fake ntfy: a raised alert is pushed once and marked; a refused
    /// push leaves it owed.
    async fn pushed(status: axum::http::StatusCode) -> (Option<Timestamp>, usize) {
        let dir = tempfile::tempdir().unwrap();
        let mut alerts = Alerts::open(dir.path(), dir.path(), &[probe()], T0).unwrap();
        // No silence rule: `run` ticks at the wall clock, long after T0.
        let mut w = watch();
        w.devices.clear();
        alerts.set_watches(WatchSet::new(vec![w]), T0).unwrap();
        for i in 0..=20 {
            temp(&mut alerts, i * 30, "21.0");
        }
        let shared = Arc::new(Mutex::new(alerts));
        let (config, seen) = crate::notify::tests::fake_ntfy(status).await;
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(run(
            shared.clone(),
            Some(Ntfy::new(&config).unwrap()),
            Arc::new(Notify::new()),
            stopped,
        ));
        // The interval's first tick is immediate, so the push follows at once.
        let sent = async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), sent)
            .await
            .expect("nothing was pushed");
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let notified = lock(&shared).alerts()[0].notified_at;
        let count = seen.lock().unwrap().len();
        (notified, count)
    }

    #[tokio::test]
    async fn a_raised_alert_is_pushed_once_and_a_refused_push_stays_owed() {
        let (notified, count) = pushed(axum::http::StatusCode::OK).await;
        assert!(notified.is_some());
        assert_eq!(count, 1);

        let (notified, count) = pushed(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        assert!(notified.is_none());
        assert_eq!(count, 1, "not retried before RETRY_AFTER");
    }
}
