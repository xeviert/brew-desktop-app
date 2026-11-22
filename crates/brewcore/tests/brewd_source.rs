//! Phase Vd. The real [`BrewdSource`] and discovery against a mock `brewd`.
//!
//! `journal_sync.rs` proves the dialect through the shared machinery. This file proves
//! what only the source does: reading its base URL and gravity unit from the live
//! config, draining a rotated journal with no gap and no deletion, and a Rebuild that
//! re-derives from every file `brewd` still has.

use brewcore::{
    brewd::{self, BrewdSource},
    config::{Brewd, Config, ConfigStore, Gravity, IspindelUnit},
    db::{Db, Metric, tier1},
    journal::{Client, ClientSettings, JournalEntry, SyncState, store},
    sync::ReadingSource,
    units::Timestamp,
};
use brewfixture::{FixtureServer, JournalScenario, JournalSpec};

struct Rig {
    db: Db,
    client: Client,
    config: ConfigStore,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn new(server: &FixtureServer) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let db = Db::open(dir.path().join("brewery.db")).expect("open the database");
        let config = ConfigStore::open(dir.path().join("config.toml")).expect("open the config");
        config
            .set(Config {
                brewd: Brewd {
                    base_url: server.base_url(),
                    ..Brewd::default()
                },
                ..Config::default()
            })
            .expect("store the brewd URL");
        Self {
            db,
            client: Client::with_settings(ClientSettings::default()).expect("build the client"),
            config,
            _dir: dir,
        }
    }

    /// Add `journal` the way the Sources pane does: list, probe, add.
    async fn add(&self, journal: &str) -> (i64, BrewdSource) {
        let base_url = self.config.get().brewd.base_url;
        let probe = brewd::probe_journal(&self.client, &base_url, journal)
            .await
            .expect("probe the journal");
        let id = brewd::add_source(&self.db, &probe, &probe.suggested_name).expect("add it");
        let source =
            brewd::build_source(&self.db, &self.client, &self.config, id).expect("build it");
        (id, source)
    }

    fn set_unit(&self, unit: IspindelUnit) {
        let mut config = self.config.get();
        config.gravity = Gravity {
            ispindel_unit: Some(unit),
        };
        self.config.set(config).expect("store the unit");
    }

    fn state(&self, id: i64) -> SyncState {
        store::load_sync_state(&self.db, id)
            .expect("read sync_state")
            .expect("the row exists")
    }

    fn times(&self, id: i64, metric: Metric) -> Vec<Timestamp> {
        self.db
            .read(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT r.ts FROM reading r JOIN stream s ON s.id = r.stream_id \
                     WHERE s.source_id = ? AND s.metric = ? ORDER BY r.ts",
                )?;
                let rows =
                    stmt.query_map(rusqlite::params![id, metric.as_str()], |row| row.get(0))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .expect("read the readings back")
    }

    fn count(&self, id: i64) -> i64 {
        self.db
            .read(|conn| tier1::count_readings(conn, id))
            .expect("count")
    }
}

fn listing(server: &FixtureServer, entries: &[(&str, usize)]) {
    let entries: Vec<JournalEntry> = entries
        .iter()
        .map(|(name, bytes)| JournalEntry {
            name: (*name).to_owned(),
            bytes: *bytes as u64,
            modified: Timestamp::from_secs(1_790_000_000),
        })
        .collect();
    server.serve_also(
        "/journals/",
        "application/json",
        serde_json::to_vec(&entries).expect("serialise the listing"),
    );
}

fn esp32(rows: usize) -> JournalSpec {
    JournalSpec {
        rows,
        ..JournalScenario::Esp32.spec()
    }
}

fn secs(spec: &JournalSpec, range: std::ops::Range<usize>) -> Vec<Timestamp> {
    range
        .map(|i| Timestamp::from_secs(spec.time_at(i)))
        .collect()
}

/// What `brewd` starts after a header change: a room probe added.
fn relabelled(after: i64, rows: usize) -> String {
    let mut out = "time,vessel_temp,chamber_temp,room_temp\n".to_owned();
    for i in 0..rows as i64 {
        out.push_str(&format!("{},18.2,17.1,21.0\n", after + 60 * (i + 1)));
    }
    out
}

