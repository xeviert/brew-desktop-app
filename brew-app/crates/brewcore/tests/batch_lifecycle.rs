//! Phase 3's "Done when", as one test: a batch created from a recipe version, pitched,
//! with manual readings and events logged against it, a correct OG/FG/ABV and
//! planned-vs-actual, and the vessel's sensor curve attached through `batch_stream`.
//!
//! Each step has its own unit tests in `db::*`. This one exists for the seams between
//! them, through a real `Db` file rather than an in-memory connection.

use brewcore::{
    calc::{abv, attenuation},
    db::{
        Db, Metric, SourceKind,
        batch::{
            BatchStatus, NewBatch, NewBatchIngredient, PlanVsActualRow, add_batch_ingredient,
            batch_cost, create_batch, get_batch, plan_vs_actual, set_batch_measurements,
            transition_batch,
        },
        measurement::{EntryUnit, Instrument, NewMeasurement, QuickEntry, record_quick_entry},
        recipe::{
            BeverageType, NewRecipeItem, NewRecipeVersion, Stage, add_recipe_item, create_recipe,
            create_recipe_version, fork_recipe_version, get_recipe_version,
        },
        reference::{
            Category, NewIngredient, NewLot, VesselKind, create_ingredient, create_lot,
            create_vessel, set_vessel_streams,
        },
        tier1::{create_source, create_stream, insert_readings},
        timeline::{
            EventKind, NewBatchEvent, add_event, batch_for_stream_at, list_batch_events,
            record_pitch,
        },
    },
    units::{Currency, Litres, Money, Quantity, Sg, Timestamp, Unit},
};
use rusqlite::Connection;

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const BREW_DAY: Timestamp = Timestamp::from_secs(1_700_000_000);
const PITCH: Timestamp = Timestamp::from_secs(1_700_000_000 + 2 * HOUR);

fn at(offset_secs: i64) -> Timestamp {
    PITCH.saturating_add_secs(offset_secs)
}

fn qty(value: f64, unit: Unit) -> Quantity {
    Quantity::new(value, unit).unwrap()
}

fn eur(minor: i64) -> Money {
    Money::new(minor, Currency::EUR)
}

fn stocked(
    conn: &Connection,
    name: &str,
    category: Category,
    unit: Unit,
    lot: Quantity,
    cost: Option<Money>,
) -> (i64, i64) {
    let ingredient_id = create_ingredient(
        conn,
        &NewIngredient {
            name: name.into(),
            category,
            default_unit: unit,
            yeast_strain_id: None,
            attrs: None,
            notes: None,
        },
    )
    .unwrap();
    let lot_id = create_lot(
        conn,
        &NewLot {
            ingredient_id,
            supplier: None,
            lot_code: None,
            entry: lot,
            cost,
            purchased_at: None,
            expires_at: None,
            notes: None,
        },
    )
    .unwrap();
    (ingredient_id, lot_id)
}

fn gravity(sg: f64) -> NewMeasurement {
    NewMeasurement {
        metric: Metric::Sg,
        entry_value: sg,
        entry_unit: EntryUnit::Sg,
        instrument: Some(Instrument::Hydrometer),
        sample_temp_c: None,
        calibration_temp_c: None,
        wort_correction: None,
    }
}

fn ph(value: f64) -> NewMeasurement {
    NewMeasurement {
        metric: Metric::Ph,
        entry_value: value,
        entry_unit: EntryUnit::Ph,
        instrument: Some(Instrument::PhMeter),
        sample_temp_c: None,
        calibration_temp_c: None,
        wort_correction: None,
    }
}

fn addition(lot: i64, entry: Quantity, stage: Stage, added_at: Timestamp) -> NewBatchIngredient {
    NewBatchIngredient {
        ingredient_lot_id: lot,
        entry,
        stage,
        added_at: Some(added_at),
        notes: None,
    }
}

