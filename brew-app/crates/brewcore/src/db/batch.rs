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
    reference::{
        CanonicalQty, canonical_from_columns, duplicate, positive_canonical, required_name,
        restore_row,
    },
};
use crate::{
    Error, Result, calc,
    units::{Currency, Grams, Litres, Money, Quantity, Sg, Timestamp},
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

pub(crate) const BATCH_COLUMNS: &str = "id, code, recipe_version_id, beverage_type, vessel_id, status, \
                             volume_l, og, fg, pitched_at, archived_at, notes";

pub(crate) fn batch_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchRow> {
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
    let code = required_name("batch code", &batch.code)?;
    check_volume(batch.volume_l)?;
    if let Some(og) = batch.og.filter(|og| !og.is_plausible()) {
        return Err(Error::MeasurementImplausible {
            metric: crate::db::Metric::Sg,
            value: og.get(),
        });
    }

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
            code,
            batch.recipe_version_id,
            batch.beverage_type,
            batch.vessel_id,
            BatchStatus::Planned,
            batch.volume_l,
            batch.og,
            batch.notes,
        ],
    )
    .map_err(duplicate("batch", code))?;
    Ok(conn.last_insert_rowid())
}

fn check_volume(volume_l: Option<Litres>) -> Result<()> {
    match volume_l.filter(|v| v.get() <= 0.0) {
        Some(v) => Err(Error::NotPositive {
            field: "volume_l",
            value: v.get(),
        }),
        None => Ok(()),
    }
}

/// What can change about a batch after it is created. The recipe version and the
/// beverage type cannot: the version was locked against this batch's type, and the way
/// out of a wrong pick is to archive the batch and create another.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BatchEdit {
    pub code: String,
    pub vessel_id: Option<i64>,
    pub volume_l: Option<Litres>,
    pub notes: Option<String>,
}

/// Replaces all four fields, so a volume or a note can be cleared — unlike
/// [`set_batch_measurements`], which patches.
pub fn update_batch(conn: &Connection, batch_id: i64, edit: &BatchEdit) -> Result<()> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;
    let code = required_name("batch code", &edit.code)?;
    check_volume(edit.volume_l)?;
    if batch.pitched_at.is_some() && edit.vessel_id != batch.vessel_id {
        return Err(Error::BatchVesselLocked { batch_id });
    }
    conn.execute(
        "UPDATE batch SET code = ?, vessel_id = ?, volume_l = ?, notes = ? WHERE id = ?",
        params![code, edit.vessel_id, edit.volume_l, edit.notes, batch_id],
    )
    .map_err(duplicate("batch", code))?;
    Ok(())
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

/// A batch with the names a list shows beside it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchSummary {
    #[serde(flatten)]
    pub batch: BatchRow,
    pub recipe_id: Option<i64>,
    pub recipe_name: Option<String>,
    pub version_no: Option<i64>,
    pub vessel_name: Option<String>,
}

