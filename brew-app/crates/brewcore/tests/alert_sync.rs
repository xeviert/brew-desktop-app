//! Phase 6. The desktop's alert round against a mock `brewd`: the log read by `Range`
//! and folded into `alert`, a truncated log read again without doubling anything, and
//! watches pushed only when `brewd` holds a different set.

use brewcore::{
    alert::{LOG_HEADER, WatchSet},
    brewd::alerts::{WatchPush, sync_alerts},
    config::{Brewd, Config},
    db::{
        Db,
        alert::list_alerts,
        batch::{NewBatch, create_batch},
        recipe::BeverageType,
    },
    journal::{Client, ClientSettings},
    units::Timestamp,
};
use brewfixture::FixtureServer;

const T0: Timestamp = Timestamp::from_secs(1_790_000_000);

fn line(time: i64, id: u64, batch: i64, rest: &str) -> String {
    format!("{time},{id},{batch},{rest}\n")
}

struct Rig {
    db: Db,
    client: Client,
    config: Config,
    batch: i64,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn new(server: &FixtureServer, token: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("brewery.db")).unwrap();
        let batch = db
            .write(|conn| {
                create_batch(
                    conn,
                    &NewBatch {
                        code: "SELTZER-001".into(),
                        recipe_version_id: None,
                        beverage_type: BeverageType::Seltzer,
                        vessel_id: None,
                        volume_l: None,
                        og: None,
                        notes: None,
                    },
                    T0,
                )
            })
            .unwrap();
        Self {
            db,
            client: Client::with_settings(ClientSettings::default()).unwrap(),
            config: Config {
                brewd: Brewd {
                    base_url: server.base_url(),
                    token: token.into(),
                },
                ..Config::default()
            },
            batch,
            _dir: dir,
        }
    }

    fn alerts(&self, open_only: bool) -> Vec<brewcore::db::alert::AlertRow> {
        self.db
            .read(|conn| list_alerts(conn, Some(self.batch), open_only))
            .unwrap()
    }
}

#[tokio::test]
async fn the_log_is_read_by_range_and_a_truncated_one_from_the_start() {
    let header = format!("{LOG_HEADER}\n");
    let server = FixtureServer::start("/alerts/log", header.clone()).await;
    let rig = Rig::new(&server, "");
    let b = rig.batch;

    let raised = line(
        1_790_000_100,
        7,
        b,
        "temp_high,,raised,,\"20.5 °C, above 20.0 °C\"",
    );
    // Half a line: `brewd` mid-write. Not applied, not skipped.
    server.append(format!(
        "{raised}{}",
        &line(1_790_000_200, 7, b, "temp_high,,acked,,")[..12]
    ));
    let report = sync_alerts(&rig.db, &rig.client, &rig.config)
        .await
        .unwrap();
    assert_eq!(report.watches, WatchPush::NoToken);
    assert_eq!(report.applied, 1);
    let open = rig.alerts(true);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].detail, "20.5 °C, above 20.0 °C");
    assert!(open[0].acked_at.is_none());

    server.set_body(format!(
        "{header}{raised}{}{}",
        line(1_790_000_200, 7, b, "temp_high,,acked,,"),
        line(1_790_000_300, 7, b, "temp_high,,cleared,,"),
    ));
    let report = sync_alerts(&rig.db, &rig.client, &rig.config)
        .await
        .unwrap();
    assert_eq!(report.applied, 2);
    assert!(rig.alerts(true).is_empty());
    assert_eq!(
        sync_alerts(&rig.db, &rig.client, &rig.config)
            .await
            .unwrap()
            .applied,
        0,
        "an idle round changes nothing"
    );

    // `brewd` lost its log and started again: read from 0, nothing doubled.
    let again = line(1_790_000_400, 9, b, "stalled,,raised,,flat");
    server.set_body(format!("{header}{again}"));
    let report = sync_alerts(&rig.db, &rig.client, &rig.config)
        .await
        .unwrap();
    assert_eq!(report.applied, 1);
    let all = rig.alerts(false);
    assert_eq!(all.iter().map(|a| a.id).collect::<Vec<_>>(), [9, 7]);
}

#[tokio::test]
async fn a_log_with_another_header_is_refused() {
    let server = FixtureServer::start("/alerts/log", "time,tilt,gravity\n1,2,3\n").await;
    let rig = Rig::new(&server, "");
    let err = sync_alerts(&rig.db, &rig.client, &rig.config)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "alert.invalid_log");
}

#[tokio::test]
async fn watches_are_pushed_only_when_brewd_holds_another_set() {
    let server = FixtureServer::start("/alerts/log", format!("{LOG_HEADER}\n")).await;
    let rig = Rig::new(&server, "0123456789abcdef");

    // Nothing pitched, so the set is empty — and `brewd` holds an empty set.
    let empty = serde_json::to_vec(&WatchSet::new(vec![])).unwrap();
    server.serve_also("/watches", "application/json", empty);
    let report = sync_alerts(&rig.db, &rig.client, &rig.config)
        .await
        .unwrap();
    assert_eq!((report.watches, report.watched), (WatchPush::Unchanged, 0));

    // A `brewd` that lost its state answers something else: the set goes out again.
    server.serve_also("/watches", "application/json", "{}");
    let report = sync_alerts(&rig.db, &rig.client, &rig.config)
        .await
        .unwrap();
    assert_eq!(report.watches, WatchPush::Pushed);
}
