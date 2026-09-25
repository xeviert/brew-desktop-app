//! Phase 4c's joins: the gravity series and the expected FG that
//! [`calc::stability`](crate::calc::stability) judges. Nothing is decided here.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::Connection;

use super::{
    Metric,
    batch::{BatchRow, get_batch},
    recipe::get_recipe_version,
    series::{Series, batch_series},
    tier1::SourceKind,
};
use crate::{
    Result,
    calc::stability::{
        Evaluation, ExpectedFg, Insufficient, Stability, StabilityParams, stability_history,
    },
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchStability {
    /// The sensor stream judged. `None` with no sensor gravity bound to the batch.
    pub stream_id: Option<i64>,
    pub current: Stability,
    pub expected_fg: Option<ExpectedFg>,
    /// One per smoothed reading since pitch, for the chart to shade the stable spans.
    pub history: Vec<Evaluation>,
}

/// The recipe version's `target_fg`, else `batch.og` and the yeast's attenuation band,
/// else `None`.
pub fn expected_fg(conn: &Connection, batch_id: i64) -> Result<Option<ExpectedFg>> {
    expected_fg_for(conn, &get_batch(conn, batch_id)?)
}

fn expected_fg_for(conn: &Connection, batch: &BatchRow) -> Result<Option<ExpectedFg>> {
    let target_fg = match batch.recipe_version_id {
        Some(id) => get_recipe_version(conn, id)?.target_fg,
        None => None,
    };
    Ok(ExpectedFg::resolve(
        target_fg,
        batch.og,
        attenuation_band(conn, batch)?,
    ))
}

/// The strains actually pitched, else the recipe's. From actuals first because a
/// substituted yeast is the case this is most often wrong about.
///
/// Two strains give the widest band of the two. A strain with no band makes the whole
/// answer `None`: it could finish anywhere, so nothing can be called a stall.
fn attenuation_band(conn: &Connection, batch: &BatchRow) -> Result<Option<(f64, f64)>> {
    let mut bands = strain_bands(
        conn,
        "SELECT DISTINCT ys.id, ys.attenuation_min, ys.attenuation_max \
           FROM batch_ingredient bi \
           JOIN ingredient_lot l ON l.id = bi.ingredient_lot_id \
           JOIN ingredient i ON i.id = l.ingredient_id \
           JOIN yeast_strain ys ON ys.id = i.yeast_strain_id \
          WHERE bi.batch_id = ?",
        batch.id,
    )?;
    if let (true, Some(version_id)) = (bands.is_empty(), batch.recipe_version_id) {
        bands = strain_bands(
            conn,
            "SELECT DISTINCT ys.id, ys.attenuation_min, ys.attenuation_max \
               FROM recipe_item ri \
               JOIN ingredient i ON i.id = ri.ingredient_id \
               JOIN yeast_strain ys ON ys.id = i.yeast_strain_id \
              WHERE ri.recipe_version_id = ?",
            version_id,
        )?;
    }

    let mut widest: Option<(f64, f64)> = None;
    for band in bands {
        let Some((min, max)) = band else {
            return Ok(None);
        };
        widest = Some(widest.map_or((min, max), |(lo, hi)| (lo.min(min), hi.max(max))));
    }
    Ok(widest)
}

fn strain_bands(conn: &Connection, sql: &str, id: i64) -> Result<Vec<Option<(f64, f64)>>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([id], |row| {
        let min: Option<f64> = row.get(1)?;
        let max: Option<f64> = row.get(2)?;
        Ok(min.zip(max))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The detector over the batch's sensor gravity since pitch, every reading of it.
///
/// Of several sensor `sg` streams, the one with the newest reading — the same pick as
/// 4e's current SG. Manual gravity is never judged: a dozen hydrometer readings do not
/// fill a day's window, and a fit through them is 4a's invented point again.
pub fn batch_stability(
    conn: &Connection,
    batch_id: i64,
    params: &StabilityParams,
) -> Result<BatchStability> {
    Ok(judge(conn, &get_batch(conn, batch_id)?, params)?.0)
}

/// [`batch_stability`], and the sensor series it judged. 4e's current SG is that
/// series' newest smoothed value, so the card and the detector cannot pick different
/// streams.
pub(crate) fn judge(
    conn: &Connection,
    batch: &BatchRow,
    params: &StabilityParams,
) -> Result<(BatchStability, Option<Series>)> {
    let expected_fg = expected_fg_for(conn, batch)?;
    // Unbounded, so every stream comes back raw: the fit wants every reading, and
    // sensor gravity is always smoothed before it is returned.
    let series = batch_series(conn, batch.id, &[Metric::Sg], None, usize::MAX)?;
    let Some(sensor) = series
        .series
        .into_iter()
        .filter(|s| s.source_kind != SourceKind::Manual && !s.ts.is_empty())
        .max_by_key(|s| s.ts.last().copied())
    else {
        let empty = BatchStability {
            stream_id: None,
            current: Stability::Insufficient {
                reason: Insufficient::Empty,
            },
            expected_fg,
            history: Vec::new(),
        };
        return Ok((empty, None));
    };

    let points: Vec<_> = sensor
        .ts
        .iter()
        .zip(&sensor.smoothed)
        .filter_map(|(&ts, value)| Some((ts, (*value)?)))
        .collect();
    let history = stability_history(&points, batch.og, expected_fg.as_ref(), params)?;
    let judged = BatchStability {
        stream_id: Some(sensor.stream_id),
        current: history.last().map_or(
            Stability::Insufficient {
                reason: Insufficient::Empty,
            },
            |e| e.state,
        ),
        expected_fg,
        history,
    };
    Ok((judged, Some(sensor)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calc::stability::FgSource,
        db::{
            batch::{
                NewBatch, NewBatchIngredient, add_batch_ingredient, create_batch, set_pitched_at,
            },
            recipe::{
                BeverageType, NewRecipeItem, NewRecipeVersion, Stage, add_recipe_item,
                create_recipe, create_recipe_version,
            },
            reference::{
                Category, NewIngredient, NewLot, NewYeastStrain, create_ingredient, create_lot,
                create_yeast_strain,
            },
            tier1::{create_source, create_stream, insert_readings},
            timeline::bind_stream,
        },
        units::{Quantity, Sg, Timestamp, Unit},
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

    fn batch(conn: &Connection, version: Option<i64>, og: Option<f64>) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: "SELTZER-001".into(),
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

    fn recipe(conn: &Connection, target_fg: Option<f64>) -> i64 {
        let recipe = create_recipe(conn, "Citron-Verveine", BeverageType::Seltzer, None).unwrap();
        create_recipe_version(
            conn,
            recipe,
            &NewRecipeVersion {
                target_fg: target_fg.map(sg),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// A yeast ingredient of a strain with this band, and one lot of it.
    fn yeast(conn: &Connection, name: &str, band: Option<(f64, f64)>) -> (i64, i64) {
        let strain = create_yeast_strain(
            conn,
            &NewYeastStrain {
                name: name.into(),
                attenuation_min: band.map(|b| b.0),
                attenuation_max: band.map(|b| b.1),
                ..Default::default()
            },
        )
        .unwrap();
        let ingredient = create_ingredient(
            conn,
            &NewIngredient {
                name: name.into(),
                category: Category::Yeast,
                default_unit: Unit::G,
                yeast_strain_id: Some(strain),
                attrs: None,
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
                entry: Quantity::new(100.0, Unit::G).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        (ingredient, lot)
    }

    fn pitch(conn: &Connection, batch_id: i64, lot: i64) {
        add_batch_ingredient(
            conn,
            batch_id,
            &NewBatchIngredient {
                ingredient_lot_id: lot,
                entry: Quantity::new(5.0, Unit::G).unwrap(),
                stage: Stage::Pitch,
                added_at: None,
                notes: None,
            },
        )
        .unwrap();
    }

    fn plan(conn: &Connection, version: i64, ingredient: i64) {
        add_recipe_item(
            conn,
            version,
            &NewRecipeItem {
                ingredient_id: ingredient,
                entry: Quantity::new(5.0, Unit::G).unwrap(),
                stage: Stage::Pitch,
                offset_hours: None,
                notes: None,
            },
        )
        .unwrap();
    }

    /// A stream of `kind` bound from `T0`, with `f(hours)` every `interval`.
    fn gravity(
        conn: &mut Connection,
        batch_id: i64,
        kind: SourceKind,
        name: &str,
        (end, interval): (i64, i64),
        f: impl Fn(f64) -> f64,
    ) -> i64 {
        let source_id = create_source(conn, kind, name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, name, Metric::Sg).unwrap();
        let rows: Vec<_> = (0..end)
            .step_by(interval as usize)
            .map(|t| (stream_id, at(t), f(t as f64 / HOUR as f64)))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        bind_stream(conn, batch_id, stream_id, T0, None).unwrap();
        stream_id
    }

    #[test]
    fn the_recipe_target_wins_over_the_strain() {
        let conn = db();
        let version = recipe(&conn, Some(1.002));
        let batch_id = batch(&conn, Some(version), Some(1.045));
        let (_, lot) = yeast(&conn, "EC-1118", Some((0.9, 1.0)));
        pitch(&conn, batch_id, lot);

        let got = expected_fg(&conn, batch_id).unwrap().unwrap();
        assert_eq!(got.source, FgSource::Recipe);
        assert_eq!(got.hi, sg(1.002));
    }

    /// The recipe planned one yeast, the brewer pitched another. The one pitched is
    /// the one that decides where the batch ends.
    #[test]
    fn the_strain_actually_pitched_beats_the_one_planned() {
        let conn = db();
        let version = recipe(&conn, None);
        let (planned, _) = yeast(&conn, "US-05", Some((0.78, 0.82)));
        plan(&conn, version, planned);
        let batch_id = batch(&conn, Some(version), Some(1.050));

        let from_plan = expected_fg(&conn, batch_id).unwrap().unwrap();
        assert_eq!(from_plan.source, FgSource::Strain);
        assert!((from_plan.hi.get() - 1.011).abs() < 1e-9, "{from_plan:?}");

        let (_, pitched) = yeast(&conn, "EC-1118", Some((0.90, 1.0)));
        pitch(&conn, batch_id, pitched);
        let from_actual = expected_fg(&conn, batch_id).unwrap().unwrap();
        assert!(
            (from_actual.hi.get() - 1.005).abs() < 1e-9,
            "{from_actual:?}"
        );
        assert!(
            (from_actual.lo.get() - 1.000).abs() < 1e-9,
            "{from_actual:?}"
        );
    }

    #[test]
    fn two_strains_give_the_widest_band_and_one_without_a_band_gives_none() {
        let conn = db();
        let batch_id = batch(&conn, None, Some(1.050));
        let (_, a) = yeast(&conn, "A", Some((0.80, 0.85)));
        let (_, b) = yeast(&conn, "B", Some((0.75, 0.90)));
        pitch(&conn, batch_id, a);
        pitch(&conn, batch_id, b);
        let got = expected_fg(&conn, batch_id).unwrap().unwrap();
        assert!((got.hi.get() - 1.0125).abs() < 1e-9, "{got:?}");
        assert!((got.lo.get() - 1.005).abs() < 1e-9, "{got:?}");

        let (_, c) = yeast(&conn, "C", None);
        pitch(&conn, batch_id, c);
        assert_eq!(expected_fg(&conn, batch_id).unwrap(), None);
    }

    #[test]
    fn no_recipe_and_no_og_is_no_expected_fg_and_a_finish_is_unchecked() {
        let mut conn = db();
        let batch_id = batch(&conn, None, None);
        set_pitched_at(&conn, batch_id, T0).unwrap();
        gravity(
            &mut conn,
            batch_id,
            SourceKind::BrewdJournal,
            "ispindel",
            (12 * DAY, 15 * 60),
            gravity_at,
        );

        let got = batch_stability(&conn, batch_id, &StabilityParams::default()).unwrap();
        assert_eq!(got.expected_fg, None);
        assert!(
            matches!(
                got.current,
                Stability::Stable {
                    fg_unchecked: true,
                    ..
                }
            ),
            "{:?}",
            got.current
        );
        assert_eq!(got.history.len(), 12 * 24 * 4);
    }

    /// The pipeline end to end: bound, clipped, smoothed, judged against the recipe.
    #[test]
    fn a_batch_stuck_above_its_target_reads_stalled() {
        let mut conn = db();
        let version = recipe(&conn, Some(1.000));
        let batch_id = batch(&conn, Some(version), Some(1.045));
        set_pitched_at(&conn, batch_id, T0).unwrap();
        let stream_id = gravity(
            &mut conn,
            batch_id,
            SourceKind::BrewdJournal,
            "ispindel",
            (12 * DAY, 15 * 60),
            |h| 1.010 + (gravity_at(h) - 1.000) * 0.035 / 0.045,
        );

        let got = batch_stability(&conn, batch_id, &StabilityParams::default()).unwrap();
        assert_eq!(got.stream_id, Some(stream_id));
        assert!(
            matches!(got.current, Stability::Stalled { .. }),
            "{:?}",
            got.current
        );
    }

    #[test]
    fn a_manual_only_batch_is_insufficient() {
        let mut conn = db();
        let batch_id = batch(&conn, None, Some(1.045));
        set_pitched_at(&conn, batch_id, T0).unwrap();
        gravity(
            &mut conn,
            batch_id,
            SourceKind::Manual,
            "hydrometer",
            (12 * DAY, DAY),
            gravity_at,
        );

        let got = batch_stability(&conn, batch_id, &StabilityParams::default()).unwrap();
        assert_eq!(got.stream_id, None);
        assert_eq!(
            got.current,
            Stability::Insufficient {
                reason: Insufficient::Empty
            }
        );
    }
}
