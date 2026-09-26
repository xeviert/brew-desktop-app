//! Phase 5a's joins: one [`BatchMetrics`] per batch, from 4a's series, 4c's history,
//! 3f's cost and the batch's actual additions. The arithmetic is
//! [`calc::batch_metrics`](crate::calc::batch_metrics); nothing is stored.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::Connection;

use super::{
    Metric,
    batch::{BatchRow, BatchStatus, batch_cost, get_batch, list_batches},
    measurement::list_measurements_for_metric,
    recipe::BeverageType,
    reference::{Category, mg_yan_per_g},
    series::{Series, SeriesRange, batch_series},
    stability::{judge, pitched_strains},
    tier1::SourceKind,
};
use crate::{
    Error, Result,
    calc::{
        batch_metrics::{
            Computed, Lag, Missing, PEAK_WINDOW_DEFAULT, PeakRate, Spread, Terminal, lag,
            peak_rate, recorded, spread, terminal, yan_added,
        },
        stability::StabilityParams,
    },
    units::{Grams, Money, Sg, Timestamp},
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchMetrics {
    pub batch_id: i64,
    pub code: String,
    pub beverage_type: BeverageType,
    pub status: BatchStatus,
    pub archived_at: Option<Timestamp>,
    pub pitched_at: Option<Timestamp>,
    /// `batch.og` / `batch.fg`, as recorded.
    pub og: Option<Sg>,
    pub fg: Option<Sg>,
    pub lag: Computed<Lag>,
    pub peak_rate: Computed<PeakRate>,
    pub terminal: Computed<Terminal>,
    /// From the recorded OG and FG, not the live gravity.
    pub apparent_attenuation: Computed<f64>,
    /// Hall's formula, named so no caller can show it unlabelled.
    pub abv_alternate: Computed<f64>,
    pub ferm_temp: Computed<FermTemp>,
    pub yan_added_mg_per_l: Computed<f64>,
    pub cost_per_litre: Computed<Money>,
    /// Additions from an unpriced lot, left out of the cost rather than counted as free.
    pub cost_lines_without_cost: usize,
    /// From the additions, never the recipe: a substituted yeast is the case the
    /// comparison is for. Empty when no yeast addition was recorded.
    pub strains: Vec<StrainRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct FermTemp {
    #[serde(flatten)]
    pub spread: Spread,
    pub stream_id: i64,
    /// Over `[pitch, terminal)`. `false` means up to now, for a batch never stable.
    pub to_terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StrainRef {
    pub id: i64,
    pub name: String,
}

pub fn batch_metrics(
    conn: &Connection,
    batch_id: i64,
    params: &StabilityParams,
) -> Result<BatchMetrics> {
    metrics_for(conn, &get_batch(conn, batch_id)?, params, Timestamp::now())
}

/// [`batch_metrics`] for every batch, archived ones too if asked: finished batches are
/// most of what gets compared.
pub fn list_batch_metrics(
    conn: &Connection,
    include_archived: bool,
    params: &StabilityParams,
) -> Result<Vec<BatchMetrics>> {
    let now = Timestamp::now();
    list_batches(conn, include_archived)?
        .iter()
        .map(|batch| metrics_for(conn, batch, params, now))
        .collect()
}

fn metrics_for(
    conn: &Connection,
    batch: &BatchRow,
    params: &StabilityParams,
    now: Timestamp,
) -> Result<BatchMetrics> {
    let curve = curve(conn, batch, params)?;
    let (apparent_attenuation, abv_alternate) = recorded(batch.og, batch.fg);

    let ferm_temp = match (batch.pitched_at, curve.terminal) {
        (None, _) => Computed::Missing(Missing::NoPitch),
        (Some(pitch), Computed::Value(t)) => ferm_temp(conn, batch.id, pitch, t.at, true)?,
        (Some(pitch), Computed::Missing(_)) => ferm_temp(conn, batch.id, pitch, now, false)?,
    };

    let (cost_per_litre, cost_lines_without_cost, volume_l) = match batch_cost(conn, batch.id) {
        Ok(cost) => {
            let per_litre = match (cost.per_litre, cost.total) {
                (Some(m), _) => Computed::Value(m),
                (None, None) => Computed::Missing(Missing::NoCost),
                (None, Some(_)) => Computed::Missing(Missing::NoVolume),
            };
            (per_litre, cost.lines_without_cost, cost.volume_l)
        }
        // One mistyped currency shouldn't take the whole table down with it.
        Err(Error::CurrencyMismatch { .. }) => {
            let volume_l = super::batch::target_or_batch_volume(conn, batch)?;
            (Computed::Missing(Missing::MixedCurrency), 0, volume_l)
        }
        Err(e) => return Err(e),
    };

    Ok(BatchMetrics {
        batch_id: batch.id,
        code: batch.code.clone(),
        beverage_type: batch.beverage_type,
        status: batch.status,
        archived_at: batch.archived_at,
        pitched_at: batch.pitched_at,
        og: batch.og,
        fg: batch.fg,
        lag: curve.lag,
        peak_rate: curve.peak_rate,
        terminal: curve.terminal,
        apparent_attenuation,
        abv_alternate,
        ferm_temp,
        yan_added_mg_per_l: yan_added(&nutrient_lines(conn, batch.id)?, volume_l)?,
        cost_per_litre,
        cost_lines_without_cost,
        strains: pitched_strains(conn, batch.id)?
            .into_iter()
            .map(|s| StrainRef {
                id: s.id,
                name: s.name,
            })
            .collect(),
    })
}

struct Curve {
    lag: Computed<Lag>,
    peak_rate: Computed<PeakRate>,
    terminal: Computed<Terminal>,
}

/// The three metrics read off the gravity curve, from the stream 4c judges — the one
/// the batch card shows.
fn curve(conn: &Connection, batch: &BatchRow, params: &StabilityParams) -> Result<Curve> {
    let all = |m| Curve {
        lag: Computed::Missing(m),
        peak_rate: Computed::Missing(m),
        terminal: Computed::Missing(m),
    };
    let Some(pitch) = batch.pitched_at else {
        return Ok(all(Missing::NoPitch));
    };
    let (judged, sensor) = judge(conn, batch, params)?;
    let Some(sensor) = sensor else {
        let manual = !list_measurements_for_metric(conn, batch.id, Metric::Sg)?.is_empty();
        return Ok(all(if manual {
            Missing::ManualOnly
        } else {
            Missing::NoSensorGravity
        }));
    };

    let points: Vec<_> = sensor
        .ts
        .iter()
        .zip(&sensor.smoothed)
        .filter_map(|(&ts, value)| Some((ts, (*value)?)))
        .collect();
    Ok(Curve {
        lag: match batch.og {
            Some(og) => lag(&points, pitch, og),
            None => Computed::Missing(Missing::NoOg),
        },
        peak_rate: peak_rate(&points, pitch, PEAK_WINDOW_DEFAULT)?,
        terminal: terminal(&judged.history, pitch),
    })
}

/// Median beer temperature over `[pitch, end)`: a sensor `temp_c`, else the
/// iSpindle's own, never a manual one — a handful of spot checks isn't a ferment
/// temperature. Of several streams of one kind, the one with the most readings.
fn ferm_temp(
    conn: &Connection,
    batch_id: i64,
    pitch: Timestamp,
    end: Timestamp,
    to_terminal: bool,
) -> Result<Computed<FermTemp>> {
    let series = batch_series(
        conn,
        batch_id,
        &[Metric::TempC, Metric::GravityTempC],
        Some(SeriesRange { start: pitch, end }),
        usize::MAX,
    )?;
    let best = ferm_temp_series(&series.series).and_then(|s| {
        let values: Vec<f64> = s.avg.iter().flatten().copied().collect();
        Some((s.stream_id, spread(&values)?))
    });
    Ok(match best {
        Some((stream_id, spread)) => Computed::Value(FermTemp {
            spread,
            stream_id,
            to_terminal,
        }),
        None => Computed::Missing(Missing::NoTemp),
    })
}

/// [`ferm_temp`]'s choice of stream among `temp_c` and `gravity_temp_c` series, shared
/// with 5b's overlay so the curve drawn is the one the median was taken over.
pub(crate) fn ferm_temp_series(series: &[Series]) -> Option<&Series> {
    series
        .iter()
        .filter(|s| s.source_kind != SourceKind::Manual)
        .filter(|s| matches!(s.metric, Metric::TempC | Metric::GravityTempC))
        .filter_map(|s| {
            let rank = match s.metric {
                Metric::TempC => 0,
                _ => 1,
            };
            let n = s.avg.iter().flatten().filter(|v| v.is_finite()).count();
            (n > 0).then_some(((rank, std::cmp::Reverse(n)), s))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, s)| s)
}

/// Each nutrient addition's grams and its product's nitrogen yield.
fn nutrient_lines(conn: &Connection, batch_id: i64) -> Result<Vec<(Option<Grams>, Option<f64>)>> {
    let mut stmt = conn.prepare(
        "SELECT bi.qty_g, i.attrs \
           FROM batch_ingredient bi \
           JOIN ingredient_lot l ON l.id = bi.ingredient_lot_id \
           JOIN ingredient i ON i.id = l.ingredient_id \
          WHERE bi.batch_id = ? AND i.category = ?",
    )?;
    let rows = stmt.query_map(rusqlite::params![batch_id, Category::Nutrient], |row| {
        let grams: Option<Grams> = row.get(0)?;
        let attrs: Option<String> = row.get(1)?;
        Ok((grams, mg_yan_per_g(attrs.as_deref())))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calc::{abv, stability::Stability, yan::DAP_MG_YAN_PER_G},
        db::{
            batch::{
                NewBatch, NewBatchIngredient, add_batch_ingredient, archive_batch, create_batch,
                set_batch_measurements, set_pitched_at,
            },
            measurement::{EntryUnit, Instrument, NewMeasurement, record_measurement},
            recipe::{
                NewRecipeItem, NewRecipeVersion, Stage, add_recipe_item, create_recipe,
                create_recipe_version,
            },
            reference::{
                NewIngredient, NewLot, NewYeastStrain, create_ingredient, create_lot,
                create_yeast_strain,
            },
            stability::batch_stability,
            tier1::{create_source, create_stream, insert_readings},
            timeline::bind_stream,
        },
        units::{Currency, Litres, Quantity, Unit},
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

    fn params() -> StabilityParams {
        StabilityParams::default()
    }

    fn value<T: std::fmt::Debug>(c: Computed<T>) -> T {
        match c {
            Computed::Value(v) => v,
            Computed::Missing(m) => panic!("missing: {m:?}"),
        }
    }

    fn missing<T>(c: &Computed<T>) -> Option<Missing> {
        match c {
            Computed::Value(_) => None,
            Computed::Missing(m) => Some(*m),
        }
    }

    /// 20 L, so the YAN and cost arithmetic has a volume.
    fn batch(conn: &Connection, code: &str, version: Option<i64>, og: Option<f64>) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: version,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: Some(Litres::new(20.0).unwrap()),
                og: og.map(sg),
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    fn recipe(conn: &Connection) -> i64 {
        let recipe = create_recipe(conn, "Citron-Verveine", BeverageType::Seltzer, None).unwrap();
        create_recipe_version(conn, recipe, &NewRecipeVersion::default()).unwrap()
    }

    /// An ingredient and one 1 kg lot of it.
    fn stocked(
        conn: &Connection,
        name: &str,
        category: Category,
        strain: Option<i64>,
        attrs: Option<&str>,
        cost: Option<Money>,
    ) -> (i64, i64) {
        let ingredient = create_ingredient(
            conn,
            &NewIngredient {
                name: name.into(),
                category,
                default_unit: Unit::G,
                yeast_strain_id: strain,
                attrs: attrs.map(str::to_owned),
                notes: None,
            },
        )
        .unwrap();
        let lot = create_lot(
            conn,
            &NewLot {
                ingredient_id: ingredient,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(1000.0, Unit::G).unwrap(),
                cost,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        (ingredient, lot)
    }

    fn yeast(conn: &Connection, name: &str) -> (i64, i64) {
        let strain = create_yeast_strain(
            conn,
            &NewYeastStrain {
                name: name.into(),
                ..Default::default()
            },
        )
        .unwrap();
        stocked(conn, name, Category::Yeast, Some(strain), None, None)
    }

    fn add(conn: &Connection, batch_id: i64, lot: i64, grams: f64, stage: Stage) {
        add_batch_ingredient(
            conn,
            batch_id,
            &NewBatchIngredient {
                ingredient_lot_id: lot,
                entry: Quantity::new(grams, Unit::G).unwrap(),
                stage,
                added_at: None,
                notes: None,
            },
        )
        .unwrap();
    }

    /// A sensor stream of `metric` bound from `T0`, with `f(hours)` every `interval`.
    fn stream(
        conn: &mut Connection,
        batch_id: i64,
        metric: Metric,
        name: &str,
        (end, interval): (i64, i64),
        f: impl Fn(f64) -> f64,
    ) -> i64 {
        let source_id = create_source(conn, SourceKind::BrewdJournal, name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, name, metric).unwrap();
        let rows: Vec<_> = (0..end)
            .step_by(interval as usize)
            .map(|t| (stream_id, at(t), f(t as f64 / HOUR as f64)))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        bind_stream(conn, batch_id, stream_id, T0, None).unwrap();
        stream_id
    }

    fn hydrometer(conn: &mut Connection, batch_id: i64, offset: i64, reading: f64) {
        let tx = conn.transaction().unwrap();
        record_measurement(
            &tx,
            batch_id,
            at(offset),
            &NewMeasurement {
                metric: Metric::Sg,
                entry_value: reading,
                entry_unit: EntryUnit::Sg,
                instrument: Some(Instrument::Hydrometer),
                sample_temp_c: None,
                calibration_temp_c: None,
                wort_correction: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// An iSpindle through the whole ferment, a vessel probe that cold-crashes on day
    /// 11, EC-1118, 12 g of priced DAP and a recorded FG.
    #[test]
    fn a_sensor_batch_has_every_metric() {
        let mut conn = db();
        let id = batch(&conn, "SELTZER-001", None, Some(1.045));
        set_pitched_at(&conn, id, T0).unwrap();
        let (_, ec1118) = yeast(&conn, "EC-1118");
        add(&conn, id, ec1118, 5.0, Stage::Pitch);
        let attrs = format!(r#"{{"mg_yan_per_g": {DAP_MG_YAN_PER_G}}}"#);
        let price = Money::new(1000, Currency::EUR);
        let (_, dap) = stocked(
            &conn,
            "DAP",
            Category::Nutrient,
            None,
            Some(&attrs),
            Some(price),
        );
        add(&conn, id, dap, 12.0, Stage::Fermentation);
        set_batch_measurements(&conn, id, None, None, Some(sg(1.000))).unwrap();

        let span = (14 * DAY, 15 * 60);
        stream(&mut conn, id, Metric::Sg, "ispindel", span, gravity_at);
        let crash = 11.0 * 24.0;
        let probe = stream(&mut conn, id, Metric::TempC, "vessel", span, |h| {
            if h < crash { 18.0 } else { 2.0 }
        });
        stream(
            &mut conn,
            id,
            Metric::GravityTempC,
            "ispindel-temp",
            span,
            |_| 25.0,
        );

        let got = batch_metrics(&conn, id, &params()).unwrap();

        let lag = value(got.lag);
        let want = 12.0 + 34.0 * (45.0_f64 / 43.0).ln();
        assert!(
            (lag.secs as f64 / HOUR as f64 - want).abs() <= 0.25,
            "{lag:?}"
        );
        let peak = value(got.peak_rate);
        assert!(
            (peak.secs_from_pitch as f64 / HOUR as f64 - 15.0).abs() <= 0.5,
            "{peak:?}"
        );

        let terminal = value(got.terminal);
        let history = batch_stability(&conn, id, &params()).unwrap().history;
        let since = history.iter().find_map(|e| match e.state {
            Stability::Stable { since, .. } => Some(since),
            _ => None,
        });
        assert_eq!(Some(terminal.at), since);
        assert!(
            terminal.at < at((crash * HOUR as f64) as i64),
            "{terminal:?}"
        );

        // The probe over the iSpindle's 25 °C, and cut at terminal: the crash is after.
        let temp = value(got.ferm_temp);
        assert_eq!(temp.stream_id, probe);
        assert!(temp.to_terminal);
        assert_eq!(
            (temp.spread.median, temp.spread.min, temp.spread.max),
            (18.0, 18.0, 18.0)
        );

        assert_eq!(
            got.abv_alternate,
            Computed::Value(abv::alternate(sg(1.045), sg(1.000)).unwrap())
        );
        assert!(matches!(got.apparent_attenuation, Computed::Value(_)));
        assert!((value(got.yan_added_mg_per_l) - 127.2).abs() < 1e-9);
        assert_eq!(
            got.cost_per_litre,
            Computed::Value(batch_cost(&conn, id).unwrap().per_litre.unwrap())
        );
        assert_eq!(got.cost_lines_without_cost, 1, "the yeast lot is unpriced");
        assert_eq!(
            got.strains
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["EC-1118"]
        );
    }

    /// The curve metrics say why; the recorded ones still come through.
    #[test]
    fn a_manual_only_batch_gives_reasons_for_the_curve() {
        let mut conn = db();
        let id = batch(&conn, "SELTZER-001", None, Some(1.045));
        set_pitched_at(&conn, id, T0).unwrap();
        for (day, reading) in [(1, 1.040), (3, 1.020), (7, 1.000)] {
            hydrometer(&mut conn, id, day * DAY, reading);
        }
        set_batch_measurements(&conn, id, None, None, Some(sg(1.000))).unwrap();
        let (_, sugar) = stocked(
            &conn,
            "Dextrose",
            Category::Sugar,
            None,
            None,
            Some(Money::new(300, Currency::EUR)),
        );
        add(&conn, id, sugar, 1000.0, Stage::Prep);

        let got = batch_metrics(&conn, id, &params()).unwrap();
        let manual = Some(Missing::ManualOnly);
        assert_eq!(missing(&got.lag), manual);
        assert_eq!(missing(&got.peak_rate), manual);
        assert_eq!(missing(&got.terminal), manual);
        assert_eq!(got.ferm_temp, Computed::Missing(Missing::NoTemp));
        assert!(matches!(got.abv_alternate, Computed::Value(_)));
        assert!(matches!(got.cost_per_litre, Computed::Value(_)));
        assert_eq!(got.yan_added_mg_per_l, Computed::Value(0.0));
    }

    #[test]
    fn an_unpitched_batch_has_no_curve_metrics() {
        let conn = db();
        let id = batch(&conn, "SELTZER-001", None, None);
        let got = batch_metrics(&conn, id, &params()).unwrap();
        let no_pitch = Some(Missing::NoPitch);
        assert_eq!(missing(&got.lag), no_pitch);
        assert_eq!(missing(&got.peak_rate), no_pitch);
        assert_eq!(missing(&got.terminal), no_pitch);
        assert_eq!(got.ferm_temp, Computed::Missing(Missing::NoPitch));
        assert_eq!(got.abv_alternate, Computed::Missing(Missing::NoOg));
        assert_eq!(got.cost_per_litre, Computed::Missing(Missing::NoCost));

        let bare = batch(&conn, "SELTZER-002", None, Some(1.045));
        set_pitched_at(&conn, bare, T0).unwrap();
        let got = batch_metrics(&conn, bare, &params()).unwrap();
        assert_eq!(got.lag, Computed::Missing(Missing::NoSensorGravity));
    }

    /// Lag is measured from the OG; peak rate isn't.
    #[test]
    fn no_og_blocks_lag_and_not_peak_rate() {
        let mut conn = db();
        let id = batch(&conn, "SELTZER-001", None, None);
        set_pitched_at(&conn, id, T0).unwrap();
        stream(
            &mut conn,
            id,
            Metric::Sg,
            "ispindel",
            (14 * DAY, 15 * 60),
            gravity_at,
        );
        let got = batch_metrics(&conn, id, &params()).unwrap();
        assert_eq!(got.lag, Computed::Missing(Missing::NoOg));
        assert!(
            matches!(got.peak_rate, Computed::Value(_)),
            "{:?}",
            got.peak_rate
        );
    }

    /// The recipe planned US-05; EC-1118 went in.
    #[test]
    fn the_strain_is_from_actuals_only() {
        let conn = db();
        let version = recipe(&conn);
        let (planned, _) = yeast(&conn, "US-05");
        add_recipe_item(
            &conn,
            version,
            &NewRecipeItem {
                ingredient_id: planned,
                entry: Quantity::new(5.0, Unit::G).unwrap(),
                stage: Stage::Pitch,
                offset_hours: None,
                notes: None,
            },
        )
        .unwrap();
        let id = batch(&conn, "SELTZER-001", Some(version), Some(1.045));
        assert!(
            batch_metrics(&conn, id, &params())
                .unwrap()
                .strains
                .is_empty()
        );

        let (_, pitched) = yeast(&conn, "EC-1118");
        add(&conn, id, pitched, 5.0, Stage::Pitch);
        let got = batch_metrics(&conn, id, &params()).unwrap();
        assert_eq!(
            got.strains
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["EC-1118"]
        );
    }

    #[test]
    fn a_nutrient_with_no_yield_is_unknown_not_zero() {
        let conn = db();
        let id = batch(&conn, "SELTZER-001", None, None);
        let (_, fermaid) = stocked(&conn, "Fermaid O", Category::Nutrient, None, None, None);
        add(&conn, id, fermaid, 10.0, Stage::Fermentation);
        let got = batch_metrics(&conn, id, &params()).unwrap();
        assert_eq!(
            got.yan_added_mg_per_l,
            Computed::Missing(Missing::NoNutrientYield)
        );
    }

    #[test]
    fn mixed_currencies_are_a_reason_not_an_error() {
        let conn = db();
        let id = batch(&conn, "SELTZER-001", None, None);
        let eur = Some(Money::new(500, Currency::EUR));
        let usd = Some(Money::new(500, Currency::USD));
        let (_, a) = stocked(&conn, "Dextrose", Category::Sugar, None, None, eur);
        let (_, b) = stocked(&conn, "Sucrose", Category::Sugar, None, None, usd);
        add(&conn, id, a, 100.0, Stage::Prep);
        add(&conn, id, b, 100.0, Stage::Prep);
        let got = batch_metrics(&conn, id, &params()).unwrap();
        assert_eq!(
            got.cost_per_litre,
            Computed::Missing(Missing::MixedCurrency)
        );
    }

    #[test]
    fn archived_batches_are_listed_only_when_asked() {
        let conn = db();
        let kept = batch(&conn, "SELTZER-001", None, None);
        let archived = batch(&conn, "SELTZER-002", None, None);
        assert!(archive_batch(&conn, archived, at(DAY)).unwrap());

        let ids = |include| {
            list_batch_metrics(&conn, include, &params())
                .unwrap()
                .into_iter()
                .map(|m| m.batch_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(false), [kept]);
        assert_eq!(ids(true), [kept, archived]);
    }
}
