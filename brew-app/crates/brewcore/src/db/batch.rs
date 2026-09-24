//! Phase 3c. Batches: the thing a recipe version exists to be used by.
//!
//! Two rules carry this module. **A batch freezes the version it points at** —
//! [`create_batch`] calls 3b's [`lock_recipe_version`] — which is what stops "every
//! batch of Citron-Verveine v3" from meaning whatever the recipe last said. And **the
//! status machine is enforced here, not in the UI**: a greyed-out button is a
//! suggestion the next caller ignores.
//!
//! Plan and actual are separate tables on purpose. `recipe_item` is what you meant to
//! do, `batch_ingredient` what you put in and which lot it came from; recording only
//! the first makes the log useless for diagnosis, only the second makes the recipe
//! unrepeatable. [`plan_vs_actual`] is the diff.
//!
//! Same contract as [`recipe`](crate::db::recipe) and [`tier1`](crate::db::tier1):
//! every function takes a borrowed connection and never opens or commits its own
//! transaction. `&Transaction` derefs to `&Connection`, so "lock the version and
//! insert the batch" is one boundary at the call site.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    enums::db_enum,
    recipe::{
        BeverageType, RecipeItemRow, Stage, get_recipe_version, list_recipe_items,
        lock_recipe_version,
    },
    reference::{CanonicalQty, canonical_for_ingredient, canonical_from_columns},
};
use crate::{
    Error, Result,
    units::{Grams, Litres, Quantity, Sg, Timestamp},
};

db_enum!(
    /// Where a batch is in its life. Not a `CHECK` in the schema — see
    /// `0004_batch.sql`.
    ///
    /// There is no `Archived` variant: archiving is `batch.archived_at`, so that one
    /// fact lives in one column and an archived batch still reads as whatever it was
    /// doing when it was put away.
    BatchStatus,
    UnknownBatchStatus,
    (Planned, "planned"),
    (Fermenting, "fermenting"),
    (Conditioning, "conditioning"),
    (Packaged, "packaged"),
);

impl BatchStatus {
    /// The chain, forwards and one step at a time.
    ///
    /// Backwards is refused rather than allowed-with-a-warning: a batch that went
    /// back to fermenting is either a mis-click or a different batch, and Phase 5's
    /// "days to terminal" cannot be computed from a status that oscillates. A no-op
    /// (`x → x`) is not a transition, so it is `false` here and `Ok(false)` from
    /// [`transition_batch`] — the same idempotence rule `lock_recipe_version` uses.
    pub fn can_move_to(self, to: Self) -> bool {
        matches!(
            (self, to),
            (Self::Planned, Self::Fermenting)
                | (Self::Fermenting, Self::Conditioning)
                | (Self::Conditioning, Self::Packaged)
        )
    }
}

