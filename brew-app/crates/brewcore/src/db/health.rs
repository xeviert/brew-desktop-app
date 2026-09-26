//! Phase 4f. Device health per source, not per batch: an iSpindle is a device, and its
//! battery outlives every batch it floats in.
//!
//! **Silent is per stream's own interval.** A source is stale when its newest reading
//! is older than [`stale_after`] for the stream that sent it — the same rule the card's
//! temperature uses. Whether the Pi could be reached is the sync status's business, not
//! this module's: the frontend puts the two side by side.
//!
//! **Why it is silent is a guess**, and [`calc::health`](crate::calc::health) says so.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    Metric, SourceKind, SourceRow,
    series::{SeriesRange, stale_after, stream_buckets},
    tier1::{list_sources, stream_ids_for},
};
use crate::{
    Result,
    calc::health::{HealthParams, SilenceGuess, battery_low, guess, rssi_weak},
    units::Timestamp,
};

/// How far back the battery sparkline and the RSSI baseline look.
pub const HEALTH_WINDOW_SECS: i64 = 7 * 86_400;
/// Points in the battery sparkline. A week at 15 min is 672 readings; a sparkline
/// can't show more than this.
pub const SPARKLINE_POINTS: usize = 84;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeviceHealth {
    pub source_id: i64,
    pub name: String,
    pub kind: SourceKind,
    /// Newest reading across the source's streams.
    pub newest: Option<Timestamp>,
    /// Past [`stale_after`] of the stream that sent `newest`.
    pub stale: bool,
    pub battery: Option<Level>,
    pub rssi: Option<Level>,
    /// The last week of battery, bucketed, oldest first. Empty without a battery stream.
    pub battery_7d: Vec<(Timestamp, f64)>,
    /// Only when `stale`, and a guess.
    pub guess: Option<SilenceGuess>,
}

/// A device's newest word about itself.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Level {
    pub ts: Timestamp,
    pub value: f64,
    /// Under the configured threshold.
    pub warn: bool,
}

/// Every sensor source. Manual entry is not a device.
pub fn all_device_health(
    conn: &Connection,
    params: &HealthParams,
    now: Timestamp,
) -> Result<Vec<DeviceHealth>> {
    list_sources(conn)?
        .iter()
        .filter(|s| s.kind != SourceKind::Manual)
        .map(|s| device_health(conn, s, params, now))
        .collect()
}