fn summaries_where(
    conn: &Connection,
    filter: &str,
    arg: &dyn rusqlite::ToSql,
) -> Result<Vec<BatchSummary>> {
    let columns = BATCH_COLUMNS
        .split(',')
        .map(|c| format!("b.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {columns}, r.id, r.name, v.version_no, ves.name          FROM batch b          LEFT JOIN recipe_version v ON v.id = b.recipe_version_id          LEFT JOIN recipe r ON r.id = v.recipe_id          LEFT JOIN vessel ves ON ves.id = b.vessel_id          WHERE {filter} ORDER BY b.code"
    ))?;
    let rows = stmt.query_map([arg], |row| {
        Ok(BatchSummary {
            batch: batch_from_row(row)?,
            recipe_id: row.get(12)?,
            recipe_name: row.get(13)?,
            version_no: row.get(14)?,
            vessel_name: row.get(15)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn list_batch_summaries(
    conn: &Connection,
    include_archived: bool,
) -> Result<Vec<BatchSummary>> {
    summaries_where(conn, "?1 OR b.archived_at IS NULL", &include_archived)
}

/// One batch as the list shows it, archived or not: the batch screen's header.
pub fn get_batch_summary(conn: &Connection, batch_id: i64) -> Result<BatchSummary> {
    summaries_where(conn, "b.id = ?1", &batch_id)?
        .pop()
        .ok_or(Error::NoSuchBatch { batch_id })
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

/// Which of the batch's two decided gravities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GravityField {
    Og,
    Fg,
}

/// Set or clear `batch.og` / `batch.fg` exactly, unlike [`set_batch_measurements`]'s
/// patch: a mistaken OG has to be removable, not only replaceable.
///
/// Checked here: the value is a plausible SG and FG ≤ OG. And an OG that post-pitch
/// refractometer readings were corrected against does not move — those readings stored
/// a value derived from it, and nothing would recompute them. [`Error::OgInUse`] says
/// how many to delete first.
pub fn set_batch_gravity(
    conn: &Connection,
    batch_id: i64,
    field: GravityField,
    value: Option<Sg>,
) -> Result<()> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;
    if let Some(sg) = value.filter(|sg| !sg.is_plausible()) {
        return Err(Error::MeasurementImplausible {
            metric: crate::db::Metric::Sg,
            value: sg.get(),
        });
    }
    let (og, fg) = match field {
        GravityField::Og => (value, batch.fg),
        GravityField::Fg => (batch.og, value),
    };
    if let (Some(og), Some(fg)) = (og, fg) {
        if fg.get() > og.get() {
            return Err(Error::RangeInverted { field: "gravity" });
        }
    }
    if field == GravityField::Og && value != batch.og {
        let readings = corrected_against_og(conn, &batch)?;
        if readings > 0 {
            return Err(Error::OgInUse { batch_id, readings });
        }
    }

    let column = match field {
        GravityField::Og => "og",
        GravityField::Fg => "fg",
    };
    conn.execute(
        &format!("UPDATE batch SET {column} = ? WHERE id = ?"),
        params![value, batch_id],
    )?;
    Ok(())
}

/// The refractometer readings `derive_gravity` ran through Terrill's cubic with this
/// batch's OG: on or after the pitch.
fn corrected_against_og(conn: &Connection, batch: &BatchRow) -> Result<i64> {
    let Some(pitched_at) = batch.pitched_at else {
        return Ok(0);
    };
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM manual_measurement \
         WHERE batch_id = ? AND instrument = 'refractometer' AND ts >= ?",
        params![batch.id, pitched_at],
        |row| row.get(0),
    )?)
}

pub(crate) fn ensure_open(batch: &BatchRow) -> Result<()> {
    match batch.archived_at {
        None => Ok(()),
        Some(_) => Err(Error::BatchArchived { batch_id: batch.id }),
    }
}

/// Move a batch along the chain at `at`. `false` means it was already there.
///
/// `planned → fermenting` needs `pitched_at`: pitch is what Phase 5's overlay aligns
/// on, what the finish detector measures from and what lag time is derived from, so a
/// batch that ferments without one can never be compared to anything. Discovering
/// that afterwards is not a fix.
///
/// `→ packaged` closes the batch's open sensor bindings at `at`: the beer has left the
/// vessel, and the vessel's sensors belong to whatever is pitched into it next. See
/// [`close_sensor_bindings`](super::timeline::close_sensor_bindings).
pub fn transition_batch(
    conn: &Connection,
    batch_id: i64,
    to: BatchStatus,
    at: Timestamp,
) -> Result<bool> {
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
    match batch.pitched_at {
        None if to == BatchStatus::Fermenting => return Err(Error::BatchNotPitched { batch_id }),
        Some(pitched_at) if at < pitched_at => return Err(Error::BeforePitch { batch_id }),
        _ => {}
    }

    conn.execute(
        "UPDATE batch SET status = ? WHERE id = ?",
        params![to, batch_id],
    )?;
    if to == BatchStatus::Packaged {
        super::timeline::close_sensor_bindings(conn, batch_id, at)?;
    }
    Ok(true)
}

/// Returns whether a row changed. A batch is never hard-deleted: it is the record of
/// something that physically happened, and its readings are bound to it.
///
/// Closes the batch's open sensor bindings at `at`, as packaging does. A batch
/// abandoned mid-ferment would otherwise hold its vessel's sensors forever, and the
/// next pitch into that vessel would fail on them.
pub fn archive_batch(conn: &Connection, batch_id: i64, at: Timestamp) -> Result<bool> {
    get_batch(conn, batch_id)?;
    let changed = conn.execute(
        "UPDATE batch SET archived_at = ? WHERE id = ? AND archived_at IS NULL",
        params![at, batch_id],
    )?;
    if changed == 1 {
        super::timeline::close_sensor_bindings(conn, batch_id, at)?;
    }
    Ok(changed == 1)
}

/// The bindings archiving closed stay closed: the sensors may have moved on to the
/// next batch in the meantime.
pub fn restore_batch(conn: &Connection, batch_id: i64) -> Result<bool> {
    get_batch(conn, batch_id)?;
    restore_row(conn, "batch", batch_id)
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

/// The lot's ingredient, then the ordinary dimension check and a positive amount.
///
/// A wrapper rather than a copy of [`canonical_for_ingredient`]: a volume landing in
/// `qty_g` has to be impossible from every write path, not from two of the three.
/// `keep` is a lot an existing addition already uses: correcting the quantity of an
/// addition from a lot archived since is a fix, not a new pick.
fn canonical_for_lot(
    conn: &Connection,
    lot_id: i64,
    entry: Quantity,
    keep: Option<i64>,
) -> Result<(Option<Grams>, Option<Litres>, Option<f64>)> {
    let (ingredient_id, archived): (i64, bool) = conn
        .query_row(
            "SELECT ingredient_id, archived_at IS NOT NULL FROM ingredient_lot WHERE id = ?",
            [lot_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NoSuchLot { lot_id })?;
    if archived && keep != Some(lot_id) {
        return Err(Error::LotArchived { lot_id });
    }

    positive_canonical(conn, ingredient_id, entry)
}

pub fn add_batch_ingredient(
    conn: &Connection,
    batch_id: i64,
    item: &NewBatchIngredient,
) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;
    let (qty_g, qty_l, qty_each) =
        canonical_for_lot(conn, item.ingredient_lot_id, item.entry, None)?;

    conn.execute(
        "INSERT INTO batch_ingredient          (batch_id, ingredient_lot_id, qty_g, qty_l, qty_each, entry_qty, entry_unit,           stage, added_at, notes)          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

/// The batch an addition belongs to, open.
fn open_batch_of(conn: &Connection, id: i64) -> Result<(i64, i64)> {
    let (batch_id, lot_id): (i64, i64) = conn
        .query_row(
            "SELECT batch_id, ingredient_lot_id FROM batch_ingredient WHERE id = ?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NoSuchBatchIngredient { id })?;
    ensure_open(&get_batch(conn, batch_id)?)?;
    Ok((batch_id, lot_id))
}

/// Replace an addition, under the same checks as [`add_batch_ingredient`].
pub fn update_batch_ingredient(
    conn: &Connection,
    id: i64,
    item: &NewBatchIngredient,
) -> Result<()> {
    let (_, current_lot) = open_batch_of(conn, id)?;
    let (qty_g, qty_l, qty_each) =
        canonical_for_lot(conn, item.ingredient_lot_id, item.entry, Some(current_lot))?;
    conn.execute(
        "UPDATE batch_ingredient SET ingredient_lot_id = ?, qty_g = ?, qty_l = ?,                 qty_each = ?, entry_qty = ?, entry_unit = ?, stage = ?, added_at = ?, notes = ?          WHERE id = ?",
        params![
            item.ingredient_lot_id,
            qty_g,
            qty_l,
            qty_each,
            item.entry.value,
            item.entry.unit,
            item.stage,
            item.added_at,
            item.notes,
            id,
        ],
    )?;
    Ok(())
}

pub fn delete_batch_ingredient(conn: &Connection, id: i64) -> Result<bool> {
    open_batch_of(conn, id)?;
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

// ------------------------------------------------------------------ cost

/// What a batch cost, from the lots its additions came from.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct BatchCost {
    pub batch_id: i64,
    /// The batch's own volume, or the recipe version's target when it has none.
    pub volume_l: Option<Litres>,
    pub total: Option<Money>,
    /// `None` when there is no total or no volume to divide it by.
    pub per_litre: Option<Money>,
    pub lines_costed: usize,
    pub lines_without_cost: usize,
}

/// Cost per batch and per litre — 3f's last item, and Phase 7's first.
///
/// Its own `SELECT` rather than a wider [`BatchIngredientRow`]: that struct is on the
/// command surface, and what a lot cost is not part of what an addition *is*. The
/// arithmetic is [`crate::calc::cost`], which is where it can be tested without a
/// database; this function's whole job is turning rows into
/// [`CostLine`](crate::calc::cost::CostLine)s.
///
/// A lot with no price becomes a `lines_without_cost` rather than a zero. The
/// per-unit price comes from
/// [`LotRow::cost_per_canonical_unit`](crate::db::reference::LotRow::cost_per_canonical_unit),
/// so a sub-cent
/// bulk price survives the trip; see its doc for why that is an `f64`.
/// The batch's own volume, else its recipe version's target. What a cost, and 5a's
/// nitrogen, divide by.
pub(crate) fn target_or_batch_volume(
    conn: &Connection,
    batch: &BatchRow,
) -> Result<Option<Litres>> {
    Ok(match (batch.volume_l, batch.recipe_version_id) {
        (Some(v), _) => Some(v),
        (None, Some(id)) => get_recipe_version(conn, id)?.target_volume_l,
        (None, None) => None,
    })
}

pub fn batch_cost(conn: &Connection, batch_id: i64) -> Result<BatchCost> {
    let batch = get_batch(conn, batch_id)?;

    let mut stmt = conn.prepare(
        "SELECT bi.qty_g, bi.qty_l, bi.qty_each, \
                lot.qty_g, lot.qty_l, lot.qty_each, \
                lot.total_cost_minor, lot.currency \
         FROM batch_ingredient bi JOIN ingredient_lot lot ON lot.id = bi.ingredient_lot_id \
         WHERE bi.batch_id = ?",
    )?;
    let rows = stmt.query_map([batch_id], |row| {
        let used = canonical_from_columns(0, row.get(0)?, row.get(1)?, row.get(2)?)?;
        let stock = canonical_from_columns(3, row.get(3)?, row.get(4)?, row.get(5)?)?;
        let cost = row
            .get::<_, Option<i64>>(6)?
            .zip(row.get::<_, Option<Currency>>(7)?)
            .map(|(minor, currency)| Money::new(minor, currency));
        Ok((used, stock, cost))
    })?;

    let mut lines = Vec::new();
    let mut lines_without_cost = 0usize;
    for row in rows {
        let (used, stock, cost) = row?;
        // Both quantities came from the same ingredient's `default_unit`, so they
        // are always the same dimension — `canonical_for_ingredient` is the only
        // write path for either. The price is minor units per *that* dimension.
        match cost.filter(|_| stock.amount() != 0.0) {
            Some(cost) => lines.push(calc::cost::CostLine {
                per_unit_minor: cost.minor as f64 / stock.amount(),
                currency: cost.currency,
                qty: used.amount(),
            }),
            None => lines_without_cost += 1,
        }
    }

    let rolled = calc::cost::total(lines, lines_without_cost)?;
    let volume_l = target_or_batch_volume(conn, &batch)?;
    let per_litre = match (rolled.total, volume_l) {
        (Some(total), Some(v)) if v.get() > 0.0 => Some(calc::cost::per_litre(total, v)?),
        _ => None,
    };

    Ok(BatchCost {
        batch_id,
        volume_l,
        total: rolled.total,
        per_litre,
        lines_costed: rolled.lines_costed,
        lines_without_cost: rolled.lines_without_cost,
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
        // A yeast must name its strain; one of the same name stands in.
        let yeast_strain_id = (category == Category::Yeast).then(|| {
            crate::db::reference::create_yeast_strain(
                conn,
                &crate::db::reference::NewYeastStrain {
                    name: name.into(),
                    ..Default::default()
                },
            )
            .unwrap()
        });
        create_ingredient(
            conn,
            &NewIngredient {
                name: name.into(),
                category,
                default_unit: unit,
                yeast_strain_id,
                attrs: None,
                notes: None,
            },
        )
        .unwrap()
    }

    /// An ingredient with one lot of it, which is what an actual points at.
    fn stocked(conn: &Connection, name: &str, category: Category, unit: Unit) -> (i64, i64) {
        priced(conn, name, category, unit, 25.0, None)
    }

    /// The same, with a price on the lot and its size spelled out — what
    /// [`batch_cost`] needs and what `stocked` deliberately does not have.
    fn priced(
        conn: &Connection,
        name: &str,
        category: Category,
        unit: Unit,
        size: f64,
        cost: Option<Money>,
    ) -> (i64, i64) {
        let ingredient_id = ingredient(conn, name, category, unit);
        let lot = create_lot(
            conn,
            &NewLot {
                ingredient_id,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(size, unit).unwrap(),
                cost,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        (ingredient_id, lot)
    }

    fn eur(minor: i64) -> Money {
        Money::new(minor, Currency::EUR)
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

        let err = transition_batch(&conn, batch, BatchStatus::Fermenting, T0)
            .expect_err("pitch is t=0 for everything downstream");
        assert_eq!(err.code(), "db.batch_not_pitched");
        assert_eq!(
            get_batch(&conn, batch).unwrap().status,
            BatchStatus::Planned
        );

        set_pitched_at(&conn, batch, T0).unwrap();
        assert!(transition_batch(&conn, batch, BatchStatus::Fermenting, T0).unwrap());
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

        let err =
            transition_batch(&conn, batch, BatchStatus::Packaged, T0).expect_err("no skipping");
        assert_eq!(err.code(), "db.illegal_batch_transition");

        // Already there is not an error, it is nothing to do.
        assert!(!transition_batch(&conn, batch, BatchStatus::Planned, T0).unwrap());

        transition_batch(&conn, batch, BatchStatus::Fermenting, T0).unwrap();
        transition_batch(&conn, batch, BatchStatus::Conditioning, T0).unwrap();
        let err = transition_batch(&conn, batch, BatchStatus::Fermenting, T0)
            .expect_err("a batch does not un-condition");
        assert_eq!(err.code(), "db.illegal_batch_transition");
        assert!(
            err.to_string().contains("conditioning"),
            "unexpected: {err}"
        );

        transition_batch(&conn, batch, BatchStatus::Packaged, T0).unwrap();
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
            transition_batch(&conn, batch, BatchStatus::Fermenting, T0).unwrap_err(),
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
    fn a_duplicate_batch_code_is_refused_by_name() {
        let conn = db();
        create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        let err = create_batch(&conn, &new_batch(" SELTZER-001 ", None), T0)
            .expect_err("codes are unique");
        assert_eq!(err.code(), "db.duplicate");
        assert!(err.to_string().contains("SELTZER-001"), "unexpected: {err}");
    }

    #[test]
    fn a_new_batch_is_checked_before_its_version_is_locked() {
        let conn = db();
        let version = seltzer_recipe(&conn);
        let refused = [
            (new_batch("  ", Some(version)), "db.name_empty"),
            (
                NewBatch {
                    og: Some(Sg::new(1040.0).unwrap()),
                    ..new_batch("SELTZER-001", Some(version))
                },
                "db.measurement_implausible",
            ),
            (
                NewBatch {
                    volume_l: Some(Litres::new(0.0).unwrap()),
                    ..new_batch("SELTZER-001", Some(version))
                },
                "units.not_positive",
            ),
        ];
        for (batch, code) in refused {
            assert_eq!(create_batch(&conn, &batch, T0).unwrap_err().code(), code);
        }
        assert!(!get_recipe_version(&conn, version).unwrap().is_locked());
    }

    #[test]
    fn an_edit_replaces_the_fields_and_keeps_the_code_unique() {
        let conn = db();
        create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        let batch = create_batch(
            &conn,
            &NewBatch {
                volume_l: Some(Litres::new(20.0).unwrap()),
                notes: Some("first try".into()),
                ..new_batch("SELTZER-002", None)
            },
            T0,
        )
        .unwrap();
        let edit = |code: &str| BatchEdit {
            code: code.into(),
            vessel_id: None,
            volume_l: None,
            notes: None,
        };

        let err = update_batch(&conn, batch, &edit("SELTZER-001")).unwrap_err();
        assert_eq!(err.code(), "db.duplicate");

        update_batch(&conn, batch, &edit(" SELTZER-003 ")).unwrap();
        let row = get_batch(&conn, batch).unwrap();
        assert_eq!(row.code, "SELTZER-003");
        assert_eq!((row.volume_l, row.notes), (None, None));
    }

    #[test]
    fn a_pitched_batch_keeps_its_vessel() {
        let conn = db();
        let vessel = |name| {
            crate::db::reference::create_vessel(
                &conn,
                name,
                crate::db::reference::VesselKind::Fermenter,
                None,
                None,
            )
            .unwrap()
        };
        let (fv1, fv2) = (vessel("FV1"), vessel("FV2"));
        let batch = create_batch(
            &conn,
            &NewBatch {
                vessel_id: Some(fv1),
                ..new_batch("SELTZER-001", None)
            },
            T0,
        )
        .unwrap();
        let edit = |vessel_id| BatchEdit {
            code: "SELTZER-001".into(),
            vessel_id,
            volume_l: None,
            notes: None,
        };

        update_batch(&conn, batch, &edit(Some(fv2))).unwrap();
        crate::db::timeline::record_pitch(&conn, batch, T0, None).unwrap();
        let err = update_batch(&conn, batch, &edit(Some(fv1))).unwrap_err();
        assert_eq!(err.code(), "db.batch_vessel_locked");
        // The rest of the batch is still editable.
        update_batch(&conn, batch, &edit(Some(fv2))).unwrap();
    }

    #[test]
    fn an_archived_batch_comes_back() {
        let conn = db();
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        archive_batch(&conn, batch, T0).unwrap();

        assert!(restore_batch(&conn, batch).unwrap());
        assert!(!restore_batch(&conn, batch).unwrap());
        assert_eq!(get_batch(&conn, batch).unwrap().archived_at, None);
        assert_eq!(
            archive_batch(&conn, 404, T0).unwrap_err().code(),
            "db.no_such_batch"
        );
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

    /// The worked case the `f64` unit price exists for: €12.50 for a 5 kg sack of
    /// sucrose is 0.25 minor units per gram, and 4.2 kg of it is €10.50. An integer
    /// per-unit price would make this €0.00.
    #[test]
    fn a_batch_built_from_a_cheap_bulk_lot_costs_more_than_nothing() {
        let conn = db();
        let (_, sugar) = priced(
            &conn,
            "Sucrose",
            Category::Sugar,
            Unit::Kg,
            5.0,
            Some(eur(1250)),
        );
        let (_, yeast) = priced(
            &conn,
            "EC-1118",
            Category::Yeast,
            Unit::G,
            500.0,
            Some(eur(2400)),
        );
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(sugar, 4.2, Unit::Kg, Stage::Prep)).unwrap();
        add_batch_ingredient(&conn, batch, &actual(yeast, 25.0, Unit::G, Stage::Pitch)).unwrap();

        let cost = batch_cost(&conn, batch).unwrap();
        // 4200 g x 0.25 = 1050, plus 25 g x 4.8 = 120.
        assert_eq!(cost.total, Some(eur(1170)));
        assert_eq!(cost.lines_costed, 2);
        assert_eq!(cost.lines_without_cost, 0);
    }

    /// The volume falls back to the recipe version's target, because a batch that has
    /// not been measured yet still has a planned cost per litre.
    #[test]
    fn cost_per_litre_falls_back_to_the_recipe_versions_target_volume() {
        let conn = db();
        let version = seltzer_recipe(&conn); // target_volume_l = 20 L
        let (_, sugar) = priced(
            &conn,
            "Sucrose",
            Category::Sugar,
            Unit::Kg,
            5.0,
            Some(eur(1250)),
        );
        let batch = create_batch(&conn, &new_batch("SELTZER-002", Some(version)), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(sugar, 4.0, Unit::Kg, Stage::Prep)).unwrap();

        let cost = batch_cost(&conn, batch).unwrap();
        assert_eq!(cost.volume_l, Some(Litres::new(20.0).unwrap()));
        assert_eq!(cost.total, Some(eur(1000)));
        assert_eq!(cost.per_litre, Some(eur(50)));

        // A measured volume wins over the plan.
        set_batch_measurements(&conn, batch, Some(Litres::new(16.0).unwrap()), None, None).unwrap();
        let cost = batch_cost(&conn, batch).unwrap();
        assert_eq!(cost.volume_l, Some(Litres::new(16.0).unwrap()));
        assert_eq!(cost.per_litre, Some(eur(63))); // 62.5, rounded
    }

    /// A lot with no price is counted, not treated as free. A total that silently
    /// omitted it would look like a total.
    #[test]
    fn an_unpriced_lot_is_reported_rather_than_costed_at_zero() {
        let conn = db();
        let (_, sugar) = priced(
            &conn,
            "Sucrose",
            Category::Sugar,
            Unit::Kg,
            5.0,
            Some(eur(1250)),
        );
        let (_, nutrient) = stocked(&conn, "Fermaid O", Category::Nutrient, Unit::G);
        let batch = create_batch(&conn, &new_batch("SELTZER-003", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(sugar, 4.0, Unit::Kg, Stage::Prep)).unwrap();
        add_batch_ingredient(
            &conn,
            batch,
            &actual(nutrient, 10.0, Unit::G, Stage::Fermentation),
        )
        .unwrap();

        let cost = batch_cost(&conn, batch).unwrap();
        assert_eq!(cost.total, Some(eur(1000)));
        assert_eq!(cost.lines_costed, 1);
        assert_eq!(cost.lines_without_cost, 1);
    }

    /// No priced lot at all is `None`, and `None` is not zero: an unpriced batch did
    /// not cost nothing.
    #[test]
    fn a_batch_with_no_priced_lots_has_no_total_rather_than_a_zero_one() {
        let conn = db();
        let (_, sugar) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-004", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(sugar, 4.0, Unit::Kg, Stage::Prep)).unwrap();

        let cost = batch_cost(&conn, batch).unwrap();
        assert!(cost.total.is_none());
        assert!(cost.per_litre.is_none());
        assert_eq!(cost.lines_without_cost, 1);
    }

    /// `units::money`'s policy, reaching the one place in the app that could have
    /// been tempted to break it.
    #[test]
    fn a_batch_spanning_two_currencies_fails_loudly() {
        let conn = db();
        let (_, sugar) = priced(
            &conn,
            "Sucrose",
            Category::Sugar,
            Unit::Kg,
            5.0,
            Some(eur(1250)),
        );
        let (_, yeast) = priced(
            &conn,
            "EC-1118",
            Category::Yeast,
            Unit::G,
            500.0,
            Some(Money::new(2400, Currency::USD)),
        );
        let batch = create_batch(&conn, &new_batch("SELTZER-005", None), T0).unwrap();
        add_batch_ingredient(&conn, batch, &actual(sugar, 4.0, Unit::Kg, Stage::Prep)).unwrap();
        add_batch_ingredient(&conn, batch, &actual(yeast, 25.0, Unit::G, Stage::Pitch)).unwrap();

        let err = batch_cost(&conn, batch).unwrap_err();
        assert_eq!(err.code(), "money.currency_mismatch");
    }

    #[test]
    fn a_batch_with_no_additions_costs_nothing_known() {
        let conn = db();
        let batch = create_batch(&conn, &new_batch("SELTZER-006", None), T0).unwrap();
        let cost = batch_cost(&conn, batch).unwrap();
        assert!(cost.total.is_none());
        assert_eq!(cost.lines_costed, 0);
        assert_eq!(cost.lines_without_cost, 0);
    }

    // ------------------------------------------------------------------- U5

    #[test]
    fn an_actual_of_nothing_or_less_is_refused() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        for qty in [0.0, -2.0] {
            let err = add_batch_ingredient(&conn, batch, &actual(lot, qty, Unit::Kg, Stage::Prep))
                .unwrap_err();
            assert_eq!(err.code(), "units.not_positive");
        }
    }

    #[test]
    fn an_archived_lot_takes_no_new_additions_but_an_existing_one_is_still_fixable() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let (_, other) = stocked(&conn, "Dextrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        let id =
            add_batch_ingredient(&conn, batch, &actual(lot, 4.0, Unit::Kg, Stage::Prep)).unwrap();
        crate::db::reference::archive_lot(&conn, lot, T0).unwrap();

        let err = add_batch_ingredient(&conn, batch, &actual(lot, 1.0, Unit::Kg, Stage::Prep))
            .unwrap_err();
        assert_eq!(err.code(), "db.lot_archived");
        update_batch_ingredient(&conn, id, &actual(lot, 4.2, Unit::Kg, Stage::Prep)).unwrap();
        assert_eq!(
            list_batch_ingredients(&conn, batch).unwrap()[0].qty,
            CanonicalQty::Mass(Grams::new(4200.0).unwrap())
        );

        // Moving it to another lot and back is a new pick of the archived one.
        update_batch_ingredient(&conn, id, &actual(other, 4.2, Unit::Kg, Stage::Prep)).unwrap();
        let err = update_batch_ingredient(&conn, id, &actual(lot, 4.2, Unit::Kg, Stage::Prep))
            .unwrap_err();
        assert_eq!(err.code(), "db.lot_archived");
    }

    #[test]
    fn an_edit_is_checked_like_an_add_and_an_archived_batch_keeps_its_actuals() {
        let conn = db();
        let (_, lot) = stocked(&conn, "Sucrose", Category::Sugar, Unit::Kg);
        let batch = create_batch(&conn, &new_batch("SELTZER-001", None), T0).unwrap();
        let id =
            add_batch_ingredient(&conn, batch, &actual(lot, 4.0, Unit::Kg, Stage::Prep)).unwrap();

        let err = update_batch_ingredient(&conn, id, &actual(lot, 4.0, Unit::L, Stage::Prep))
            .unwrap_err();
        assert_eq!(err.code(), "units.dimension_mismatch");
        let err = update_batch_ingredient(&conn, 404, &actual(lot, 1.0, Unit::Kg, Stage::Prep))
            .unwrap_err();
        assert_eq!(err.code(), "db.no_such_batch_ingredient");

        archive_batch(&conn, batch, T0).unwrap();
        for err in [
            update_batch_ingredient(&conn, id, &actual(lot, 5.0, Unit::Kg, Stage::Prep))
                .unwrap_err(),
            delete_batch_ingredient(&conn, id).unwrap_err(),
        ] {
            assert_eq!(err.code(), "db.batch_archived");
        }
        restore_batch(&conn, batch).unwrap();
        assert!(delete_batch_ingredient(&conn, id).unwrap());
    }
}
