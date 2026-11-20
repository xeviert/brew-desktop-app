//! MQTT in, journal rows out.
//!
//! Two tasks. [`pump`] owns `rumqttc`'s event loop and does nothing but stamp and
//! forward: `EventLoop::poll` is not documented as cancel-safe, so it never sits in
//! a `select!` beside a timer. [`run`] owns the bursts and the journals, and is the
//! only thing that writes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use brewcore::units::Timestamp;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, SubscribeFilter};
use tokio::sync::{Notify, mpsc, watch};

use crate::alerts::{self, Shared};
use crate::burst::{Bursts, Row};
use crate::config::{Config, Device};
use crate::journal::Journal;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// One publish, stamped on arrival — iSpindle and ESPHome payloads carry no time,
/// and Mosquitto adds none.
#[derive(Debug)]
pub struct Message {
    pub topic: String,
    pub value: String,
    pub at: Instant,
    pub time: Timestamp,
}

/// Topic routing, bursts, and the journals they close into.
pub struct Ingest {
    routes: HashMap<String, (usize, usize)>,
    journals: Vec<Journal>,
    bursts: Bursts,
    /// Phase 6: every journalled row is judged, and `wake` told when one raises.
    alerts: Option<(Shared, Arc<Notify>)>,
}

impl Ingest {
    /// `journals[d]` is `devices[d]`'s.
    pub fn new(devices: &[Device], journals: Vec<Journal>, window: Duration) -> Self {
        let routes = devices
            .iter()
            .enumerate()
            .flat_map(|(d, device)| {
                device
                    .columns
                    .iter()
                    .enumerate()
                    .map(move |(c, column)| (column.topic.clone(), (d, c)))
            })
            .collect();
        let widths = devices.iter().map(|d| d.columns.len()).collect();
        Self {
            routes,
            journals,
            bursts: Bursts::new(widths, window),
            alerts: None,
        }
    }

    pub fn with_alerts(mut self, alerts: Shared, wake: Arc<Notify>) -> Self {
        self.alerts = Some((alerts, wake));
        self
    }

    pub fn topics(&self) -> impl Iterator<Item = &str> {
        self.routes.keys().map(String::as_str)
    }

    pub fn on_message(&mut self, message: Message) {
        let Some(&(device, column)) = self.routes.get(&message.topic) else {
            tracing::debug!(topic = %message.topic, "no device maps this topic");
            return;
        };
        let rows = self
            .bursts
            .push(device, column, message.value, message.at, message.time);
        self.write(rows);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.bursts.next_deadline()
    }

    pub fn expire(&mut self, now: Instant) {
        let rows = self.bursts.expire(now);
        self.write(rows);
    }

    pub fn drain(&mut self) {
        let rows = self.bursts.drain();
        self.write(rows);
    }

    fn write(&mut self, rows: Vec<Row>) {
        for row in rows {
            let journal = &mut self.journals[row.device];
            // Logged and dropped: a full disk will not be fixed by exiting, and a
            // restart loop would lose the bursts that are still open.
            if let Err(e) = journal.append(row.time, &row.cells) {
                tracing::error!(
                    journal = %journal.path().display(),
                    error = %e,
                    "cannot append to journal, reading lost"
                );
            }
            // Judged even when the journal write failed: the reading happened, and a
            // full disk is no reason to miss a ferment running hot.
            if let Some((shared, wake)) = &self.alerts {
                if alerts::lock(shared).on_row(row.device, row.time, &row.cells) {
                    wake.notify_one();
                }
            }
        }
    }
}

/// Ingest until `shutdown` fires, then close every open burst into its journal.
pub async fn run(config: &Config, mut ingest: Ingest, mut shutdown: watch::Receiver<bool>) {
    let filters: Vec<_> = ingest
        .topics()
        .map(|topic| SubscribeFilter::new(topic.to_owned(), QoS::AtMostOnce))
        .collect();
    let (tx, mut rx) = mpsc::channel(256);
    let broker = format!("{}:{}", config.mqtt.host, config.mqtt.port);
    let pump = tokio::spawn(pump(options(config), broker, filters, tx));

    loop {
        let deadline = ingest.next_deadline();
        tokio::select! {
            _ = shutdown.changed() => break,
            message = rx.recv() => match message {
                Some(message) => ingest.on_message(message),
                None => break,
            },
            () = sleep_until(deadline) => ingest.expire(Instant::now()),
        }
    }

    pump.abort();
    ingest.drain();
}

fn options(config: &Config) -> MqttOptions {
    let mqtt = &config.mqtt;
    let mut options = MqttOptions::new(&mqtt.client_id, &mqtt.host, mqtt.port);
    options.set_clean_session(true);
    if let Some(username) = &mqtt.username {
        options.set_credentials(username, mqtt.password.clone().unwrap_or_default());
    }
    options
}