/// Sources bound to the batch now that report a battery or a signal — the ones worth a
/// line on its card. An ESP32 on mains has neither.
pub fn batch_device_health(
    conn: &Connection,
    batch_id: i64,
    params: &HealthParams,
    now: Timestamp,
) -> Result<Vec<DeviceHealth>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.source_id FROM batch_stream bs \
           JOIN stream s ON s.id = bs.stream_id \
          WHERE bs.batch_id = ? AND bs.ends_at IS NULL",
    )?;
    let bound: Vec<i64> = stmt
        .query_map([batch_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut out = Vec::new();
    for source in list_sources(conn)? {
        if source.kind == SourceKind::Manual || !bound.contains(&source.id) {
            continue;
        }
        let streams = stream_ids_for(conn, source.id)?;
        if streams.contains_key(&Metric::BatteryV) || streams.contains_key(&Metric::RssiDbm) {
            out.push(device_health(conn, &source, params, now)?);
        }
    }
    Ok(out)
}

fn device_health(
    conn: &Connection,
    source: &SourceRow,
    params: &HealthParams,
    now: Timestamp,
) -> Result<DeviceHealth> {
    let streams = stream_ids_for(conn, source.id)?;

    let mut newest: Option<(Timestamp, i64)> = None;
    for &stream_id in streams.values() {
        if let Some((ts, _)) = latest(conn, stream_id, now)? {
            if newest.is_none_or(|(n, _)| ts > n) {
                newest = Some((ts, stream_id));
            }
        }
    }
    let stale = match newest {
        Some((ts, stream_id)) => ts.secs_until(now) > stale_after(conn, stream_id, ts)?,
        None => false,
    };

    let level = |metric: Metric, warn: fn(f64, &HealthParams) -> bool| -> Result<Option<Level>> {
        let Some(&stream_id) = streams.get(&metric) else {
            return Ok(None);
        };
        Ok(latest(conn, stream_id, now)?.map(|(ts, value)| Level {
            ts,
            value,
            warn: warn(value, params),
        }))
    };
    let battery = level(Metric::BatteryV, battery_low)?;
    let rssi = level(Metric::RssiDbm, rssi_weak)?;

    let battery_7d = match streams.get(&Metric::BatteryV) {
        Some(&stream_id) => stream_buckets(conn, stream_id, week_to(now), SPARKLINE_POINTS)?,
        None => Vec::new(),
    };

    // The week up to the device's last word, not up to now: a device silent for ten
    // days still had a signal before it stopped.
    let guess = match (stale, newest) {
        (true, Some((last, _))) => {
            let rssi_week = match streams.get(&Metric::RssiDbm) {
                Some(&stream_id) => values(conn, stream_id, week_to(last))?,
                None => Vec::new(),
            };
            guess(battery.map(|b| b.value), &rssi_week, params)
        }
        _ => None,
    };

    Ok(DeviceHealth {
        source_id: source.id,
        name: source.name.clone(),
        kind: source.kind,
        newest: newest.map(|(ts, _)| ts),
        stale,
        battery,
        rssi,
        battery_7d,
        guess,
    })
}

fn week_to(end: Timestamp) -> SeriesRange {
    SeriesRange {
        start: end.saturating_add_secs(-HEALTH_WINDOW_SECS),
        end: end.saturating_add_secs(1),
    }
}

fn latest(conn: &Connection, stream_id: i64, now: Timestamp) -> Result<Option<(Timestamp, f64)>> {
    Ok(conn
        .query_row(
            "SELECT ts, value FROM reading WHERE stream_id = ? AND ts <= ? \
             ORDER BY ts DESC LIMIT 1",
            params![stream_id, now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn values(conn: &Connection, stream_id: i64, range: SeriesRange) -> Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT value FROM reading WHERE stream_id = ? AND ts >= ? AND ts < ? ORDER BY ts",
    )?;
    let rows = stmt.query_map(params![stream_id, range.start, range.end], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, create_batch},
        recipe::BeverageType,
        tier1::{create_source, create_stream, insert_readings},
        timeline::{bind_stream, unbind_stream},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const MIN: i64 = 60;
    const DAY: i64 = 86_400;
    const P: HealthParams = HealthParams {
        battery_warn_v: 3.6,
        rssi_warn_dbm: -80.0,
    };

    fn at(offset_secs: i64) -> Timestamp {
        Timestamp::from_secs(T0.as_secs() + offset_secs)
    }

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    /// A stream of `source_id` with `f(offset)` every `interval` over `[start, end)`.
    fn stream(
        conn: &mut Connection,
        source_id: i64,
        metric: Metric,
        (start, end, interval): (i64, i64, i64),
        f: impl Fn(i64) -> f64,
    ) -> i64 {
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, metric.as_str(), metric).unwrap();
        let rows: Vec<_> = (start..end)
            .step_by(interval as usize)
            .map(|t| (stream_id, at(t), f(t)))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        stream_id
    }

    /// An iSpindle reporting every 15 min for `days`, battery and RSSI from `battery(t)`
    /// and `rssi(t)`.
    fn ispindel(
        conn: &mut Connection,
        days: i64,
        battery: impl Fn(i64) -> f64,
        rssi: impl Fn(i64) -> f64,
    ) -> i64 {
        let source = create_source(conn, SourceKind::BrewdJournal, "ispindel", None).unwrap();
        let span = (0, days * DAY, 15 * MIN);
        stream(conn, source, Metric::Sg, span, |_| 1.010);
        stream(conn, source, Metric::BatteryV, span, battery);
        stream(conn, source, Metric::RssiDbm, span, rssi);
        source
    }

    fn only(conn: &Connection, now: i64) -> DeviceHealth {
        let mut all = all_device_health(conn, &P, at(now)).unwrap();
        assert_eq!(all.len(), 1);
        all.pop().unwrap()
    }

    #[test]
    fn a_reporting_device_is_fresh_with_its_levels_and_no_guess() {
        let mut conn = db();
        ispindel(&mut conn, 3, |_| 4.05, |_| -65.0);
        let last = 3 * DAY - 15 * MIN;

        let h = only(&conn, last + 10 * MIN);
        assert_eq!(h.newest, Some(at(last)));
        assert!(!h.stale);
        assert_eq!(h.battery.unwrap().value, 4.05);
        assert!(!h.battery.unwrap().warn);
        assert_eq!(h.rssi.unwrap().value, -65.0);
        assert_eq!(h.guess, None);
    }

    #[test]
    fn a_silent_device_near_its_battery_threshold_guesses_the_battery() {
        let mut conn = db();
        ispindel(
            &mut conn,
            3,
            |t| 4.1 - 0.45 * t as f64 / (3 * DAY) as f64,
            |_| -65.0,
        );

        let h = only(&conn, 4 * DAY);
        assert!(h.stale);
        assert!(h.battery.unwrap().value < 3.7);
        assert_eq!(h.guess, Some(SilenceGuess::Battery));
    }

    #[test]
    fn a_silent_device_with_a_falling_signal_guesses_the_signal() {
        let mut conn = db();
        let fell = 3 * DAY - 6 * 3_600;
        ispindel(
            &mut conn,
            3,
            |_| 4.0,
            |t| if t < fell { -62.0 } else { -76.0 },
        );

        let h = only(&conn, 4 * DAY);
        assert!(h.stale);
        assert!(!h.rssi.unwrap().warn);
        assert_eq!(h.guess, Some(SilenceGuess::Signal));
    }

    #[test]
    fn a_silent_device_with_nothing_wrong_makes_no_guess() {
        let mut conn = db();
        ispindel(&mut conn, 3, |_| 4.0, |_| -65.0);
        let h = only(&conn, 4 * DAY);
        assert!(h.stale);
        assert_eq!(h.guess, None);
    }

    #[test]
    fn the_battery_sparkline_is_a_week_at_most_and_capped() {
        let mut conn = db();
        ispindel(&mut conn, 20, |_| 4.0, |_| -65.0);
        let now = 20 * DAY;

        let h = only(&conn, now);
        assert!(!h.battery_7d.is_empty());
        assert!(h.battery_7d.len() <= SPARKLINE_POINTS);
        assert!(h.battery_7d[0].0 >= at(now - HEALTH_WINDOW_SECS));
    }

    #[test]
    fn manual_sources_are_not_devices() {
        let mut conn = db();
        let manual = create_source(&conn, SourceKind::Manual, "manual", None).unwrap();
        stream(&mut conn, manual, Metric::Ph, (0, DAY, DAY / 2), |_| 3.4);
        assert!(all_device_health(&conn, &P, at(DAY)).unwrap().is_empty());
    }

    #[test]
    fn a_batch_card_shows_devices_bound_now_that_report_battery_or_signal() {
        let mut conn = db();
        let batch_id = create_batch(
            &conn,
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
        .unwrap();
        let ispindel = ispindel(&mut conn, 2, |_| 4.0, |_| -65.0);
        let esp32 = create_source(&conn, SourceKind::BrewdJournal, "esp32", None).unwrap();
        let temp = stream(&mut conn, esp32, Metric::TempC, (0, 2 * DAY, 30), |_| 18.0);
        let old = create_source(&conn, SourceKind::BrewdJournal, "old ispindel", None).unwrap();
        let old_battery = stream(&mut conn, old, Metric::BatteryV, (0, DAY, 900), |_| 3.9);

        let sg = stream_ids_for(&conn, ispindel).unwrap()[&Metric::Sg];
        bind_stream(&conn, batch_id, sg, T0, None).unwrap();
        bind_stream(&conn, batch_id, temp, T0, None).unwrap();
        let binding = bind_stream(&conn, batch_id, old_battery, T0, None).unwrap();
        unbind_stream(&conn, binding, at(DAY)).unwrap();

        let devices = batch_device_health(&conn, batch_id, &P, at(2 * DAY)).unwrap();
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["ispindel"]);
    }
}