#[test]
fn a_batch_runs_from_recipe_to_package_with_its_sensor_curve_attached() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("brewery.db")).unwrap();

    // Reference data and a vessel whose probe is a brewd journal stream.
    let (vessel, probe) = db
        .write(|conn| {
            let tx = conn.transaction()?;
            let vessel = create_vessel(
                &tx,
                "FV1",
                VesselKind::Fermenter,
                Some(Litres::new(30.0)?),
                None,
            )?;
            let source = create_source(&tx, SourceKind::BrewdJournal, "esp32-1", None)?;
            let probe = create_stream(&tx, source, "vessel_temp", Metric::TempC)?;
            set_vessel_streams(&tx, vessel, &[probe])?;
            tx.commit()?;
            Ok((vessel, probe))
        })
        .unwrap();

    let (sugar, sugar_lot, nutrient, nutrient_lot, _, zest_lot) = db
        .write(|conn| {
            let (sugar, sugar_lot) = stocked(
                conn,
                "Sucrose",
                Category::Sugar,
                Unit::Kg,
                qty(5.0, Unit::Kg),
                Some(eur(1250)),
            );
            let (nutrient, nutrient_lot) = stocked(
                conn,
                "Fermaid O",
                Category::Nutrient,
                Unit::G,
                qty(100.0, Unit::G),
                Some(eur(1000)),
            );
            let (zest, zest_lot) = stocked(
                conn,
                "Lemon zest",
                Category::Flavoring,
                Unit::G,
                qty(200.0, Unit::G),
                None,
            );
            Ok((sugar, sugar_lot, nutrient, nutrient_lot, zest, zest_lot))
        })
        .unwrap();

    // A recipe: sugar at prep, then a two-step nutrient schedule.
    let version = db
        .write(|conn| {
            let recipe = create_recipe(conn, "Citron", BeverageType::Seltzer, None)?;
            let version = create_recipe_version(
                conn,
                recipe,
                &NewRecipeVersion {
                    target_volume_l: Some(Litres::new(20.0)?),
                    target_og: Some(Sg::new(1.040)?),
                    ..Default::default()
                },
            )?;
            for (ingredient_id, entry, stage, offset_hours) in [
                (sugar, qty(3.5, Unit::Kg), Stage::Prep, None),
                (
                    nutrient,
                    qty(10.0, Unit::G),
                    Stage::Fermentation,
                    Some(24.0),
                ),
                (
                    nutrient,
                    qty(10.0, Unit::G),
                    Stage::Fermentation,
                    Some(48.0),
                ),
            ] {
                add_recipe_item(
                    conn,
                    version,
                    &NewRecipeItem {
                        ingredient_id,
                        entry,
                        stage,
                        offset_hours,
                        notes: None,
                    },
                )?;
            }
            Ok(version)
        })
        .unwrap();

    // Creating the batch freezes the version; further edits have to fork.
    let batch = db
        .write(|conn| {
            create_batch(
                conn,
                &NewBatch {
                    code: "SELTZER-001".into(),
                    recipe_version_id: Some(version),
                    beverage_type: BeverageType::Seltzer,
                    vessel_id: Some(vessel),
                    volume_l: None,
                    og: None,
                    notes: None,
                },
                BREW_DAY,
            )
        })
        .unwrap();
    db.write(|conn| {
        assert!(get_recipe_version(conn, version)?.is_locked());
        let err = add_recipe_item(
            conn,
            version,
            &NewRecipeItem {
                ingredient_id: sugar,
                entry: qty(1.0, Unit::Kg),
                stage: Stage::Prep,
                offset_hours: None,
                notes: None,
            },
        )
        .expect_err("a version a batch points at is immutable");
        assert_eq!(err.code(), "db.recipe_version_locked");
        let fork = fork_recipe_version(conn, version)?;
        assert_eq!(get_recipe_version(conn, fork)?.version_no, 2);
        Ok(())
    })
    .unwrap();

    // Brew day: sugar goes in, the OG is taken and then deliberately adopted as the OG.
    db.write(|conn| {
        let tx = conn.transaction()?;
        add_batch_ingredient(
            &tx,
            batch,
            &addition(sugar_lot, qty(3.6, Unit::Kg), Stage::Prep, BREW_DAY),
        )?;
        let og = record_quick_entry(
            &tx,
            batch,
            &QuickEntry {
                ts: BREW_DAY,
                measurements: vec![gravity(1.040)],
                note: None,
            },
        )?;
        let og = Sg::new(og.measurements[0].value)?;
        set_batch_measurements(&tx, batch, None, Some(og), None)?;
        tx.commit()?;
        Ok(())
    })
    .unwrap();

    // A probe reading from before the pitch belongs to no batch.
    db.write(|conn| {
        let tx = conn.transaction()?;
        insert_readings(&tx, &[(probe, BREW_DAY, 21.0)])?;
        tx.commit()?;
        Ok(())
    })
    .unwrap();

    // Pitch: t=0, status moves, and the vessel's probe binds from here on.
    db.write(|conn| {
        let tx = conn.transaction()?;
        record_pitch(&tx, batch, PITCH, Some("US-05, rehydrated"))?;
        tx.commit()?;
        Ok(())
    })
    .unwrap();
    let pitched = db.read(|conn| get_batch(conn, batch)).unwrap();
    assert_eq!(pitched.status, BatchStatus::Fermenting);
    assert_eq!(pitched.pitched_at, Some(PITCH));

    // Fermentation: the probe logs, the first nutrient dose goes in late, the second
    // never does, and a flavoring nobody planned is added.
    let curve: Vec<_> = (0..=10)
        .map(|d| (probe, at(d * DAY), 20.0 - 0.1 * d as f64))
        .collect();
    db.write(|conn| {
        let tx = conn.transaction()?;
        assert_eq!(insert_readings(&tx, &curve)?, curve.len());
        add_batch_ingredient(
            &tx,
            batch,
            &addition(
                nutrient_lot,
                qty(10.0, Unit::G),
                Stage::Fermentation,
                at(30 * HOUR),
            ),
        )?;
        add_event(
            &tx,
            batch,
            &NewBatchEvent {
                ts: at(30 * HOUR),
                kind: EventKind::NutrientAddition,
                payload: None,
                note: Some("six hours late".into()),
            },
        )?;
        add_batch_ingredient(
            &tx,
            batch,
            &addition(
                zest_lot,
                qty(15.0, Unit::G),
                Stage::Fermentation,
                at(7 * DAY),
            ),
        )?;
        tx.commit()?;
        Ok(())
    })
    .unwrap();

    // Day 10: dry. FG, pH and a taste note from one bench moment.
    let finished = db
        .write(|conn| {
            let tx = conn.transaction()?;
            let result = record_quick_entry(
                &tx,
                batch,
                &QuickEntry {
                    ts: at(10 * DAY),
                    measurements: vec![gravity(0.998), ph(3.2)],
                    note: Some("clean, dry, lemony".into()),
                },
            )?;
            let fg = Sg::new(result.measurements[0].value)?;
            set_batch_measurements(&tx, batch, None, None, Some(fg))?;
            tx.commit()?;
            Ok(result)
        })
        .unwrap();
    assert_eq!(
        finished.measurements[0].ts, finished.measurements[1].ts,
        "one bench moment is one timestamp"
    );
    assert!(finished.event_id.is_some());

    // OG / FG / ABV.
    let batch_row = db.read(|conn| get_batch(conn, batch)).unwrap();
    let (og, fg) = (batch_row.og.unwrap(), batch_row.fg.unwrap());
    assert_eq!((og.get(), fg.get()), (1.040, 0.998));
    // Fractions: (1.040 − 0.998) × 1.3125, and Hall's 76.08·0.042/0.735 × 0.998/0.794.
    assert!((abv::simple(og, fg).unwrap() - 0.055125).abs() < 1e-9);
    assert!((abv::alternate(og, fg).unwrap() - 0.05464).abs() < 1e-4);
    // A seltzer that finishes below water attenuates past 100 %, unclamped.
    assert!((attenuation::apparent(og, fg).unwrap() - 1.05).abs() < 1e-9);

    // Events: pitch, nutrient, taste note, in time order.
    let kinds: Vec<_> = db
        .read(|conn| list_batch_events(conn, batch))
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            EventKind::Pitch,
            EventKind::NutrientAddition,
            EventKind::Note
        ]
    );

    // Planned vs actual: sugar over by 100 g, first nutrient on target (late is not a
    // difference), second missing, zest unplanned.
    let pva = db.read(|conn| plan_vs_actual(conn, batch)).unwrap();
    assert_eq!(pva.recipe_version_id, Some(version));
    let mut both = Vec::new();
    let mut missing = Vec::new();
    let mut unplanned = Vec::new();
    for row in pva.rows {
        match row {
            PlanVsActualRow::Both { planned, delta, .. } => {
                both.push((planned.ingredient_id, delta.unwrap()))
            }
            PlanVsActualRow::Missing(planned) => missing.push(planned.ingredient_id),
            PlanVsActualRow::Unplanned(actual) => unplanned.push(actual.ingredient_lot_id),
        }
    }
    both.sort_by_key(|(id, _)| *id);
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].0, sugar);
    assert!(
        (both[0].1 - 100.0).abs() < 1e-6,
        "sugar delta {}",
        both[0].1
    );
    assert_eq!(both[1], (nutrient, 0.0));
    assert_eq!(missing, [nutrient]);
    assert_eq!(unplanned, [zest_lot]);

    // Cost: 3.6 kg of a €12.50 / 5 kg lot is €9.00, 10 g of a €10.00 / 100 g lot is
    // €1.00. The zest was never priced and is counted as such, not as free. No volume
    // measured, so per litre falls back to the recipe's 20 L.
    let cost = db.read(|conn| batch_cost(conn, batch)).unwrap();
    assert_eq!(cost.total, Some(eur(1000)));
    assert_eq!(cost.per_litre, Some(eur(50)));
    assert_eq!((cost.lines_costed, cost.lines_without_cost), (2, 1));

    // Package: the probe's window closes, the manual gravity stream stays open.
    let packaged_at = at(14 * DAY);
    db.write(|conn| {
        let tx = conn.transaction()?;
        transition_batch(&tx, batch, BatchStatus::Conditioning, at(11 * DAY))?;
        transition_batch(&tx, batch, BatchStatus::Packaged, packaged_at)?;
        tx.commit()?;
        Ok(())
    })
    .unwrap();

    let manual_sg = finished.measurements[0].stream_id;
    db.read(|conn| {
        assert_eq!(
            batch_for_stream_at(conn, probe, BREW_DAY)?,
            None,
            "before pitch"
        );
        for &(_, ts, _) in &curve {
            assert_eq!(batch_for_stream_at(conn, probe, ts)?, Some(batch));
        }
        assert_eq!(
            batch_for_stream_at(conn, probe, packaged_at)?,
            None,
            "after package"
        );
        // The pre-pitch OG widened the manual binding back to brew day.
        assert_eq!(batch_for_stream_at(conn, manual_sg, BREW_DAY)?, Some(batch));
        assert_eq!(
            batch_for_stream_at(conn, manual_sg, at(30 * DAY))?,
            Some(batch)
        );
        Ok(())
    })
    .unwrap();
}