// ----------------------------------------------------------------- batch

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewBatch {
    pub code: String,
    /// `None` for an experiment with no written recipe. [`plan_vs_actual`] then
    /// reports every addition as unplanned instead of failing.
    pub recipe_version_id: Option<i64>,
    pub beverage_type: BeverageType,
    pub vessel_id: Option<i64>,
    pub volume_l: Option<Litres>,
    pub og: Option<Sg>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchRow {
    pub id: i64,
    pub code: String,
    pub recipe_version_id: Option<i64>,
    pub beverage_type: BeverageType,
    pub vessel_id: Option<i64>,
    pub status: BatchStatus,
    pub volume_l: Option<Litres>,
    pub og: Option<Sg>,
    pub fg: Option<Sg>,
    pub pitched_at: Option<Timestamp>,
    pub archived_at: Option<Timestamp>,
    pub notes: Option<String>,
}

const BATCH_COLUMNS: &str = "id, code, recipe_version_id, beverage_type, vessel_id, status, \
                             volume_l, og, fg, pitched_at, archived_at, notes";

fn batch_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchRow> {
    Ok(BatchRow {
        id: row.get(0)?,
        code: row.get(1)?,
        recipe_version_id: row.get(2)?,
        beverage_type: row.get(3)?,
        vessel_id: row.get(4)?,
        status: row.get(5)?,
        volume_l: row.get(6)?,
        og: row.get(7)?,
        fg: row.get(8)?,
        pitched_at: row.get(9)?,
        archived_at: row.get(10)?,
        notes: row.get(11)?,
    })
}

/// Insert a batch, freezing the recipe version it points at.
///
/// `at` is when the lock is recorded, and only matters for the first batch off a
/// version — [`lock_recipe_version`] keeps the earliest. The version is checked
/// against the batch's own `beverage_type` first: they are two columns stating one
/// fact, and a seltzer batch of a beer recipe is a typo that would otherwise decide
/// which side tables apply in Phase 4.
pub fn create_batch(conn: &Connection, batch: &NewBatch, at: Timestamp) -> Result<i64> {
    if let Some(version_id) = batch.recipe_version_id {
        let version = get_recipe_version(conn, version_id)?;
        let recipe: BeverageType = conn.query_row(
            "SELECT beverage_type FROM recipe WHERE id = ?",
            [version.recipe_id],
            |row| row.get(0),
        )?;
        if recipe != batch.beverage_type {
            return Err(Error::BeverageTypeMismatch {
                batch: batch.beverage_type,
                recipe,
            });
        }
        lock_recipe_version(conn, version_id, at)?;
    }

    conn.execute(
        "INSERT INTO batch \
         (code, recipe_version_id, beverage_type, vessel_id, status, volume_l, og, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            batch.code,
            batch.recipe_version_id,
            batch.beverage_type,
            batch.vessel_id,
            BatchStatus::Planned,
            batch.volume_l,
            batch.og,
            batch.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The next unused code in this beverage type's series, as a suggestion.
///
/// `SELTZER-018` follows `SELTZER-017`, and a beer batch is numbered apart. Codes
/// that do not fit the pattern are skipped rather than rejected — the series belongs
/// to whoever types it, and `UNIQUE` is the only thing that actually enforces
/// anything here.
pub fn suggest_batch_code(conn: &Connection, beverage_type: BeverageType) -> Result<String> {
    let prefix = format!("{}-", beverage_type.as_str().to_uppercase());

    let mut stmt = conn.prepare("SELECT code FROM batch WHERE code LIKE ?1 || '%'")?;
    let highest = stmt
        .query_map([&prefix], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .filter_map(|code| code[prefix.len()..].parse::<u32>().ok())
        .max()
        .unwrap_or(0);

    Ok(format!("{prefix}{:03}", highest + 1))
}

pub fn get_batch(conn: &Connection, batch_id: i64) -> Result<BatchRow> {
    conn.query_row(
        &format!("SELECT {BATCH_COLUMNS} FROM batch WHERE id = ?"),
        [batch_id],
        batch_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchBatch { batch_id })
}

pub fn list_batches(conn: &Connection, include_archived: bool) -> Result<Vec<BatchRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BATCH_COLUMNS} FROM batch WHERE ?1 OR archived_at IS NULL ORDER BY code"
    ))?;
    let rows = stmt.query_map([include_archived], batch_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Record t=0. Separate from the transition because the pitch is a fact about the
/// batch and the transition is a fact about the process — 3d's `pitch` event will
/// write this column and then move the status, in that order.
pub fn set_pitched_at(conn: &Connection, batch_id: i64, at: Timestamp) -> Result<()> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;
    conn.execute(
        "UPDATE batch SET pitched_at = ? WHERE id = ?",
        params![at, batch_id],
    )?;
    Ok(())
}

/// Volume, OG and FG as they actually came out. `None` leaves a value alone.
pub fn set_batch_measurements(
    conn: &Connection,
    batch_id: i64,
    volume_l: Option<Litres>,
    og: Option<Sg>,
    fg: Option<Sg>,
) -> Result<()> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;
    conn.execute(
        "UPDATE batch SET volume_l = COALESCE(?, volume_l), og = COALESCE(?, og), \
                fg = COALESCE(?, fg) \
         WHERE id = ?",
        params![volume_l, og, fg, batch_id],
    )?;
    Ok(())
}

pub(crate) fn ensure_open(batch: &BatchRow) -> Result<()> {
    match batch.archived_at {
        None => Ok(()),
        Some(_) => Err(Error::BatchArchived { batch_id: batch.id }),
    }
}

