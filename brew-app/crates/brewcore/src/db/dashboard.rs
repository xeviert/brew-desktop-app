//! Phase 4e. The batch card: OG, current gravity, estimated ABV, pH, temperature and
//! whether it has finished — one struct, so Phase 6's alert and the screen read the
//! same numbers.
//!
//! **OG is `batch.og` and nothing else.** The first iSpindle point reads the lag phase,
//! or a hydrometer left on the wall, as OG; an inferred OG looks right and is wrong.
//! With none, the card offers the earliest gravity as a candidate, and setting it stays
//! the deliberate act [`set_batch_measurements`](super::batch::set_batch_measurements)
//! documents.
//!
//! **Every number says where it came from and how old it is.** A hydrometer from
//! yesterday and an iSpindle from two minutes ago are different claims.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use std::cmp::Reverse;

use rusqlite::Connection;

use super::{
    Metric,
    batch::get_batch,
    health::{DeviceHealth, batch_device_health},
    measurement::{Instrument, MeasurementRow, list_measurements_for_metric},
    series::{Latest, Series, latest_readings, stale_after},
    stability::judge,
    tier1::SourceKind,
};
use crate::{
    Result,
    calc::{
        abv, attenuation,
        gravity::sg_to_display,
        health::HealthParams,
        stability::{ExpectedFg, LAG_DROP, Stability, StabilityParams},
    },
    units::{GravityDisplay, Sg, Timestamp},
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchDashboard {
    pub batch_id: i64,
    /// `batch.og`. Never inferred.
    pub og: Option<Sg>,
    /// The earliest gravity, offered as the OG while there is none: a manual reading,
    /// else the judged sensor stream's first since pitch. `None` once `og` is set.
    pub og_candidate: Option<Gravity>,
    /// The judged sensor stream's newest smoothed value, else the newest manual reading.
    pub gravity: Option<Gravity>,
    pub estimate: Estimate,
    pub ph: Option<Reading>,
    /// Beer temperature: a sensor `temp_c`, else the iSpindle's own, else a manual one.
    /// Fridge and room air are not the beer.
    pub temp: Option<Reading>,
    /// 4c's detector, with its `since`.
    pub stability: Stability,
    pub expected_fg: Option<ExpectedFg>,
    /// Every source bound to the batch, for matching a `sync://status` event.
    pub source_ids: Vec<i64>,
    /// 4f's health of the bound devices that report a battery or a signal.
    pub devices: Vec<DeviceHealth>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Gravity {
    pub ts: Timestamp,
    pub sg: Sg,
    /// 4b's rolling median rather than a reading.
    pub smoothed: bool,
    pub source_kind: SourceKind,
    pub label: String,
    /// Manual readings only, and only when one was recorded.
    pub instrument: Option<Instrument>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Reading {
    pub metric: Metric,
    pub ts: Timestamp,
    pub value: f64,
    pub source_kind: SourceKind,
    pub label: String,
    /// Past the stream's [`stale_after`]. Never for a manual reading: a pH days old is
    /// normal.
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Estimate {
    /// Fractions. Hall's formula, named so no caller can show it unlabelled.
    Estimated {
        abv_alternate: f64,
        apparent_attenuation: f64,
    },
    NoOg,
    NoGravity,
    /// Within [`LAG_DROP`] of the OG: the lag phase, where an estimate reads 0.1 %.
    NotStarted,
    /// An OG within two points of water — `calc::attenuation`'s refusal.
    OgTooLow,
}

/// The card's gravities in `ui.gravity_display`, beside the SG ones. The SG stays: it
/// is what "use this as the OG" writes back.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct DisplayGravity {
    pub og: Option<f64>,
    pub og_candidate: Option<f64>,
    pub gravity: Option<f64>,
    pub expected_fg_lo: Option<f64>,
    pub expected_fg_hi: Option<f64>,
}

impl BatchDashboard {
    pub fn gravity_in(&self, display: GravityDisplay) -> DisplayGravity {
        let fit = |sg: Sg| sg_to_display(sg, display);
        DisplayGravity {
            og: self.og.map(fit),
            og_candidate: self.og_candidate.as_ref().map(|g| fit(g.sg)),
            gravity: self.gravity.as_ref().map(|g| fit(g.sg)),
            expected_fg_lo: self.expected_fg.map(|e| fit(e.lo)),
            expected_fg_hi: self.expected_fg.map(|e| fit(e.hi)),
        }
    }
}

pub fn batch_dashboard(
    conn: &Connection,
    batch_id: i64,
    params: &StabilityParams,
    health: &HealthParams,
) -> Result<BatchDashboard> {
    dashboard_at(conn, batch_id, params, health, Timestamp::now())
}

pub(crate) fn dashboard_at(
    conn: &Connection,
    batch_id: i64,
    params: &StabilityParams,
    health: &HealthParams,
    now: Timestamp,
) -> Result<BatchDashboard> {
    let batch = get_batch(conn, batch_id)?;
    let (judged, sensor) = judge(conn, &batch, params)?;
    let manual = list_measurements_for_metric(conn, batch_id, Metric::Sg)?;

    let gravity = match &sensor {
        Some(s) => newest_smoothed(s)?,
        None => None,
    }
    .or_else(|| manual.last().map(manual_gravity));
    let og_candidate = match (batch.og, manual.first(), &sensor) {
        (Some(_), ..) => None,
        (None, Some(m), _) => Some(manual_gravity(m)),
        (None, None, Some(s)) => first_raw(s)?,
        (None, None, None) => None,
    };

    let latest = latest_readings(
        conn,
        batch_id,
        &[Metric::Ph, Metric::TempC, Metric::GravityTempC],
        now,
    )?;
    let ph = latest
        .iter()
        .filter(|l| l.metric == Metric::Ph)
        .max_by_key(|l| l.ts);
    let temp = latest
        .iter()
        .filter_map(|l| {
            let rank = match (l.metric, l.source_kind == SourceKind::Manual) {
                (Metric::TempC, false) => 0,
                (Metric::GravityTempC, false) => 1,
                (Metric::TempC, true) => 2,
                _ => return None,
            };
            Some(((rank, Reverse(l.ts)), l))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, l)| l);
    let reading = |l: &Latest| -> Result<Reading> {
        let stale = l.source_kind != SourceKind::Manual
            && l.ts.secs_until(now) > stale_after(conn, l.stream_id, l.ts)?;
        Ok(Reading {
            metric: l.metric,
            ts: l.ts,
            value: l.value,
            source_kind: l.source_kind,
            label: l.label.clone(),
            stale,
        })
    };

    Ok(BatchDashboard {
        batch_id,
        og: batch.og,
        og_candidate,
        estimate: estimate(batch.og, gravity.as_ref()),
        gravity,
        ph: ph.map(reading).transpose()?,
        temp: temp.map(reading).transpose()?,
        stability: judged.current,
        expected_fg: judged.expected_fg,
        source_ids: bound_sources(conn, batch_id)?,
        devices: batch_device_health(conn, batch_id, health, now)?,
    })
}

fn newest_smoothed(s: &Series) -> Result<Option<Gravity>> {
    let Some((ts, value)) =
        s.ts.iter()
            .zip(&s.smoothed)
            .rev()
            .find_map(|(&ts, v)| Some((ts, (*v)?)))
    else {
        return Ok(None);
    };
    Ok(Some(sensor_gravity(s, ts, Sg::new(value)?, true)))
}

fn first_raw(s: &Series) -> Result<Option<Gravity>> {
    match (s.ts.first(), s.avg.first()) {
        (Some(&ts), Some(&Some(value))) => Ok(Some(sensor_gravity(s, ts, Sg::new(value)?, false))),
        _ => Ok(None),
    }
}

fn sensor_gravity(s: &Series, ts: Timestamp, sg: Sg, smoothed: bool) -> Gravity {
    Gravity {
        ts,
        sg,
        smoothed,
        source_kind: s.source_kind,
        label: s.label.clone(),
        instrument: None,
    }
}

fn manual_gravity(m: &MeasurementRow) -> Gravity {
    Gravity {
        ts: m.ts,
        sg: Sg::new_unchecked(m.value),
        smoothed: false,
        source_kind: SourceKind::Manual,
        label: "manual".to_owned(),
        instrument: m.instrument,
    }
}

/// Refused below `OG − LAG_DROP`, so the lag phase doesn't read 0.1 %.
fn estimate(og: Option<Sg>, gravity: Option<&Gravity>) -> Estimate {
    let Some(og) = og else {
        return Estimate::NoOg;
    };
    let Some(g) = gravity else {
        return Estimate::NoGravity;
    };
    if g.sg.get() > og.get() - LAG_DROP {
        return Estimate::NotStarted;
    }
    match (abv::alternate(og, g.sg), attenuation::apparent(og, g.sg)) {
        (Ok(abv_alternate), Ok(apparent_attenuation)) => Estimate::Estimated {
            abv_alternate,
            apparent_attenuation,
        },
        _ => Estimate::OgTooLow,
    }
}

fn bound_sources(conn: &Connection, batch_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.source_id FROM batch_stream bs \
           JOIN stream s ON s.id = bs.stream_id \
          WHERE bs.batch_id = ? ORDER BY s.source_id",
    )?;
    let rows = stmt.query_map([batch_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calc::{
            gravity::sg_to_plato,
            stability::{FgSource, Insufficient},
        },
        db::{
            batch::{NewBatch, create_batch, set_pitched_at},
            measurement::{EntryUnit, NewMeasurement, record_measurement},
            recipe::{BeverageType, NewRecipeVersion, create_recipe, create_recipe_version},
            tier1::{create_source, create_stream, insert_readings},
            timeline::bind_stream,
        },
    };
    use brewfixture::curve::gravity_at;

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;

    fn at(offset_secs: i64) -> Timestamp {
        Timestamp::from_secs(T0.as_secs() + offset_secs)
    }

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }

    fn batch(conn: &Connection, code: &str, version: Option<i64>, og: Option<f64>) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: version,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: og.map(sg),
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    fn pitched(conn: &Connection, version: Option<i64>, og: Option<f64>) -> i64 {
        let batch_id = batch(conn, "SELTZER-001", version, og);
        set_pitched_at(conn, batch_id, T0).unwrap();
        batch_id
    }

    fn recipe(conn: &Connection, target_fg: f64) -> i64 {
        let recipe = create_recipe(conn, "Citron-Verveine", BeverageType::Seltzer, None).unwrap();
        create_recipe_version(
            conn,
            recipe,
            &NewRecipeVersion {
                target_fg: Some(sg(target_fg)),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn source(conn: &Connection, name: &str) -> i64 {
        create_source(conn, SourceKind::BrewdJournal, name, None).unwrap()
    }

    /// A stream of `source_id` with `f(hours)` every `interval` over `[start, end)`.
    fn stream(
        conn: &mut Connection,
        source_id: i64,
        metric: Metric,
        (start, end, interval): (i64, i64, i64),
        f: impl Fn(f64) -> f64,
    ) -> i64 {
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, metric.as_str(), metric).unwrap();
        let rows: Vec<_> = (start..end)
            .step_by(interval as usize)
            .map(|t| (stream_id, at(t), f(t as f64 / HOUR as f64)))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        stream_id
    }

    /// [`stream`], bound to `batch_id` from `T0` on.
    fn bound(
        conn: &mut Connection,
        batch_id: i64,
        source_id: i64,
        metric: Metric,
        span: (i64, i64, i64),
        f: impl Fn(f64) -> f64,
    ) -> i64 {
        let stream_id = stream(conn, source_id, metric, span, f);
        bind_stream(conn, batch_id, stream_id, T0, None).unwrap();
        stream_id
    }

    fn measure(
        conn: &mut Connection,
        batch_id: i64,
        offset: i64,
        (metric, entry_value, entry_unit): (Metric, f64, EntryUnit),
        instrument: Instrument,
    ) {
        let tx = conn.transaction().unwrap();
        record_measurement(
            &tx,
            batch_id,
            at(offset),
            &NewMeasurement {
                metric,
                entry_value,
                entry_unit,
                instrument: Some(instrument),
                sample_temp_c: None,
                calibration_temp_c: None,
                wort_correction: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    fn hydrometer(value: f64) -> (Metric, f64, EntryUnit) {
        (Metric::Sg, value, EntryUnit::Sg)
    }

    fn dashboard(conn: &Connection, batch_id: i64, now: i64) -> BatchDashboard {
        dashboard_at(
            conn,
            batch_id,
            &StabilityParams::default(),
            &HealthParams::default(),
            at(now),
        )
        .unwrap()
    }

    #[test]
    fn a_finished_batch_fills_every_field() {
        let mut conn = db();
        let version = recipe(&conn, 1.000);
        let batch_id = pitched(&conn, Some(version), Some(1.045));
        let ispindel = source(&conn, "ispindel");
        let esp32 = source(&conn, "esp32");
        let span = (0, 12 * DAY, 15 * 60);
        bound(&mut conn, batch_id, ispindel, Metric::Sg, span, gravity_at);
        bound(
            &mut conn,
            batch_id,
            ispindel,
            Metric::GravityTempC,
            span,
            |_| 20.0,
        );
        bound(
            &mut conn,
            batch_id,
            esp32,
            Metric::TempC,
            (0, 12 * DAY, 60),
            |_| 18.0,
        );
        let ph = |v| (Metric::Ph, v, EntryUnit::Ph);
        measure(&mut conn, batch_id, 3 * DAY, ph(3.4), Instrument::PhMeter);
        measure(&mut conn, batch_id, 10 * DAY, ph(3.1), Instrument::PhMeter);

        let d = dashboard(&conn, batch_id, 12 * DAY + 60);
        assert_eq!(d.og, Some(sg(1.045)));
        assert_eq!(d.og_candidate, None);

        let g = d.gravity.clone().unwrap();
        assert!(g.smoothed);
        assert_eq!(g.source_kind, SourceKind::BrewdJournal);
        assert_eq!(g.label, "ispindel");
        assert_eq!(g.ts, at(12 * DAY - 15 * 60));
        assert!((g.sg.get() - 1.000).abs() < 0.0005, "{g:?}");

        let Estimate::Estimated {
            abv_alternate,
            apparent_attenuation,
        } = d.estimate
        else {
            panic!("{:?}", d.estimate);
        };
        assert_eq!(abv_alternate, abv::alternate(sg(1.045), g.sg).unwrap());
        assert!((apparent_attenuation - 1.0).abs() < 0.02);

        let ph = d.ph.clone().unwrap();
        assert_eq!((ph.value, ph.ts, ph.stale), (3.1, at(10 * DAY), false));
        let temp = d.temp.clone().unwrap();
        assert_eq!((temp.metric, temp.value), (Metric::TempC, 18.0));
        assert!(!temp.stale);

        assert!(
            matches!(
                d.stability,
                Stability::Stable {
                    fg_unchecked: false,
                    ..
                }
            ),
            "{:?}",
            d.stability
        );
        assert_eq!(d.expected_fg.unwrap().source, FgSource::Recipe);
        assert_eq!(d.source_ids.len(), 3);

        let plato = d.gravity_in(GravityDisplay::Plato);
        assert_eq!(plato.og, Some(sg_to_plato(sg(1.045))));
        assert_eq!(plato.expected_fg_hi, Some(sg_to_plato(sg(1.000))));
    }

    /// With no OG the card offers the earliest gravity and estimates nothing — even
    /// with a sensor that has read 1.045 since pitch.
    #[test]
    fn no_og_offers_the_earliest_manual_gravity_and_no_estimate() {
        let mut conn = db();
        let batch_id = pitched(&conn, None, None);
        let ispindel = source(&conn, "ispindel");
        let span = (0, 12 * DAY, 15 * 60);
        bound(&mut conn, batch_id, ispindel, Metric::Sg, span, gravity_at);
        measure(
            &mut conn,
            batch_id,
            5 * DAY,
            hydrometer(1.010),
            Instrument::Hydrometer,
        );
        measure(
            &mut conn,
            batch_id,
            -HOUR,
            hydrometer(1.046),
            Instrument::Hydrometer,
        );

        let d = dashboard(&conn, batch_id, 12 * DAY);
        assert_eq!(d.og, None);
        let candidate = d.og_candidate.unwrap();
        assert_eq!((candidate.sg, candidate.ts), (sg(1.046), at(-HOUR)));
        assert_eq!(candidate.instrument, Some(Instrument::Hydrometer));
        assert!(!candidate.smoothed);
        assert_eq!(d.estimate, Estimate::NoOg);
        assert!(
            d.gravity.unwrap().smoothed,
            "the sensor wins over the hydrometer"
        );

        // No recipe and no OG: nothing to call a stall against.
        assert_eq!(d.expected_fg, None);
        assert!(
            matches!(
                d.stability,
                Stability::Stable {
                    fg_unchecked: true,
                    ..
                }
            ),
            "{:?}",
            d.stability
        );
    }

    #[test]
    fn with_no_manual_gravity_the_candidate_is_the_first_raw_sensor_reading() {
        let mut conn = db();
        let batch_id = pitched(&conn, None, None);
        let ispindel = source(&conn, "ispindel");
        let span = (0, 2 * DAY, 15 * 60);
        bound(&mut conn, batch_id, ispindel, Metric::Sg, span, gravity_at);

        let candidate = dashboard(&conn, batch_id, 2 * DAY).og_candidate.unwrap();
        assert_eq!((candidate.ts, candidate.sg), (T0, sg(1.045)));
        assert!(!candidate.smoothed);
        assert_eq!(candidate.instrument, None);
    }

    #[test]
    fn the_lag_phase_is_not_estimated() {
        let mut conn = db();
        let batch_id = pitched(&conn, None, Some(1.045));
        let ispindel = source(&conn, "ispindel");
        let span = (0, 2 * DAY, 15 * 60);
        bound(&mut conn, batch_id, ispindel, Metric::Sg, span, |_| 1.0435);

        let d = dashboard(&conn, batch_id, 2 * DAY);
        assert!((d.gravity.unwrap().sg.get() - 1.0435).abs() < 1e-9);
        assert_eq!(d.estimate, Estimate::NotStarted);
        assert_eq!(
            d.stability,
            Stability::Insufficient {
                reason: Insufficient::NotStarted
            }
        );
    }

    #[test]
    fn a_batch_with_no_bindings_is_empty_not_an_error() {
        let conn = db();
        let with_og = batch(&conn, "SELTZER-001", None, Some(1.045));
        let d = dashboard(&conn, with_og, DAY);
        assert_eq!(d.og_candidate, None);
        assert_eq!(d.gravity, None);
        assert_eq!(d.estimate, Estimate::NoGravity);
        assert_eq!((d.ph, d.temp), (None, None));
        assert_eq!(
            d.stability,
            Stability::Insufficient {
                reason: Insufficient::Empty
            }
        );
        assert!(d.source_ids.is_empty());

        let without = batch(&conn, "SELTZER-002", None, None);
        let d = dashboard(&conn, without, DAY);
        assert_eq!((d.og_candidate, d.estimate), (None, Estimate::NoOg));
    }

    #[test]
    fn a_manual_only_batch_reads_its_hydrometer_and_never_goes_stale() {
        let mut conn = db();
        let batch_id = pitched(&conn, None, Some(1.045));
        measure(
            &mut conn,
            batch_id,
            DAY,
            hydrometer(1.030),
            Instrument::Hydrometer,
        );
        measure(
            &mut conn,
            batch_id,
            4 * DAY,
            hydrometer(1.005),
            Instrument::Hydrometer,
        );
        let temp = (Metric::TempC, 18.0, EntryUnit::Celsius);
        measure(&mut conn, batch_id, 4 * DAY, temp, Instrument::Thermometer);

        let d = dashboard(&conn, batch_id, 30 * DAY);
        let g = d.gravity.unwrap();
        assert_eq!((g.sg, g.ts), (sg(1.005), at(4 * DAY)));
        assert_eq!(g.source_kind, SourceKind::Manual);
        assert_eq!(g.instrument, Some(Instrument::Hydrometer));
        assert!(!g.smoothed);
        assert!(matches!(d.estimate, Estimate::Estimated { .. }));
        assert_eq!(
            d.stability,
            Stability::Insufficient {
                reason: Insufficient::Empty
            }
        );
        let temp = d.temp.unwrap();
        assert_eq!((temp.source_kind, temp.stale), (SourceKind::Manual, false));
    }

    /// Stale is relative to the stream's own interval: twenty minutes is forty missed
    /// readings from a 30 s probe and not one from a 15 min iSpindle.
    #[test]
    fn staleness_follows_the_stream_interval() {
        let mut conn = db();
        let fast = pitched(&conn, None, None);
        let esp32 = source(&conn, "esp32");
        bound(&mut conn, fast, esp32, Metric::TempC, (0, DAY, 30), |_| {
            18.0
        });
        let last = DAY - 30;
        assert!(dashboard(&conn, fast, last + 20 * 60).temp.unwrap().stale);
        assert!(!dashboard(&conn, fast, last + 5 * 60).temp.unwrap().stale);

        let slow = batch(&conn, "SELTZER-002", None, None);
        let ispindel = source(&conn, "ispindel");
        let span = (0, DAY, 15 * 60);
        bound(
            &mut conn,
            slow,
            ispindel,
            Metric::GravityTempC,
            span,
            |_| 20.0,
        );
        let last = DAY - 15 * 60;
        assert!(!dashboard(&conn, slow, last + 20 * 60).temp.unwrap().stale);
        assert!(dashboard(&conn, slow, last + 50 * 60).temp.unwrap().stale);
    }

    /// Batches 17 and 18 share a fermenter probe: 17's newest temperature is the last
    /// one before its binding ended, not 18's cold crash.
    #[test]
    fn a_shared_stream_after_the_binding_ended_is_the_next_batch() {
        let mut conn = db();
        let b17 = batch(&conn, "SELTZER-017", None, None);
        let b18 = batch(&conn, "SELTZER-018", None, None);
        let esp32 = source(&conn, "esp32");
        let probe = stream(&mut conn, esp32, Metric::TempC, (0, 10 * DAY, 60), |h| {
            if h < 5.0 * 24.0 { 18.0 } else { 2.0 }
        });
        bind_stream(&conn, b17, probe, T0, Some(at(5 * DAY))).unwrap();
        bind_stream(&conn, b18, probe, at(5 * DAY), None).unwrap();

        let t17 = dashboard(&conn, b17, 10 * DAY).temp.unwrap();
        assert_eq!((t17.ts, t17.value), (at(5 * DAY - 60), 18.0));
        let t18 = dashboard(&conn, b18, 10 * DAY).temp.unwrap();
        assert_eq!((t18.ts, t18.value), (at(10 * DAY - 60), 2.0));
    }
}