#[tokio::test]
async fn discovery_lists_probes_and_adds_a_journal() {
    let spec = esp32(5);
    let server = FixtureServer::start(spec.remote_path(), spec.csv()).await;
    listing(&server, &[("esp32-1.csv", spec.csv().len())]);
    let rig = Rig::new(&server);

    let journals = brewd::list_journals(&rig.client, &server.base_url())
        .await
        .expect("list");
    assert_eq!(journals[0].name, "esp32-1.csv");

    let probe = brewd::probe_journal(&rig.client, &server.base_url(), "esp32-1.csv")
        .await
        .expect("probe");
    assert_eq!(probe.remote_path, "/journals/esp32-1.csv");
    assert_eq!(probe.suggested_name, "esp32-1");
    assert!(!probe.rotated);
    assert!(!probe.header.needs_ispindel_unit);
    let metrics: Vec<_> = probe.header.mapped.iter().map(|c| c.metric).collect();
    assert_eq!(metrics, [Metric::TempC, Metric::FridgeTempC]);

    let (id, mut source) = rig.add("esp32-1.csv").await;
    let report = source.poll().await.expect("poll");
    assert_eq!(report.rows_added, 10);
    assert_eq!(report.fault, None);
    assert_eq!(rig.state(id).byte_offset, spec.csv().len() as u64);
}

#[tokio::test]
async fn a_host_without_a_journal_listing_is_not_brewd() {
    let server = FixtureServer::start("/journals/esp32-1.csv", "time\n").await;
    let rig = Rig::new(&server);
    let err = brewd::list_journals(&rig.client, &server.base_url())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "brewd.not_brewd");

    server.serve_also("/journals/", "text/html", "<html></html>");
    let err = brewd::list_journals(&rig.client, &server.base_url())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "brewd.not_brewd");
}

#[tokio::test]
async fn the_base_url_is_read_live_and_an_empty_one_is_unconfigured() {
    let spec = esp32(3);
    let server = FixtureServer::start(spec.remote_path(), spec.csv()).await;
    let rig = Rig::new(&server);
    let (_, mut source) = rig.add("esp32-1.csv").await;

    rig.config.set(Config::default()).expect("clear the URL");
    let err = source.poll().await.unwrap_err();
    assert_eq!(err.code(), "brewd.unconfigured");

    rig.config
        .set(Config {
            brewd: Brewd {
                base_url: server.base_url(),
                ..Brewd::default()
            },
            ..Config::default()
        })
        .expect("restore it");
    assert_eq!(source.poll().await.expect("poll").rows_added, 6);
}

/// A stopped `brewd` is unreachable, which the footer reports as its own state. A host
/// that answers wrongly is not.
#[tokio::test]
async fn a_stopped_brewd_is_unreachable_and_a_wrong_host_is_not() {
    let spec = esp32(3);
    let server = FixtureServer::start(spec.remote_path(), spec.csv()).await;
    let rig = Rig::new(&server);
    let (_, mut source) = rig.add("esp32-1.csv").await;

    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("find a free port");
    rig.config
        .set(Config {
            brewd: Brewd {
                base_url: format!("http://{closed}"),
                ..Brewd::default()
            },
            ..Config::default()
        })
        .expect("point at nothing");
    let err = source.poll().await.unwrap_err();
    assert!(err.is_unreachable(), "{err:?}");

    let err = brewd::list_journals(&rig.client, &server.base_url())
        .await
        .unwrap_err();
    assert!(!err.is_unreachable(), "{err:?}");
}

#[tokio::test]
async fn an_ispindel_journal_waits_for_its_unit_then_catches_up() {
    let spec = JournalScenario::Ispindel.spec();
    let server = FixtureServer::start(spec.remote_path(), spec.csv()).await;
    let rig = Rig::new(&server);
    let (id, mut source) = rig.add("ispindel-1.csv").await;

    let err = source.poll().await.unwrap_err();
    assert_eq!(err.code(), "brewd.ispindel_unit_unset");
    assert_eq!(rig.state(id).byte_offset, 0);

    rig.set_unit(IspindelUnit::Sg);
    source.poll().await.expect("poll");
    assert_eq!(rig.times(id, Metric::Sg), secs(&spec, 0..spec.rows));
}