/// Move a batch along the chain. `false` means it was already there.
///
/// `planned → fermenting` needs `pitched_at`: pitch is what Phase 5's overlay aligns
/// on, what the finish detector measures from and what lag time is derived from, so a
/// batch that ferments without one can never be compared to anything. Discovering
/// that afterwards is not a fix.
pub fn transition_batch(conn: &Connection, batch_id: i64, to: BatchStatus) -> Result<bool> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;

    if batch.status == to {
        return Ok(false);
    }
    if !batch.status.can_move_to(to) {
        return Err(Error::IllegalBatchTransition {
            batch_id,
            from: batch.status,
            to,
        });
    }
    if to == BatchStatus::Fermenting && batch.pitched_at.is_none() {
        return Err(Error::BatchNotPitched { batch_id });
    }

    conn.execute(
        "UPDATE batch SET status = ? WHERE id = ?",
        params![to, batch_id],
    )?;
    Ok(true)
}

/// Returns whether a row changed. A batch is never hard-deleted: it is the record of
/// something that physically happened, and its readings are bound to it.
pub fn archive_batch(conn: &Connection, batch_id: i64, at: Timestamp) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE batch SET archived_at = ? WHERE id = ? AND archived_at IS NULL",
        params![at, batch_id],
    )?;
    Ok(changed == 1)
}

