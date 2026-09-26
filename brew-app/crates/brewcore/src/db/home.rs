//! Phase 4f. The home screen: every batch in the fermenter, and every device feeding
//! one.
//!
//! **One card per active batch, built from 4e's dashboard.** The home card and the
//! batch screen read the same [`BatchDashboard`], so they cannot disagree about the
//! gravity or whether it has finished.
//!
//! **Stale per stream.** Each sensor stream bound to the batch now carries its own
//! freshness by [`stream_freshness`]: a 30 s ESP32 and a 15 min iSpindle cannot share a
//! threshold.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::{Connection, OptionalExtension};

use super::{
    batch::{BatchStatus, list_batches},
    dashboard::{BatchDashboard, dashboard_at},
    health::{DeviceHealth, all_device_health},
    series::{Freshness, stream_freshness},
};
use crate::{
    Result,
    calc::{health::HealthParams, stability::StabilityParams},
    units::Timestamp,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HomeSummary {
    pub batches: Vec<HomeBatch>,
    pub devices: Vec<DeviceHealth>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HomeBatch {
    pub code: String,
    pub status: BatchStatus,
    pub vessel: Option<String>,
    pub dashboard: BatchDashboard,
    pub streams: Vec<Freshness>,
    /// Newest reading across `streams`.
    pub newest: Option<Timestamp>,
}

pub fn home_summary(
    conn: &Connection,
    stability: &StabilityParams,
    health: &HealthParams,
) -> Result<HomeSummary> {
    summary_at(conn, stability, health, Timestamp::now())
}

fn summary_at(
    conn: &Connection,
    stability: &StabilityParams,
    health: &HealthParams,
    now: Timestamp,
) -> Result<HomeSummary> {
    let mut batches = Vec::new();
    for batch in list_batches(conn, false)? {
        if !matches!(
            batch.status,
            BatchStatus::Fermenting | BatchStatus::Conditioning
        ) {
            continue;
        }
        let vessel = match batch.vessel_id {
            Some(id) => conn
                .query_row("SELECT name FROM vessel WHERE id = ?", [id], |row| {
                    row.get(0)
                })
                .optional()?,
            None => None,
        };
        let streams = stream_freshness(conn, batch.id, now)?;
        batches.push(HomeBatch {
            newest: streams.iter().filter_map(|s| s.newest).max(),
            dashboard: dashboard_at(conn, batch.id, stability, health, now)?,
            code: batch.code,
            status: batch.status,
            vessel,
            streams,
        });
    }
    Ok(HomeSummary {
        batches,
        devices: all_device_health(conn, health, now)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        Metric, SourceKind,
        batch::{NewBatch, archive_batch, create_batch, transition_batch},
        recipe::BeverageType,
        reference::{VesselKind, create_vessel},
        tier1::{create_source, create_stream, insert_readings},
        timeline::{bind_stream, record_pitch, unbind_stream},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const MIN: i64 = 60;
    const DAY: i64 = 86_400;

    fn at(offset_secs: i64) -> Timestamp {
        Timestamp::from_secs(T0.as_secs() + offset_secs)
    }

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection, code: &str, vessel_id: Option<i64>) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id,
                volume_l: None,
                og: None,
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    fn pitched(conn: &Connection, code: &str) -> i64 {
        let id = batch(conn, code, None);
        record_pitch(conn, id, T0, None).unwrap();
        id
    }

    /// A stream every `interval` over `[0, end)`, bound to `batch_id` from `T0`.
    fn bound(conn: &mut Connection, batch_id: i64, name: &str, end: i64, interval: i64) -> i64 {
        let source = create_source(conn, SourceKind::BrewdJournal, name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source, "temp", Metric::TempC).unwrap();
        let rows: Vec<_> = (0..end)
            .step_by(interval as usize)
            .map(|t| (stream_id, at(t), 18.0))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        bind_stream(conn, batch_id, stream_id, T0, None).unwrap();
        stream_id
    }

    fn summary(conn: &Connection, now: i64) -> HomeSummary {
        summary_at(
            conn,
            &StabilityParams::default(),
            &HealthParams::default(),
            at(now),
        )
        .unwrap()
    }

    fn stale(s: &HomeSummary, label: &str) -> bool {
        s.batches[0]
            .streams
            .iter()
            .find(|f| f.label == label)
            .unwrap()
            .stale
    }

    #[test]
    fn only_fermenting_and_conditioning_batches_that_are_not_archived_show() {
        let conn = db();
        batch(&conn, "SELTZER-001", None);
        let fermenting = pitched(&conn, "SELTZER-002");
        let conditioning = pitched(&conn, "SELTZER-003");
        transition_batch(&conn, conditioning, BatchStatus::Conditioning, at(DAY)).unwrap();
        let packaged = pitched(&conn, "SELTZER-004");
        transition_batch(&conn, packaged, BatchStatus::Conditioning, at(DAY)).unwrap();
        transition_batch(&conn, packaged, BatchStatus::Packaged, at(2 * DAY)).unwrap();
        let archived = pitched(&conn, "SELTZER-005");
        archive_batch(&conn, archived, at(DAY)).unwrap();

        let s = summary(&conn, 3 * DAY);
        let shown: Vec<_> = s.batches.iter().map(|b| b.dashboard.batch_id).collect();
        assert_eq!(shown, [fermenting, conditioning]);
        assert_eq!(s.batches[1].status, BatchStatus::Conditioning);
    }

    #[test]
    fn each_stream_is_stale_by_its_own_interval() {
        let mut conn = db();
        let id = pitched(&conn, "SELTZER-001");
        bound(&mut conn, id, "esp32", DAY, 30);
        bound(&mut conn, id, "ispindel", DAY, 15 * MIN);

        // Five minutes after the ESP32's last reading: past 3 × 30 s, but under the
        // floor. Ten more and it is stale; the iSpindle, 15 min apart, is not.
        let s = summary(&conn, DAY - 30 + 5 * MIN);
        assert!(!stale(&s, "esp32"));
        let s = summary(&conn, DAY - 30 + 11 * MIN);
        assert!(stale(&s, "esp32"));
        assert!(!stale(&s, "ispindel"));
        assert_eq!(s.batches[0].newest, Some(at(DAY - 30)));

        let s = summary(&conn, DAY - 15 * MIN + 46 * MIN);
        assert!(stale(&s, "ispindel"));
    }

    #[test]
    fn a_closed_binding_is_history_not_silence() {
        let mut conn = db();
        let id = pitched(&conn, "SELTZER-001");
        bound(&mut conn, id, "esp32", DAY, 30);
        let old = bound(&mut conn, id, "old probe", DAY / 2, 30);
        let binding: i64 = conn
            .query_row(
                "SELECT id FROM batch_stream WHERE stream_id = ?",
                [old],
                |row| row.get(0),
            )
            .unwrap();
        unbind_stream(&conn, binding, at(DAY / 2)).unwrap();

        let s = summary(&conn, DAY);
        let labels: Vec<_> = s.batches[0]
            .streams
            .iter()
            .map(|f| f.label.as_str())
            .collect();
        assert_eq!(labels, ["esp32"]);
    }

    #[test]
    fn the_card_names_its_vessel_and_lists_every_device() {
        let mut conn = db();
        let vessel = create_vessel(&conn, "FV1", VesselKind::Fermenter, None, None).unwrap();
        let id = batch(&conn, "SELTZER-001", Some(vessel));
        record_pitch(&conn, id, T0, None).unwrap();
        bound(&mut conn, id, "esp32", DAY, 30);
        create_source(&conn, SourceKind::Manual, "manual", None).unwrap();

        let s = summary(&conn, DAY);
        assert_eq!(s.batches[0].vessel.as_deref(), Some("FV1"));
        assert_eq!(s.batches[0].code, "SELTZER-001");
        let devices: Vec<_> = s.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(devices, ["esp32"]);
    }
}