/// Poll the broker forever, reconnecting with backoff, and forward every live
/// publish. Returns only when `run` has gone.
async fn pump(
    options: MqttOptions,
    broker: String,
    filters: Vec<SubscribeFilter>,
    tx: mpsc::Sender<Message>,
) {
    let (client, mut eventloop) = AsyncClient::new(options, 16);
    let mut backoff = MIN_BACKOFF;

    loop {
        match eventloop.poll().await {
            // Clean session: the broker forgets subscriptions on every disconnect.
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                tracing::info!(%broker, topics = filters.len(), "connected to broker");
                backoff = MIN_BACKOFF;
                if let Err(e) = client.try_subscribe_many(filters.clone()) {
                    tracing::error!(error = %e, "cannot subscribe");
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                // A retained message is the broker replaying an old value on
                // subscribe. Stamping it now would journal a stale reading as fresh.
                // Live messages are forwarded with `retain` cleared.
                if publish.retain {
                    tracing::debug!(topic = %publish.topic, "skipping retained message");
                    continue;
                }
                let message = Message {
                    topic: publish.topic,
                    value: String::from_utf8_lossy(&publish.payload).into_owned(),
                    at: Instant::now(),
                    time: Timestamp::now(),
                };
                if tx.send(message).await.is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    %broker,
                    error = %e,
                    retry_in_secs = backoff.as_secs(),
                    "broker connection failed"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Column;

    const T0: Timestamp = Timestamp::from_secs(1_790_000_000);

    fn device(name: &str, topics: &[(&str, &str)]) -> Device {
        Device {
            name: name.to_owned(),
            columns: topics
                .iter()
                .map(|(topic, column)| Column {
                    topic: (*topic).to_owned(),
                    column: (*column).to_owned(),
                })
                .collect(),
        }
    }

    fn message(topic: &str, value: &str, at: Instant) -> Message {
        Message {
            topic: topic.to_owned(),
            value: value.to_owned(),
            at,
            time: T0,
        }
    }

    /// An iSpindle wake cycle, one topic per field, lands as one journal row; a topic
    /// nobody mapped is ignored rather than misfiled.
    #[test]
    fn a_burst_of_topics_becomes_one_row_in_its_devices_journal() {
        let dir = tempfile::tempdir().unwrap();
        let devices = [
            device(
                "ispindel-1",
                &[
                    ("ispindel/i1/tilt", "tilt"),
                    ("ispindel/i1/gravity", "gravity"),
                ],
            ),
            device(
                "esp32-1",
                &[("esp32-1/sensor/vessel_temp/state", "vessel_temp")],
            ),
        ];
        let journals = devices
            .iter()
            .map(|d| {
                let columns: Vec<_> = d.columns.iter().map(|c| c.column.as_str()).collect();
                Journal::open(dir.path(), &d.name, &columns, T0).unwrap()
            })
            .collect();
        let mut ingest = Ingest::new(&devices, journals, Duration::from_secs(2));
        let now = Instant::now();

        ingest.on_message(message("ispindel/i1/tilt", "45.2", now));
        ingest.on_message(message("ispindel/i1/unmapped", "x", now));
        ingest.on_message(message("esp32-1/sensor/vessel_temp/state", "18.4", now));
        ingest.on_message(message("ispindel/i1/gravity", "1.012", now));

        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(
            read("ispindel-1.csv"),
            "time,tilt,gravity\n1790000000,45.2,1.012\n"
        );
        assert_eq!(read("esp32-1.csv"), "time,vessel_temp\n1790000000,18.4\n");
    }

    /// `run` against a broker nobody is listening on: the pump retries in the
    /// background, and every write comes from bursts opened before `run` started.
    fn run_with_open_burst(dir: &std::path::Path, window: Duration) -> RunningIngest {
        let config: Config = toml::from_str(&format!(
            "journal_dir = {dir:?}
burst_window_ms = {}
[mqtt]
host = \"127.0.0.1\"
port = 1
             [[device]]
name = \"ispindel-1\"
columns = [             {{ topic = \"ispindel/i1/tilt\", column = \"tilt\" }},              {{ topic = \"ispindel/i1/gravity\", column = \"gravity\" }}]",
            window.as_millis()
        ))
        .unwrap();
        let columns = ["tilt", "gravity"];
        let journal = Journal::open(dir, "ispindel-1", &columns, T0).unwrap();
        let mut ingest = Ingest::new(&config.devices, vec![journal], config.burst_window());
        ingest.on_message(message("ispindel/i1/gravity", "1.012", Instant::now()));

        let (shutdown, rx) = watch::channel(false);
        let task = tokio::spawn(async move { run(&config, ingest, rx).await });
        RunningIngest { shutdown, task }
    }

    struct RunningIngest {
        shutdown: watch::Sender<bool>,
        task: tokio::task::JoinHandle<()>,
    }

    impl RunningIngest {
        async fn stop(self) {
            self.shutdown.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .expect("run returns on shutdown")
                .unwrap();
        }
    }

    const PARTIAL: &str = "time,tilt,gravity
1790000000,,1.012
";

    fn journal(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("ispindel-1.csv")).unwrap()
    }

    /// A wake cycle that loses a field is still a reading, written once its window
    /// runs out rather than waiting for a message that will never come.
    #[tokio::test]
    async fn run_writes_an_incomplete_burst_when_its_window_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        let running = run_with_open_burst(dir.path(), Duration::from_millis(100));

        let written = async {
            while journal(dir.path()) != PARTIAL {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), written)
            .await
            .expect("the expired burst was never written");
        running.stop().await;
        assert_eq!(
            journal(dir.path()),
            PARTIAL,
            "written once, not again on drain"
        );
    }

    #[tokio::test]
    async fn shutting_down_writes_the_bursts_still_open() {
        let dir = tempfile::tempdir().unwrap();
        let running = run_with_open_burst(dir.path(), Duration::from_secs(60));
        assert_eq!(
            journal(dir.path()),
            "time,tilt,gravity
"
        );

        running.stop().await;
        assert_eq!(journal(dir.path()), PARTIAL);
    }
}