// ------------------------------------------------------ batch_ingredient

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewBatchIngredient {
    /// The lot, not the ingredient: traceability and cost are the same join.
    pub ingredient_lot_id: i64,
    /// As typed. The canonical column comes from the lot's ingredient.
    pub entry: Quantity,
    pub stage: Stage,
    /// Absolute, not an offset from pitch — an addition can precede the pitch.
    pub added_at: Option<Timestamp>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchIngredientRow {
    pub id: i64,
    pub batch_id: i64,
    pub ingredient_lot_id: i64,
    /// Joined through the lot, because pairing against the plan needs it and every
    /// caller would otherwise write the same join.
    pub ingredient_id: i64,
    pub qty: CanonicalQty,
    pub entry: Quantity,
    pub stage: Stage,
    pub added_at: Option<Timestamp>,
    pub notes: Option<String>,
}

/// The lot's ingredient, then the ordinary dimension check.
///
/// A wrapper rather than a copy of [`canonical_for_ingredient`]: a volume landing in
/// `qty_g` has to be impossible from every write path, not from two of the three.
fn canonical_for_lot(
    conn: &Connection,
    lot_id: i64,
    entry: Quantity,
) -> Result<(Option<Grams>, Option<Litres>, Option<f64>)> {
    let ingredient_id: i64 = conn
        .query_row(
            "SELECT ingredient_id FROM ingredient_lot WHERE id = ?",
            [lot_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NoSuchLot { lot_id })?;

    canonical_for_ingredient(conn, ingredient_id, entry)
}

pub fn add_batch_ingredient(
    conn: &Connection,
    batch_id: i64,
    item: &NewBatchIngredient,
) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;
    let (qty_g, qty_l, qty_each) = canonical_for_lot(conn, item.ingredient_lot_id, item.entry)?;

    conn.execute(
        "INSERT INTO batch_ingredient \
         (batch_id, ingredient_lot_id, qty_g, qty_l, qty_each, entry_qty, entry_unit, \
          stage, added_at, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            batch_id,
            item.ingredient_lot_id,
            qty_g,
            qty_l,
            qty_each,
            item.entry.value,
            item.entry.unit,
            item.stage,
            item.added_at,
            item.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_batch_ingredient(conn: &Connection, id: i64) -> Result<bool> {
    let changed = conn.execute("DELETE FROM batch_ingredient WHERE id = ?", [id])?;
    Ok(changed == 1)
}

/// In the order it happened. An addition with no recorded time sorts first, the same
/// convention `list_recipe_items` uses for an item with no offset.
pub fn list_batch_ingredients(conn: &Connection, batch_id: i64) -> Result<Vec<BatchIngredientRow>> {
    let mut stmt = conn.prepare(
        "SELECT bi.id, bi.batch_id, bi.ingredient_lot_id, lot.ingredient_id, \
                bi.qty_g, bi.qty_l, bi.qty_each, bi.entry_qty, bi.entry_unit, \
                bi.stage, bi.added_at, bi.notes \
         FROM batch_ingredient bi JOIN ingredient_lot lot ON lot.id = bi.ingredient_lot_id \
         WHERE bi.batch_id = ? \
         ORDER BY bi.added_at IS NULL DESC, bi.added_at, bi.id",
    )?;
    let rows = stmt.query_map([batch_id], |row| {
        Ok(BatchIngredientRow {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            ingredient_lot_id: row.get(2)?,
            ingredient_id: row.get(3)?,
            qty: canonical_from_columns(4, row.get(4)?, row.get(5)?, row.get(6)?)?,
            entry: Quantity {
                value: row.get(7)?,
                unit: row.get(8)?,
            },
            stage: row.get(9)?,
            added_at: row.get(10)?,
            notes: row.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// -------------------------------------------------------- plan vs actual

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum PlanVsActualRow {
    /// Planned and never added.
    Missing(RecipeItemRow),
    /// Added without being planned — every row of a batch that has no recipe.
    Unplanned(BatchIngredientRow),
    Both {
        planned: RecipeItemRow,
        actual: BatchIngredientRow,
        /// Actual minus planned, in the canonical unit. `None` when the two are in
        /// different dimensions, which only a hand-edited database can produce.
        delta: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PlanVsActual {
    pub batch_id: i64,
    pub recipe_version_id: Option<i64>,
    pub rows: Vec<PlanVsActualRow>,
}

/// Pair the plan against what actually went in.
///
/// Keyed on `(ingredient_id, stage, occurrence)` — the same key
/// [`diff_items`](crate::db::recipe::diff_items) uses, and not the time: a nutrient
/// addition that slipped six hours is the single most interesting row here, and
/// keying on when it happened would split it into an omission plus a surprise.
pub fn plan_vs_actual(conn: &Connection, batch_id: i64) -> Result<PlanVsActual> {
    let batch = get_batch(conn, batch_id)?;
    let actuals = list_batch_ingredients(conn, batch_id)?;
    let planned = match batch.recipe_version_id {
        Some(version_id) => list_recipe_items(conn, version_id)?,
        None => Vec::new(),
    };

    /// `(ingredient_id, stage, occurrence)`, assigned in list order.
    fn keyed<T>(items: &[T], of: impl Fn(&T) -> (i64, Stage)) -> Vec<(i64, Stage, usize)> {
        let mut keys: Vec<(i64, Stage, usize)> = Vec::new();
        for item in items {
            let (ingredient_id, stage) = of(item);
            let occurrence = keys
                .iter()
                .filter(|(id, s, _)| *id == ingredient_id && *s == stage)
                .count();
            keys.push((ingredient_id, stage, occurrence));
        }
        keys
    }

    let plan_keys = keyed(&planned, |item| (item.ingredient_id, item.stage));
    let actual_keys = keyed(&actuals, |item| (item.ingredient_id, item.stage));
    let mut rows = Vec::new();

    for (key, item) in plan_keys.iter().zip(&planned) {
        match actual_keys.iter().position(|k| k == key) {
            None => rows.push(PlanVsActualRow::Missing(item.clone())),
            Some(index) => {
                let actual = actuals[index].clone();
                let delta = (actual.qty.dimension() == item.qty.dimension())
                    .then(|| actual.qty.amount() - item.qty.amount());
                rows.push(PlanVsActualRow::Both {
                    planned: item.clone(),
                    actual,
                    delta,
                });
            }
        }
    }
    for (key, item) in actual_keys.iter().zip(&actuals) {
        if !plan_keys.contains(key) {
            rows.push(PlanVsActualRow::Unplanned(item.clone()));
        }
    }

    Ok(PlanVsActual {
        batch_id,
        recipe_version_id: batch.recipe_version_id,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{
            recipe::{
                NewRecipeItem, NewRecipeVersion, add_recipe_item, create_recipe,
                create_recipe_version,
            },
            reference::{Category, NewIngredient, NewLot, create_ingredient, create_lot},
        },
        units::Unit,
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn ingredient(conn: &Connection, name: &str, category: Category, unit: Unit) -> i64 {
        create_ingredient(
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
        .unwrap()
    }

    /// An ingredient with one lot of it, which is what an actual points at.
    fn stocked(conn: &Connection, name: &str, category: Category, unit: Unit) -> (i64, i64) {
        let ingredient_id = ingredient(conn, name, category, unit);
        let lot = create_lot(
            conn,
            &NewLot {
                ingredient_id,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(25.0, unit).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        (ingredient_id, lot)
    }

    /// A seltzer recipe with one version. The shape most tests below start from.
    fn seltzer_recipe(conn: &Connection) -> i64 {
        let recipe = create_recipe(conn, "Citron-Verveine", BeverageType::Seltzer, None).unwrap();
        create_recipe_version(
            conn,
            recipe,
            &NewRecipeVersion {
                target_volume_l: Some(Litres::new(20.0).unwrap()),
                target_og: Some(Sg::new(1.040).unwrap()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn new_batch(code: &str, version: Option<i64>) -> NewBatch {
        NewBatch {
            code: code.into(),
            recipe_version_id: version,
            beverage_type: BeverageType::Seltzer,
            vessel_id: None,
            volume_l: None,
            og: None,
            notes: None,
        }
    }

    fn actual(lot: i64, qty: f64, unit: Unit, stage: Stage) -> NewBatchIngredient {
        NewBatchIngredient {
            ingredient_lot_id: lot,
            entry: Quantity::new(qty, unit).unwrap(),
            stage,
            added_at: None,
            notes: None,
        }
    }

    /// 3b's whole point, finally wired: the version stops moving the moment a batch
    /// depends on it.
    #[test]
    fn creating_a_batch_freezes_the_recipe_version_it_points_at() {
        let conn = db();
        let version = seltzer_recipe(&conn);
        assert!(!get_recipe_version(&conn, version).unwrap().is_locked());

        create_batch(&conn, &new_batch("SELTZER-001", Some(version)), T0).unwrap();
        assert_eq!(
            get_recipe_version(&conn, version).unwrap().locked_at,
            Some(T0)
        );

        // A second batch off the same version is the normal case, and the lock keeps
        // recording the first.
        let later = Timestamp::from_secs(1_800_000_000);
        create_batch(&conn, &new_batch("SELTZER-002", Some(version)), later).unwrap();
        assert_eq!(
            get_recipe_version(&conn, version).unwrap().locked_at,
            Some(T0)
        );
    }

    #[test]
    fn a_batch_cannot_leave_planned_without_a_pitch_time() {
        let conn = db();
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();

        let err = transition_batch(&conn, batch, BatchStatus::Fermenting)
            .expect_err("pitch is t=0 for everything downstream");
        assert_eq!(err.code(), "db.batch_not_pitched");
        assert_eq!(
            get_batch(&conn, batch).unwrap().status,
            BatchStatus::Planned
        );

        set_pitched_at(&conn, batch, T0).unwrap();
        assert!(transition_batch(&conn, batch, BatchStatus::Fermenting).unwrap());
        assert_eq!(
            get_batch(&conn, batch).unwrap().status,
            BatchStatus::Fermenting
        );
    }

    #[test]
    fn the_status_machine_refuses_a_backward_or_skipping_move() {
        let conn = db();
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        set_pitched_at(&conn, batch, T0).unwrap();

        let err = transition_batch(&conn, batch, BatchStatus::Packaged).expect_err("no skipping");
        assert_eq!(err.code(), "db.illegal_batch_transition");

        // Already there is not an error, it is nothing to do.
        assert!(!transition_batch(&conn, batch, BatchStatus::Planned).unwrap());

        transition_batch(&conn, batch, BatchStatus::Fermenting).unwrap();
        transition_batch(&conn, batch, BatchStatus::Conditioning).unwrap();
        let err = transition_batch(&conn, batch, BatchStatus::Fermenting)
            .expect_err("a batch does not un-condition");
        assert_eq!(err.code(), "db.illegal_batch_transition");
        assert!(
            err.to_string().contains("conditioning"),
            "unexpected: {err}"
        );

        transition_batch(&conn, batch, BatchStatus::Packaged).unwrap();
        assert_eq!(
            get_batch(&conn, batch).unwrap().status,
            BatchStatus::Packaged
        );
    }

    #[test]
    fn an_archived_batch_takes_no_further_changes() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        assert!(archive_batch(&conn, batch, T0).unwrap());
        assert!(!archive_batch(&conn, batch, T0).unwrap());

        for err in [
            transition_batch(&conn, batch, BatchStatus::Fermenting).unwrap_err(),
            set_pitched_at(&conn, batch, T0).unwrap_err(),
            add_batch_ingredient(&conn, batch, &actual(lot, 2.5, Unit::Kg, Stage::Prep))
                .unwrap_err(),
        ] {
            assert_eq!(err.code(), "db.batch_archived");
        }
    }

    #[test]
    fn a_batch_whose_beverage_type_contradicts_its_recipe_is_refused() {
        let conn = db();
        let version = seltzer_recipe(&conn);

        let err = create_batch(
            &conn,
            &NewBatch {
                beverage_type: BeverageType::Beer,
                ..new_batch("BEER-001", Some(version))
            },
            T0,
        )
        .expect_err("a beer batch of a seltzer recipe is a typo");
        assert_eq!(err.code(), "db.beverage_type_mismatch");
        // Refused before the lock: the version is still editable.
        assert!(!get_recipe_version(&conn, version).unwrap().is_locked());
        assert!(list_batches(&conn, true).unwrap().is_empty());
    }

    #[test]
    fn the_next_code_continues_the_series_per_beverage_type() {
        let conn = db();
        assert_eq!(
            suggest_batch_code(&conn, BeverageType::Seltzer).unwrap(),
            "SELTZER-001"
        );

        create_batch(&conn, &new_batch("SELTZER-017", None), T0).unwrap();
        assert_eq!(
            suggest_batch_code(&conn, BeverageType::Seltzer).unwrap(),
            "SELTZER-018"
        );
        // A hand-typed code in the series is skipped, not parsed into nonsense.
        create_batch(&conn, &new_batch("SELTZER-experiment", None), T0).unwrap();
        assert_eq!(
            suggest_batch_code(&conn, BeverageType::Seltzer).unwrap(),
            "SELTZER-018"
        );
        // Another type numbers apart.
        assert_eq!(
            suggest_batch_code(&conn, BeverageType::Beer).unwrap(),
            "BEER-001"
        );
    }

    #[test]
    fn a_duplicate_batch_code_is_refused_by_the_schema() {
        let conn = db();
        create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        let err =
            create_batch(&conn, &new_batch("SELTZER-001", None), T0).expect_err("codes are unique");
        assert!(err.to_string().contains("UNIQUE"), "unexpected: {err}");
    }

    #[test]
    fn an_actual_entered_in_the_wrong_dimension_is_refused() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();

        let err = add_batch_ingredient(&conn, batch, &actual(lot, 2.5, Unit::L, Stage::Prep))
            .expect_err("sucrose is a mass");
        assert_eq!(err.code(), "units.dimension_mismatch");
        assert!(list_batch_ingredients(&conn, batch).unwrap().is_empty());
    }

    #[test]
    fn an_actual_entered_in_kg_is_stored_in_grams_and_prints_back_in_kg() {
        let conn = db();
        let (ingredient_id, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(lot, 2.5, Unit::Kg, Stage::Prep)).unwrap();

        let rows = list_batch_ingredients(&conn, batch).unwrap();
        assert_eq!(rows[0].qty.amount(), 2500.0);
        assert_eq!(rows[0].entry.to_string(), "2.5 kg");
        // Joined through the lot, so a caller can pair it against the plan.
        assert_eq!(rows[0].ingredient_id, ingredient_id);
    }

    #[test]
    fn an_actual_against_a_lot_that_does_not_exist_is_named() {
        let conn = db();
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        let err = add_batch_ingredient(&conn, batch, &actual(404, 1.0, Unit::G, Stage::Prep))
            .expect_err("no such lot");
        assert_eq!(err.code(), "db.no_such_lot");
    }

    /// The row a sugar wash is diagnosed from: less nutrient than planned, and six
    /// hours late. Keyed on the time, it would read as an omission plus a surprise.
    #[test]
    fn a_nutrient_addition_that_came_up_short_is_one_row_with_a_delta() {
        let conn = db();
        let version = seltzer_recipe(&conn);
        let (nutrient, nutrient_lot) = stocked(&conn, "Fermaid O", Category::Nutrient, Unit::G);
        add_recipe_item(
            &conn,
            version,
            &NewRecipeItem {
                ingredient_id: nutrient,
                entry: Quantity::new(5.0, Unit::G).unwrap(),
                stage: Stage::Fermentation,
                offset_hours: Some(24.0),
                notes: None,
            },
        )
        .unwrap();

        let batch = create_batch(&conn, &new_batch("SELTZER-001", Some(version)), T0).unwrap();
        add_batch_ingredient(
            &conn,
            batch,
            &NewBatchIngredient {
                added_at: Some(Timestamp::from_secs(1_700_000_000 + 30 * 3600)),
                ..actual(nutrient_lot, 4.0, Unit::G, Stage::Fermentation)
            },
        )
        .unwrap();

        let comparison = plan_vs_actual(&conn, batch).unwrap();
        assert_eq!(comparison.rows.len(), 1, "got {:?}", comparison.rows);
        match &comparison.rows[0] {
            PlanVsActualRow::Both { delta, .. } => assert_eq!(*delta, Some(-1.0)),
            other => panic!("expected one paired row, got {other:?}"),
        }
    }

    #[test]
    fn a_planned_item_never_added_and_an_addition_never_planned_both_show() {
        let conn = db();
        let version = seltzer_recipe(&conn);
        let (sugar, _) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let (_, acid_lot) = stocked(&conn, "Citric acid", Category::Acid, Unit::G);
        add_recipe_item(
            &conn,
            version,
            &NewRecipeItem {
                ingredient_id: sugar,
                entry: Quantity::new(2.5, Unit::Kg).unwrap(),
                stage: Stage::Prep,
                offset_hours: None,
                notes: None,
            },
        )
        .unwrap();

        let batch = create_batch(&conn, &new_batch("SELTZER-001", Some(version)), T0).unwrap();
        add_batch_ingredient(
            &conn,
            batch,
            &actual(acid_lot, 12.0, Unit::G, Stage::Conditioning),
        )
        .unwrap();

        let rows = plan_vs_actual(&conn, batch).unwrap().rows;
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], PlanVsActualRow::Missing(_)));
        assert!(matches!(rows[1], PlanVsActualRow::Unplanned(_)));
    }

    #[test]
    fn a_batch_with_no_recipe_version_reports_every_actual_as_unplanned() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(lot, 2.5, Unit::Kg, Stage::Prep)).unwrap();

        let comparison = plan_vs_actual(&conn, batch).unwrap();
        assert_eq!(comparison.recipe_version_id, None);
        assert!(matches!(
            comparison.rows.as_slice(),
            [PlanVsActualRow::Unplanned(_)]
        ));
    }

    /// `status` carries no `CHECK`, so the enum is the guarantee.
    #[test]
    fn an_unknown_status_string_in_the_database_is_an_error_not_a_panic() {
        let conn = db();
        conn.execute(
            "INSERT INTO batch (code, beverage_type, status) VALUES ('X-001', 'seltzer', 'krausen')",
            [],
        )
        .unwrap();

        let err = list_batches(&conn, true).expect_err("unknown status");
        assert_eq!(err.code(), "db.sqlite");
        assert!(err.to_string().contains("krausen"), "unexpected: {err}");
    }

    #[test]
    fn archiving_a_batch_hides_it_without_deleting_its_ingredients() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(lot, 2.5, Unit::Kg, Stage::Prep)).unwrap();
        archive_batch(&conn, batch, T0).unwrap();

        assert!(list_batches(&conn, false).unwrap().is_empty());
        assert_eq!(list_batches(&conn, true).unwrap()[0].archived_at, Some(T0));
        assert_eq!(list_batch_ingredients(&conn, batch).unwrap().len(), 1);
    }

    #[test]
    fn measurements_are_patched_not_replaced() {
        let conn = db();
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        set_batch_measurements(&conn, batch, Some(Litres::new(19.5).unwrap()), None, None).unwrap();
        set_batch_measurements(&conn, batch, None, None, Some(Sg::new(1.000).unwrap())).unwrap();

        let row = get_batch(&conn, batch).unwrap();
        assert_eq!(row.volume_l, Some(Litres::new(19.5).unwrap()));
        assert_eq!(row.fg, Some(Sg::new(1.000).unwrap()));
    }
}