/// `brewd` rotates between two polls, after writing rows we had not fetched yet. Those
/// rows are in the rotated file only; the source must read them from there, keep
/// everything it already had, and carry on with the new live file.
async fn rotation_is_drained(live_rows: usize) {
    let before = esp32(10);
    let rotated = esp32(15);
    let rotated_name = rotated.rotated_name(rotated.time_at(15));
    let server = FixtureServer::start(before.remote_path(), before.csv()).await;
    let rig = Rig::new(&server);
    let (id, mut source) = rig.add("esp32-1.csv").await;
    source.poll().await.expect("first poll");
    assert_eq!(rig.count(id), 20);

    // An older rotation with another header must not be mistaken for this one.
    let older = "time,vessel_temp\n1780000000,18.0\n".repeat(20);
    server.serve_also(
        "/journals/esp32-1.1780000000.csv",
        "text/csv",
        older.clone(),
    );
    server.serve_also(
        format!("/journals/{rotated_name}"),
        "text/csv",
        rotated.csv(),
    );
    let live = relabelled(rotated.time_at(14), live_rows);
    server.set_body(live.clone());
    listing(
        &server,
        &[
            ("esp32-1.1780000000.csv", older.len()),
            (&rotated_name, rotated.csv().len()),
            ("esp32-1.csv", live.len()),
        ],
    );

    let report = source.poll().await.expect("the rotation poll");
    assert_eq!(report.fault, None, "a rotation is not a fault");
    assert_eq!(report.rows_added, 10);
    assert_eq!(rig.state(id).byte_offset, 0);
    assert_eq!(rig.times(id, Metric::TempC), secs(&rotated, 0..15));

    source.poll().await.expect("the next poll");
    let after_rotation: Vec<_> = (1..=live_rows as i64)
        .map(|i| Timestamp::from_secs(rotated.time_at(14) + 60 * i))
        .collect();
    let mut expected = secs(&rotated, 0..15);
    expected.extend(after_rotation.iter().copied());
    assert_eq!(rig.times(id, Metric::TempC), expected);
    assert_eq!(rig.times(id, Metric::RoomTempC), after_rotation);
    assert_eq!(rig.state(id).byte_offset, live.len() as u64);
    assert_eq!(rig.state(id).last_error, None);
}

#[tokio::test]
async fn a_rotation_to_a_shorter_file_is_drained_without_a_gap() {
    rotation_is_drained(2).await;
}

#[tokio::test]
async fn a_rotation_to_a_longer_file_is_drained_without_a_gap() {
    rotation_is_drained(40).await;
}

/// A shrink with no rotated file behind it is not a rotation: the restart is reported,
/// and still keeps the readings.
#[tokio::test]
async fn a_truncation_without_a_rotated_file_is_a_fault_that_keeps_readings() {
    let spec = esp32(10);
    let server = FixtureServer::start(spec.remote_path(), spec.csv()).await;
    listing(&server, &[("esp32-1.csv", 0)]);
    let rig = Rig::new(&server);
    let (id, mut source) = rig.add("esp32-1.csv").await;
    source.poll().await.expect("first poll");

    server.truncate_to(40);
    let report = source.poll().await.expect("poll");
    assert!(report.fault.is_some_and(|f| f.contains("truncated")));
    assert_eq!(rig.count(id), 20);
}

#[tokio::test]
async fn rebuild_deletes_then_rereads_every_rotated_file_and_the_live_one() {
    let rotated = esp32(15);
    let rotated_name = rotated.rotated_name(rotated.time_at(15));
    let live = relabelled(rotated.time_at(14), 3);
    let server = FixtureServer::start("/journals/esp32-1.csv", live.clone()).await;
    server.serve_also(
        format!("/journals/{rotated_name}"),
        "text/csv",
        rotated.csv(),
    );
    listing(
        &server,
        &[
            (&rotated_name, rotated.csv().len()),
            ("esp32-1.csv", live.len()),
        ],
    );
    let rig = Rig::new(&server);
    let (id, mut source) = rig.add("esp32-1.csv").await;
    source.poll().await.expect("poll");
    assert_eq!(rig.count(id), 9, "only the live file before a rebuild");

    // A stale reading the rebuild must remove.
    rig.db
        .write(|conn| {
            let tx = conn.transaction()?;
            let stream = tier1::stream_ids_for(&tx, id)?[&Metric::TempC];
            tier1::insert_readings(&tx, &[(stream, Timestamp::from_secs(1), 99.0)])?;
            Ok(tx.commit()?)
        })
        .expect("plant a bad row");

    let report = source.rebuild().await.expect("rebuild");
    assert_eq!(report.fault, None);
    assert_eq!(rig.count(id), 30 + 9);
    assert_eq!(report.rows_added, 39);
    assert_eq!(
        rig.times(id, Metric::TempC)[0],
        Timestamp::from_secs(rotated.time_at(0))
    );
    assert_eq!(rig.state(id).byte_offset, live.len() as u64);
}
