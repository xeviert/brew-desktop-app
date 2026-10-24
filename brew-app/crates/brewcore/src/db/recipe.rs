//! Phase 3b. Recipes, versioned properly.
//!
//! The rule this module exists to enforce: **a version referenced by a batch is
//! immutable.** Without it, "every batch of Citron-Verveine v3" is a lie — v3 would
//! mean whatever the recipe happened to say when it was last opened, and the twenty
//! batches being compared would be comparing nothing. Editing a locked version forks
//! `version_no + 1`, which is also what removes the need to snapshot a recipe onto a
//! batch: a frozen version *is* the snapshot, and it stays deduplicated and diffable.
//!
//! Same contract as [`tier1`](crate::db::tier1) and [`reference`](crate::db::reference):
//! every function takes a borrowed connection and never opens or commits its own
//! transaction. `&Transaction` derefs to `&Connection`, so "create a version and its
//! items" composes into one boundary at the call site.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    enums::db_enum,
    reference::{
        CanonicalQty, archive_row, canonical_from_columns, duplicate, positive_canonical,
        required_name, restore_row,
    },
};
use crate::{
    Error, Result,
    units::{Litres, Quantity, Sg, Timestamp},
};

db_enum!(
    /// What is being made. On `recipe`, and on `batch` in 3c, where it decides which
    /// optional side tables apply — so `batch` never grows beer-only nullable columns.
    ///
    /// `Beer` is a *value*, not a table or a column: the Scope rule in
    /// docs/BUILDOUT.md and the `schema_conventions` linter both bind names only.
    BeverageType,
    UnknownBeverageType,
    (Seltzer, "seltzer"),
    (Beer, "beer"),
    (Cider, "cider"),
    (Wine, "wine"),
    (Mead, "mead"),
    (Kombucha, "kombucha"),
    (Other, "other"),
);

db_enum!(
    /// When in the process an item goes in. Not a `CHECK` in the schema — see
    /// `0003_recipe.sql`.
    Stage,
    UnknownStage,
    (Prep, "prep"),
    (Pitch, "pitch"),
    (Fermentation, "fermentation"),
    (Conditioning, "conditioning"),
    (Packaging, "packaging"),
);

// ---------------------------------------------------------------- recipe

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RecipeRow {
    pub id: i64,
    pub name: String,
    pub beverage_type: BeverageType,
    pub notes: Option<String>,
    pub archived_at: Option<Timestamp>,
}

/// The name is trimmed and required, and unique among every recipe, archived ones
/// included — an archived recipe's versions are still what old batches point at.
pub fn create_recipe(
    conn: &Connection,
    name: &str,
    beverage_type: BeverageType,
    notes: Option<&str>,
) -> Result<i64> {
    let name = required_name("recipe", name)?;
    conn.execute(
        "INSERT INTO recipe (name, beverage_type, notes) VALUES (?, ?, ?)",
        params![name, beverage_type, notes],
    )
    .map_err(duplicate("recipe", name))?;
    Ok(conn.last_insert_rowid())
}

/// Renaming is always allowed. The type is not, once a version is locked: the batch
/// that locked it was checked against the type it had then.
pub fn update_recipe(
    conn: &Connection,
    id: i64,
    name: &str,
    beverage_type: BeverageType,
    notes: Option<&str>,
) -> Result<()> {
    let name = required_name("recipe", name)?;
    let current = get_recipe(conn, id)?;
    if current.beverage_type != beverage_type {
        let locked: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM recipe_version \
             WHERE recipe_id = ? AND locked_at IS NOT NULL)",
            [id],
            |row| row.get(0),
        )?;
        if locked {
            return Err(Error::RecipeTypeLocked {
                recipe_id: id,
                beverage_type: current.beverage_type,
            });
        }
    }
    conn.execute(
        "UPDATE recipe SET name = ?, beverage_type = ?, notes = ? WHERE id = ?",
        params![name, beverage_type, notes, id],
    )
    .map_err(duplicate("recipe", name))?;
    Ok(())
}

fn recipe_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecipeRow> {
    Ok(RecipeRow {
        id: row.get(0)?,
        name: row.get(1)?,
        beverage_type: row.get(2)?,
        notes: row.get(3)?,
        archived_at: row.get(4)?,
    })
}

pub fn get_recipe(conn: &Connection, id: i64) -> Result<RecipeRow> {
    conn.query_row(
        "SELECT id, name, beverage_type, notes, archived_at FROM recipe WHERE id = ?",
        [id],
        recipe_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchRecipe { recipe_id: id })
}

pub fn list_recipes(conn: &Connection, include_archived: bool) -> Result<Vec<RecipeRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, beverage_type, notes, archived_at FROM recipe \
         WHERE ?1 OR archived_at IS NULL ORDER BY beverage_type, name",
    )?;
    let rows = stmt.query_map([include_archived], recipe_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Returns whether a row changed. A recipe is never hard-deleted: its versions are
/// what old batches point at.
pub fn archive_recipe(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    archive_row(conn, "recipe", id, at)
}

pub fn restore_recipe(conn: &Connection, id: i64) -> Result<bool> {
    restore_row(conn, "recipe", id)
}

// -------------------------------------------------------- recipe_version

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewRecipeVersion {
    pub target_volume_l: Option<Litres>,
    pub target_og: Option<Sg>,
    pub target_fg: Option<Sg>,
    /// A fraction, like `yeast_strain.attenuation_*` — `0.055`, not `5.5`.
    pub target_abv: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RecipeVersionRow {
    pub id: i64,
    pub recipe_id: i64,
    pub version_no: i64,
    pub target_volume_l: Option<Litres>,
    pub target_og: Option<Sg>,
    pub target_fg: Option<Sg>,
    pub target_abv: Option<f64>,
    /// Non-NULL means frozen: a batch references it, so edits fork instead.
    pub locked_at: Option<Timestamp>,
    pub notes: Option<String>,
}

impl RecipeVersionRow {
    pub fn is_locked(&self) -> bool {
        self.locked_at.is_some()
    }
}

const VERSION_COLUMNS: &str = "id, recipe_id, version_no, target_volume_l, target_og, \
                               target_fg, target_abv, locked_at, notes";

fn version_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecipeVersionRow> {
    Ok(RecipeVersionRow {
        id: row.get(0)?,
        recipe_id: row.get(1)?,
        version_no: row.get(2)?,
        target_volume_l: row.get(3)?,
        target_og: row.get(4)?,
        target_fg: row.get(5)?,
        target_abv: row.get(6)?,
        locked_at: row.get(7)?,
        notes: row.get(8)?,
    })
}

/// The next version number for a recipe. `UNIQUE (recipe_id, version_no)` is the
/// backstop for two callers racing, not the mechanism — which is why this wants to
/// run inside the caller's transaction.
fn next_version_no(conn: &Connection, recipe_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version_no), 0) + 1 FROM recipe_version WHERE recipe_id = ?",
        [recipe_id],
        |row| row.get(0),
    )?)
}

/// `Sg` and `Litres` deserialise with only a finiteness check, so a target typed as
/// `1040` or `-20` would otherwise reach the finish detector's expected FG and every
/// cost per litre. Same plausibility rule as `set_batch_og`.
fn validate_targets(version: &NewRecipeVersion) -> Result<()> {
    for sg in [version.target_og, version.target_fg].into_iter().flatten() {
        if !sg.is_plausible() {
            return Err(Error::MeasurementImplausible {
                metric: crate::db::Metric::Sg,
                value: sg.get(),
            });
        }
    }
    if let (Some(og), Some(fg)) = (version.target_og, version.target_fg) {
        if fg.get() > og.get() {
            return Err(Error::RangeInverted { field: "gravity" });
        }
    }
    if let Some(abv) = version.target_abv {
        if !(0.0..=1.0).contains(&abv) {
            return Err(Error::NotAFraction {
                field: "target_abv",
                value: abv,
            });
        }
    }
    if let Some(volume) = version.target_volume_l.filter(|v| v.get() <= 0.0) {
        return Err(Error::NotPositive {
            field: "target_volume_l",
            value: volume.get(),
        });
    }
    Ok(())
}

pub fn create_recipe_version(
    conn: &Connection,
    recipe_id: i64,
    version: &NewRecipeVersion,
) -> Result<i64> {
    validate_targets(version)?;
    let version_no = next_version_no(conn, recipe_id)?;
    conn.execute(
        "INSERT INTO recipe_version \
         (recipe_id, version_no, target_volume_l, target_og, target_fg, target_abv, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            recipe_id,
            version_no,
            version.target_volume_l,
            version.target_og,
            version.target_fg,
            version.target_abv,
            version.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_recipe_versions(conn: &Connection, recipe_id: i64) -> Result<Vec<RecipeVersionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERSION_COLUMNS} FROM recipe_version WHERE recipe_id = ? ORDER BY version_no"
    ))?;
    let rows = stmt.query_map([recipe_id], version_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_recipe_version(conn: &Connection, id: i64) -> Result<RecipeVersionRow> {
    conn.query_row(
        &format!("SELECT {VERSION_COLUMNS} FROM recipe_version WHERE id = ?"),
        [id],
        version_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchRecipeVersion {
        recipe_version_id: id,
    })
}

/// Every mutation goes through here first.
///
/// A missing row and a locked row are different errors: the first is a bug in the
/// caller, the second is the rule working, and the UI branches on
/// `db.recipe_version_locked` to offer the fork.
fn ensure_unlocked(conn: &Connection, recipe_version_id: i64) -> Result<()> {
    let locked_at: Option<Timestamp> = conn
        .query_row(
            "SELECT locked_at FROM recipe_version WHERE id = ?",
            [recipe_version_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NoSuchRecipeVersion { recipe_version_id })?;

    match locked_at {
        None => Ok(()),
        Some(_) => Err(Error::RecipeVersionLocked { recipe_version_id }),
    }
}

/// The version an item belongs to. An item of a locked version is as frozen as the
/// version row itself, so every item mutation resolves this first.
fn version_of_item(conn: &Connection, item_id: i64) -> Result<i64> {
    conn.query_row(
        "SELECT recipe_version_id FROM recipe_item WHERE id = ?",
        [item_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(Error::NoSuchRecipeItem {
        recipe_item_id: item_id,
    })
}

pub fn update_recipe_version(conn: &Connection, id: i64, version: &NewRecipeVersion) -> Result<()> {
    ensure_unlocked(conn, id)?;
    validate_targets(version)?;
    conn.execute(
        "UPDATE recipe_version SET target_volume_l = ?, target_og = ?, target_fg = ?, \
                target_abv = ?, notes = ? \
         WHERE id = ?",
        params![
            version.target_volume_l,
            version.target_og,
            version.target_fg,
            version.target_abv,
            version.notes,
            id,
        ],
    )?;
    Ok(())
}

/// Freeze a version. Called by 3c's `create_batch`, inside its transaction.
///
/// Idempotent: relocking an already-locked version returns `false`, not an error —
/// the second batch off a recipe version is the normal case, not a mistake.
pub fn lock_recipe_version(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE recipe_version SET locked_at = ? WHERE id = ? AND locked_at IS NULL",
        params![at, id],
    )?;
    Ok(changed == 1)
}

/// Copy a version and all of its items to the next `version_no`, unlocked.
///
/// Forking a *locked* version is the whole point of the lock, so there is
/// deliberately no `ensure_unlocked` here.
pub fn fork_recipe_version(conn: &Connection, from_id: i64) -> Result<i64> {
    let from = get_recipe_version(conn, from_id)?;
    let version_no = next_version_no(conn, from.recipe_id)?;

    conn.execute(
        "INSERT INTO recipe_version \
         (recipe_id, version_no, target_volume_l, target_og, target_fg, target_abv, notes) \
         SELECT recipe_id, ?, target_volume_l, target_og, target_fg, target_abv, notes \
         FROM recipe_version WHERE id = ?",
        params![version_no, from_id],
    )?;
    let new_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO recipe_item \
         (recipe_version_id, ingredient_id, qty_g, qty_l, qty_each, entry_qty, entry_unit, \
          stage, offset_hours, notes) \
         SELECT ?, ingredient_id, qty_g, qty_l, qty_each, entry_qty, entry_unit, \
                stage, offset_hours, notes \
         FROM recipe_item WHERE recipe_version_id = ? ORDER BY id",
        params![new_id, from_id],
    )?;

    Ok(new_id)
}

/// Delete an unlocked draft and its items — the way out of a fork made by mistake.
///
/// Only the newest version, so numbers stay contiguous and a deleted v3 never leaves
/// the next fork to reuse "v3" for something else. A locked version is never a
/// draft: a batch points at it.
pub fn delete_recipe_version(conn: &Connection, id: i64) -> Result<()> {
    ensure_unlocked(conn, id)?;
    let version = get_recipe_version(conn, id)?;
    let newest = next_version_no(conn, version.recipe_id)? - 1;
    if version.version_no != newest {
        return Err(Error::RecipeVersionNotNewest {
            version_no: version.version_no,
        });
    }
    // Items go by the schema's ON DELETE CASCADE.
    conn.execute("DELETE FROM recipe_version WHERE id = ?", [id])?;
    Ok(())
}

/// Batches per version of one recipe, archived ones included — the lock badge's
/// "used by N batches". A version no batch used is absent, not zero.
pub fn version_batch_counts(conn: &Connection, recipe_id: i64) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare(
        "SELECT b.recipe_version_id, COUNT(*) FROM batch b \
         JOIN recipe_version v ON v.id = b.recipe_version_id \
         WHERE v.recipe_id = ? GROUP BY b.recipe_version_id",
    )?;
    let rows = stmt.query_map([recipe_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ----------------------------------------------------------- recipe_item

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewRecipeItem {
    pub ingredient_id: i64,
    /// As typed: `2.5 kg`. The canonical column is derived from the ingredient's
    /// `default_unit`, never chosen by the caller.
    pub entry: Quantity,
    pub stage: Stage,
    /// Hours from pitch. For a sugar wash the nutrient schedule is the process
    /// variable that decides the batch, so it is a column and not a free-text note.
    pub offset_hours: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RecipeItemRow {
    pub id: i64,
    pub recipe_version_id: i64,
    pub ingredient_id: i64,
    pub qty: CanonicalQty,
    pub entry: Quantity,
    pub stage: Stage,
    pub offset_hours: Option<f64>,
    pub notes: Option<String>,
}

/// Negative is allowed — a prep step hours before pitch — but not a NaN, which
/// `ORDER BY offset_hours` would sort somewhere nobody expects.
fn check_offset(item: &NewRecipeItem) -> Result<()> {
    match item.offset_hours {
        Some(value) if !value.is_finite() => Err(Error::NotFinite {
            field: "offset_hours",
            value,
        }),
        _ => Ok(()),
    }
}

pub fn add_recipe_item(
    conn: &Connection,
    recipe_version_id: i64,
    item: &NewRecipeItem,
) -> Result<i64> {
    ensure_unlocked(conn, recipe_version_id)?;
    check_offset(item)?;
    let (qty_g, qty_l, qty_each) = positive_canonical(conn, item.ingredient_id, item.entry)?;

    conn.execute(
        "INSERT INTO recipe_item \
         (recipe_version_id, ingredient_id, qty_g, qty_l, qty_each, entry_qty, entry_unit, \
          stage, offset_hours, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            recipe_version_id,
            item.ingredient_id,
            qty_g,
            qty_l,
            qty_each,
            item.entry.value,
            item.entry.unit,
            item.stage,
            item.offset_hours,
            item.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_recipe_item(conn: &Connection, id: i64, item: &NewRecipeItem) -> Result<()> {
    ensure_unlocked(conn, version_of_item(conn, id)?)?;
    check_offset(item)?;
    let (qty_g, qty_l, qty_each) = positive_canonical(conn, item.ingredient_id, item.entry)?;

    conn.execute(
        "UPDATE recipe_item SET ingredient_id = ?, qty_g = ?, qty_l = ?, qty_each = ?, \
                entry_qty = ?, entry_unit = ?, stage = ?, offset_hours = ?, notes = ? \
         WHERE id = ?",
        params![
            item.ingredient_id,
            qty_g,
            qty_l,
            qty_each,
            item.entry.value,
            item.entry.unit,
            item.stage,
            item.offset_hours,
            item.notes,
            id,
        ],
    )?;
    Ok(())
}

pub fn delete_recipe_item(conn: &Connection, id: i64) -> Result<bool> {
    ensure_unlocked(conn, version_of_item(conn, id)?)?;
    let changed = conn.execute("DELETE FROM recipe_item WHERE id = ?", [id])?;
    Ok(changed == 1)
}

/// Ordered the way the process runs: by how long after pitch, within the stage.
///
/// `offset_hours IS NULL` sorts first — an item with no schedule is part of the
/// stage itself, not something timed inside it.
pub fn list_recipe_items(conn: &Connection, recipe_version_id: i64) -> Result<Vec<RecipeItemRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, recipe_version_id, ingredient_id, qty_g, qty_l, qty_each, \
                entry_qty, entry_unit, stage, offset_hours, notes \
         FROM recipe_item WHERE recipe_version_id = ? \
         ORDER BY offset_hours IS NULL DESC, offset_hours, id",
    )?;
    let rows = stmt.query_map([recipe_version_id], |row| {
        Ok(RecipeItemRow {
            id: row.get(0)?,
            recipe_version_id: row.get(1)?,
            ingredient_id: row.get(2)?,
            qty: canonical_from_columns(3, row.get(3)?, row.get(4)?, row.get(5)?)?,
            entry: Quantity {
                value: row.get(6)?,
                unit: row.get(7)?,
            },
            stage: row.get(8)?,
            offset_hours: row.get(9)?,
            notes: row.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ------------------------------------------------------------------ diff

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase", tag = "change")]
pub enum ItemChange {
    Added(RecipeItemRow),
    Removed(RecipeItemRow),
    Changed {
        from: RecipeItemRow,
        to: RecipeItemRow,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VersionDiff {
    pub from: RecipeVersionRow,
    pub to: RecipeVersionRow,
    /// `Some((before, after))` only where the two versions disagree.
    pub target_volume_l: Option<(Option<Litres>, Option<Litres>)>,
    pub target_og: Option<(Option<Sg>, Option<Sg>)>,
    pub target_fg: Option<(Option<Sg>, Option<Sg>)>,
    pub target_abv: Option<(Option<f64>, Option<f64>)>,
    pub items: Vec<ItemChange>,
}

/// Pair items up and report what moved.
///
/// The pairing key is `(ingredient_id, stage)` plus an occurrence index for
/// duplicates, and deliberately **not** `offset_hours`: a nutrient addition moved
/// from 24 h to 36 h is the single most interesting change in a sugar-wash diff, and
/// keying on the offset would report it as a removal plus an addition — the two
/// halves of one fact, listed apart.
pub fn diff_items(from: &[RecipeItemRow], to: &[RecipeItemRow]) -> Vec<ItemChange> {
    type Key = (i64, Stage, usize);

    fn keyed(items: &[RecipeItemRow]) -> Vec<(Key, &RecipeItemRow)> {
        let mut seen: Vec<Key> = Vec::new();
        items
            .iter()
            .map(|item| {
                let occurrence = seen
                    .iter()
                    .filter(|(id, stage, _)| *id == item.ingredient_id && *stage == item.stage)
                    .count();
                let key = (item.ingredient_id, item.stage, occurrence);
                seen.push(key);
                (key, item)
            })
            .collect()
    }

    let (from_keyed, to_keyed) = (keyed(from), keyed(to));
    let mut changes = Vec::new();

    for (key, old) in &from_keyed {
        match to_keyed.iter().find(|(k, _)| k == key) {
            None => changes.push(ItemChange::Removed((*old).clone())),
            Some((_, new)) => {
                if old.qty != new.qty
                    || old.offset_hours != new.offset_hours
                    || old.notes != new.notes
                {
                    changes.push(ItemChange::Changed {
                        from: (*old).clone(),
                        to: (*new).clone(),
                    });
                }
            }
        }
    }
    for (key, new) in &to_keyed {
        if !from_keyed.iter().any(|(k, _)| k == key) {
            changes.push(ItemChange::Added((*new).clone()));
        }
    }
    changes
}

/// Load both versions and diff them. The comparison itself is [`diff_items`], which
/// needs no database.
pub fn diff_recipe_versions(conn: &Connection, from_id: i64, to_id: i64) -> Result<VersionDiff> {
    let from = get_recipe_version(conn, from_id)?;
    let to = get_recipe_version(conn, to_id)?;
    let items = diff_items(
        &list_recipe_items(conn, from_id)?,
        &list_recipe_items(conn, to_id)?,
    );

    fn delta<T: PartialEq>(a: Option<T>, b: Option<T>) -> Option<(Option<T>, Option<T>)> {
        (a != b).then_some((a, b))
    }

    Ok(VersionDiff {
        target_volume_l: delta(from.target_volume_l, to.target_volume_l),
        target_og: delta(from.target_og, to.target_og),
        target_fg: delta(from.target_fg, to.target_fg),
        target_abv: delta(from.target_abv, to.target_abv),
        items,
        from,
        to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::reference::{Category, NewIngredient, create_ingredient},
        units::{Grams, Unit},
    };

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

    fn sucrose(conn: &Connection) -> i64 {
        ingredient(conn, "Sucrose", Category::Sugar, Unit::Kg)
    }

    fn fermaid(conn: &Connection) -> i64 {
        ingredient(conn, "Fermaid O", Category::Nutrient, Unit::G)
    }

    /// A recipe with one empty version. The shape every test below starts from.
    fn seltzer(conn: &Connection) -> (i64, i64) {
        let recipe = create_recipe(conn, "Citron-Verveine", BeverageType::Seltzer, None).unwrap();
        let version = create_recipe_version(
            conn,
            recipe,
            &NewRecipeVersion {
                target_volume_l: Some(Litres::new(20.0).unwrap()),
                target_og: Some(Sg::new(1.040).unwrap()),
                target_fg: Some(Sg::new(1.000).unwrap()),
                ..Default::default()
            },
        )
        .unwrap();
        (recipe, version)
    }

    fn sugar_item(ingredient_id: i64, kg: f64) -> NewRecipeItem {
        NewRecipeItem {
            ingredient_id,
            entry: Quantity::new(kg, Unit::Kg).unwrap(),
            stage: Stage::Prep,
            offset_hours: None,
            notes: None,
        }
    }

    fn nutrient_at(ingredient_id: i64, grams: f64, hours: f64) -> NewRecipeItem {
        NewRecipeItem {
            ingredient_id,
            entry: Quantity::new(grams, Unit::G).unwrap(),
            stage: Stage::Fermentation,
            offset_hours: Some(hours),
            notes: None,
        }
    }

    /// The rule the whole module exists for.
    #[test]
    fn a_locked_version_refuses_an_edit_and_names_the_fork_as_the_way_out() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);

        assert!(lock_recipe_version(&conn, version, Timestamp::from_secs(1_700_000_000)).unwrap());

        let err = update_recipe_version(&conn, version, &NewRecipeVersion::default())
            .expect_err("a locked version is frozen");
        assert_eq!(err.code(), "db.recipe_version_locked");
        assert!(err.to_string().contains("fork"), "unexpected: {err}");

        let err = add_recipe_item(&conn, version, &sugar_item(sugar, 2.5))
            .expect_err("a locked version takes no new items");
        assert_eq!(err.code(), "db.recipe_version_locked");

        // The targets are untouched: nothing was half-applied.
        let row = get_recipe_version(&conn, version).unwrap();
        assert!(row.is_locked());
        assert_eq!(row.target_og, Some(Sg::new(1.040).unwrap()));
    }

    /// Item mutations resolve their version first, or the freeze would cover the
    /// version row only and leave every quantity in it editable.
    #[test]
    fn an_item_of_a_locked_version_cannot_be_edited_or_deleted() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let nutrient = fermaid(&conn);
        let item = add_recipe_item(&conn, version, &nutrient_at(nutrient, 5.0, 24.0)).unwrap();

        lock_recipe_version(&conn, version, Timestamp::from_secs(1_700_000_000)).unwrap();

        let err =
            update_recipe_item(&conn, item, &nutrient_at(nutrient, 9.0, 24.0)).expect_err("frozen");
        assert_eq!(err.code(), "db.recipe_version_locked");
        let err = delete_recipe_item(&conn, item).expect_err("frozen");
        assert_eq!(err.code(), "db.recipe_version_locked");

        let items = list_recipe_items(&conn, version).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].qty, CanonicalQty::Mass(Grams::new_unchecked(5.0)));
    }

    /// The second batch off a version is the normal case, not a mistake.
    #[test]
    fn locking_an_already_locked_version_is_false_not_an_error() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let first = Timestamp::from_secs(1_700_000_000);

        assert!(lock_recipe_version(&conn, version, first).unwrap());
        assert!(!lock_recipe_version(&conn, version, Timestamp::from_secs(1_800_000_000)).unwrap());
        // The lock records the *first* batch, not the last.
        assert_eq!(
            get_recipe_version(&conn, version).unwrap().locked_at,
            Some(first)
        );
    }

    #[test]
    fn forking_copies_every_item_and_leaves_the_original_untouched() {
        let conn = db();
        let (recipe, v1) = seltzer(&conn);
        let sugar = sucrose(&conn);
        let nutrient = fermaid(&conn);
        add_recipe_item(&conn, v1, &sugar_item(sugar, 2.5)).unwrap();
        add_recipe_item(&conn, v1, &nutrient_at(nutrient, 5.0, 24.0)).unwrap();
        lock_recipe_version(&conn, v1, Timestamp::from_secs(1_700_000_000)).unwrap();

        // Forking a locked version is the point of the lock, not an error.
        let v2 = fork_recipe_version(&conn, v1).unwrap();
        let forked = get_recipe_version(&conn, v2).unwrap();
        assert_eq!(forked.version_no, 2);
        assert_eq!(forked.recipe_id, recipe);
        assert!(!forked.is_locked());
        assert_eq!(forked.target_og, Some(Sg::new(1.040).unwrap()));

        let (before, after) = (
            list_recipe_items(&conn, v1).unwrap(),
            list_recipe_items(&conn, v2).unwrap(),
        );
        assert_eq!(after.len(), 2);
        assert!(
            diff_items(&before, &after).is_empty(),
            "a fresh fork differs in nothing"
        );

        // Editing the fork leaves the frozen original alone.
        let moved = after.iter().find(|i| i.offset_hours.is_some()).unwrap();
        update_recipe_item(&conn, moved.id, &nutrient_at(nutrient, 5.0, 36.0)).unwrap();
        assert_eq!(
            list_recipe_items(&conn, v1)
                .unwrap()
                .iter()
                .find(|i| i.offset_hours.is_some())
                .unwrap()
                .offset_hours,
            Some(24.0)
        );
    }

    #[test]
    fn version_numbers_are_per_recipe_and_do_not_share_a_counter() {
        let conn = db();
        let (_, first) = seltzer(&conn);
        let other = create_recipe(&conn, "Sugar wash", BeverageType::Other, None).unwrap();
        let others_first =
            create_recipe_version(&conn, other, &NewRecipeVersion::default()).unwrap();

        assert_eq!(get_recipe_version(&conn, first).unwrap().version_no, 1);
        assert_eq!(
            get_recipe_version(&conn, others_first).unwrap().version_no,
            1
        );

        let second = fork_recipe_version(&conn, first).unwrap();
        assert_eq!(get_recipe_version(&conn, second).unwrap().version_no, 2);
        // The other recipe's numbering did not move.
        assert_eq!(list_recipe_versions(&conn, other).unwrap().len(), 1);
    }

    #[test]
    fn a_fork_with_one_changed_quantity_diffs_as_one_change_not_an_add_and_a_remove() {
        let conn = db();
        let (_, v1) = seltzer(&conn);
        let sugar = sucrose(&conn);
        add_recipe_item(&conn, v1, &sugar_item(sugar, 2.5)).unwrap();

        let v2 = fork_recipe_version(&conn, v1).unwrap();
        let forked_item = list_recipe_items(&conn, v2).unwrap()[0].id;
        update_recipe_item(&conn, forked_item, &sugar_item(sugar, 3.0)).unwrap();
        update_recipe_version(
            &conn,
            v2,
            &NewRecipeVersion {
                target_volume_l: Some(Litres::new(20.0).unwrap()),
                target_og: Some(Sg::new(1.048).unwrap()),
                target_fg: Some(Sg::new(1.000).unwrap()),
                ..Default::default()
            },
        )
        .unwrap();

        let diff = diff_recipe_versions(&conn, v1, v2).unwrap();
        assert_eq!(diff.items.len(), 1);
        match &diff.items[0] {
            ItemChange::Changed { from, to } => {
                assert_eq!(from.qty, CanonicalQty::Mass(Grams::new_unchecked(2500.0)));
                assert_eq!(to.qty, CanonicalQty::Mass(Grams::new_unchecked(3000.0)));
            }
            other => panic!("expected one change, got {other:?}"),
        }
        assert_eq!(
            diff.target_og,
            Some((Some(Sg::new(1.040).unwrap()), Some(Sg::new(1.048).unwrap())))
        );
        // An unchanged target is absent, not `Some((x, x))`.
        assert_eq!(diff.target_fg, None);
        assert_eq!(diff.target_volume_l, None);
    }

    /// Keying the diff on `offset_hours` would report this as a removal plus an
    /// addition — the two halves of the one fact a sugar-wash diff is read for.
    #[test]
    fn a_nutrient_addition_moved_from_24h_to_36h_is_a_change_not_a_readd() {
        let conn = db();
        let (_, v1) = seltzer(&conn);
        let nutrient = fermaid(&conn);
        add_recipe_item(&conn, v1, &nutrient_at(nutrient, 5.0, 24.0)).unwrap();

        let v2 = fork_recipe_version(&conn, v1).unwrap();
        let forked = list_recipe_items(&conn, v2).unwrap()[0].id;
        update_recipe_item(&conn, forked, &nutrient_at(nutrient, 5.0, 36.0)).unwrap();

        let diff = diff_recipe_versions(&conn, v1, v2).unwrap();
        assert_eq!(diff.items.len(), 1, "got {:?}", diff.items);
        match &diff.items[0] {
            ItemChange::Changed { from, to } => {
                assert_eq!(from.offset_hours, Some(24.0));
                assert_eq!(to.offset_hours, Some(36.0));
            }
            other => panic!("expected a change, got {other:?}"),
        }
    }

    /// A dropped item and a new one are still reported as such.
    #[test]
    fn a_dropped_item_and_a_new_one_are_a_removal_and_an_addition() {
        let conn = db();
        let (_, v1) = seltzer(&conn);
        let nutrient = fermaid(&conn);
        let acid = ingredient(&conn, "Citric acid", Category::Acid, Unit::G);
        add_recipe_item(&conn, v1, &nutrient_at(nutrient, 5.0, 24.0)).unwrap();

        let v2 = fork_recipe_version(&conn, v1).unwrap();
        let forked = list_recipe_items(&conn, v2).unwrap()[0].id;
        assert!(delete_recipe_item(&conn, forked).unwrap());
        add_recipe_item(
            &conn,
            v2,
            &NewRecipeItem {
                ingredient_id: acid,
                entry: Quantity::new(12.0, Unit::G).unwrap(),
                stage: Stage::Conditioning,
                offset_hours: None,
                notes: None,
            },
        )
        .unwrap();

        let diff = diff_recipe_versions(&conn, v1, v2).unwrap();
        assert_eq!(diff.items.len(), 2);
        assert!(matches!(diff.items[0], ItemChange::Removed(_)));
        assert!(matches!(diff.items[1], ItemChange::Added(_)));
    }

    #[test]
    fn an_item_entered_in_litres_against_a_mass_ingredient_is_refused() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);

        let err = add_recipe_item(
            &conn,
            version,
            &NewRecipeItem {
                ingredient_id: sugar,
                entry: Quantity::new(2.5, Unit::L).unwrap(),
                stage: Stage::Prep,
                offset_hours: None,
                notes: None,
            },
        )
        .expect_err("sucrose is a mass");
        assert_eq!(err.code(), "units.dimension_mismatch");
        assert!(list_recipe_items(&conn, version).unwrap().is_empty());
    }

    #[test]
    fn an_item_entered_in_kg_is_stored_in_grams_and_prints_back_in_kg() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);
        add_recipe_item(&conn, version, &sugar_item(sugar, 2.5)).unwrap();

        let items = list_recipe_items(&conn, version).unwrap();
        let item = &items[0];
        assert_eq!(item.qty, CanonicalQty::Mass(Grams::new_unchecked(2500.0)));
        assert_eq!(item.entry.unit, Unit::Kg);
        assert_eq!(item.entry.to_string(), "2.5 kg");
    }

    /// The schedule reads in the order it is run, with the unscheduled prep first.
    #[test]
    fn a_nutrient_schedule_lists_in_the_order_it_is_run() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);
        let nutrient = fermaid(&conn);

        add_recipe_item(&conn, version, &nutrient_at(nutrient, 3.0, 48.0)).unwrap();
        add_recipe_item(&conn, version, &nutrient_at(nutrient, 5.0, 24.0)).unwrap();
        add_recipe_item(&conn, version, &sugar_item(sugar, 2.5)).unwrap();

        let offsets: Vec<_> = list_recipe_items(&conn, version)
            .unwrap()
            .iter()
            .map(|i| i.offset_hours)
            .collect();
        assert_eq!(offsets, [None, Some(24.0), Some(48.0)]);
    }

    /// Neither open vocabulary has a `CHECK` behind it, so this is the guarantee.
    #[test]
    fn an_unknown_stage_string_in_the_database_is_an_error_not_a_panic() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);
        conn.execute(
            "INSERT INTO recipe_item \
             (recipe_version_id, ingredient_id, qty_g, entry_qty, entry_unit, stage) \
             VALUES (?, ?, 100.0, 100.0, 'g', 'whirlpool')",
            params![version, sugar],
        )
        .unwrap();

        let err = list_recipe_items(&conn, version).expect_err("unknown stage");
        assert_eq!(err.code(), "db.sqlite");
        assert!(err.to_string().contains("whirlpool"), "unexpected: {err}");
    }

    #[test]
    fn a_duplicate_version_number_is_refused_by_the_schema() {
        let conn = db();
        let (recipe, _) = seltzer(&conn);
        let err = conn
            .execute(
                "INSERT INTO recipe_version (recipe_id, version_no) VALUES (?, 1)",
                [recipe],
            )
            .expect_err("version 1 already exists");
        assert!(err.to_string().contains("UNIQUE"), "unexpected: {err}");
    }

    #[test]
    fn an_item_with_two_canonical_quantities_is_refused_by_the_schema() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);
        let err = conn
            .execute(
                "INSERT INTO recipe_item \
                 (recipe_version_id, ingredient_id, qty_g, qty_l, entry_qty, entry_unit, stage) \
                 VALUES (?, ?, 2500.0, 2.5, 2.5, 'kg', 'prep')",
                params![version, sugar],
            )
            .expect_err("exactly one canonical column");
        assert!(err.to_string().contains("CHECK"), "unexpected: {err}");
    }

    #[test]
    fn archiving_a_recipe_hides_it_without_deleting_its_versions() {
        let conn = db();
        let (recipe, version) = seltzer(&conn);
        let at = Timestamp::from_secs(1_700_000_000);

        assert!(archive_recipe(&conn, recipe, at).unwrap());
        assert!(!archive_recipe(&conn, recipe, at).unwrap());

        assert!(list_recipes(&conn, false).unwrap().is_empty());
        assert_eq!(list_recipes(&conn, true).unwrap()[0].archived_at, Some(at));
        // The version an old batch points at is still readable.
        assert_eq!(
            get_recipe_version(&conn, version).unwrap().recipe_id,
            recipe
        );
    }

    #[test]
    fn a_missing_version_is_named_rather_than_read_as_empty() {
        let conn = db();
        let err = get_recipe_version(&conn, 404).expect_err("no such version");
        assert_eq!(err.code(), "db.no_such_recipe_version");
        assert!(err.to_string().contains("404"), "unexpected: {err}");
    }

    /// A batch off `version`, which locks it — the way a version gets locked for real.
    fn brew(conn: &Connection, version: i64, code: &str) -> i64 {
        crate::db::batch::create_batch(
            conn,
            &crate::db::batch::NewBatch {
                code: code.into(),
                recipe_version_id: Some(version),
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: None,
            },
            Timestamp::from_secs(1_700_000_000),
        )
        .unwrap()
    }

    #[test]
    fn a_recipe_name_is_trimmed_required_and_unique() {
        let conn = db();
        let err = create_recipe(&conn, "  ", BeverageType::Seltzer, None).unwrap_err();
        assert_eq!(err.code(), "db.name_empty");

        let id = create_recipe(&conn, " Citron ", BeverageType::Seltzer, None).unwrap();
        assert_eq!(get_recipe(&conn, id).unwrap().name, "Citron");
        let err = create_recipe(&conn, "Citron", BeverageType::Cider, None).unwrap_err();
        assert_eq!(err.to_string(), "recipe 'Citron' already exists");

        let other = create_recipe(&conn, "Pomme", BeverageType::Cider, None).unwrap();
        let err = update_recipe(&conn, other, "Citron", BeverageType::Cider, None).unwrap_err();
        assert_eq!(err.code(), "db.duplicate");
        let err = update_recipe(&conn, 404, "X", BeverageType::Cider, None).unwrap_err();
        assert_eq!(err.code(), "db.no_such_recipe");
    }

    #[test]
    fn a_locked_version_freezes_the_type_but_not_the_name() {
        let conn = db();
        let (recipe, version) = seltzer(&conn);
        // Unlocked: anything goes.
        update_recipe(&conn, recipe, "Citron", BeverageType::Cider, None).unwrap();
        update_recipe(&conn, recipe, "Citron", BeverageType::Seltzer, None).unwrap();

        brew(&conn, version, "SELTZER-001");
        update_recipe(
            &conn,
            recipe,
            "Citron-Verveine",
            BeverageType::Seltzer,
            Some("renamed"),
        )
        .unwrap();
        let err =
            update_recipe(&conn, recipe, "Citron-Verveine", BeverageType::Cider, None).unwrap_err();
        assert_eq!(err.code(), "db.recipe_type_locked");
        assert_eq!(
            get_recipe(&conn, recipe).unwrap().beverage_type,
            BeverageType::Seltzer
        );
    }

    #[test]
    fn version_targets_are_checked_before_they_are_stored() {
        let conn = db();
        let (recipe, version) = seltzer(&conn);
        let targets = |og: f64, fg: f64| NewRecipeVersion {
            target_og: Some(Sg::new_unchecked(og)),
            target_fg: Some(Sg::new_unchecked(fg)),
            ..Default::default()
        };
        for (bad, code) in [
            (targets(1040.0, 1.000), "db.measurement_implausible"),
            (targets(1.000, 1.040), "units.range_inverted"),
            (
                NewRecipeVersion {
                    target_abv: Some(5.5),
                    ..Default::default()
                },
                "units.not_a_fraction",
            ),
            (
                NewRecipeVersion {
                    target_volume_l: Some(Litres::new(0.0).unwrap()),
                    ..Default::default()
                },
                "units.not_positive",
            ),
        ] {
            assert_eq!(
                update_recipe_version(&conn, version, &bad)
                    .unwrap_err()
                    .code(),
                code
            );
            assert_eq!(
                create_recipe_version(&conn, recipe, &bad)
                    .unwrap_err()
                    .code(),
                code
            );
        }
        // Equal OG and FG is not inverted: an unfermented sample says just that.
        update_recipe_version(&conn, version, &targets(1.040, 1.040)).unwrap();
        assert_eq!(list_recipe_versions(&conn, recipe).unwrap().len(), 1);
    }

    #[test]
    fn an_item_needs_a_positive_quantity_and_a_finite_offset() {
        let conn = db();
        let (_, version) = seltzer(&conn);
        let sugar = sucrose(&conn);
        let nutrient = fermaid(&conn);
        let err = add_recipe_item(&conn, version, &sugar_item(sugar, 0.0)).unwrap_err();
        assert_eq!(err.code(), "units.not_positive");
        let err =
            add_recipe_item(&conn, version, &nutrient_at(nutrient, 5.0, f64::NAN)).unwrap_err();
        assert_eq!(err.code(), "units.not_finite");

        // Before pitch is a real schedule, not a typo.
        let id = add_recipe_item(&conn, version, &nutrient_at(nutrient, 5.0, -2.0)).unwrap();
        let err = update_recipe_item(&conn, id, &nutrient_at(nutrient, -5.0, 24.0)).unwrap_err();
        assert_eq!(err.code(), "units.not_positive");
    }

    #[test]
    fn only_the_newest_unlocked_version_can_be_deleted() {
        let conn = db();
        let (recipe, v1) = seltzer(&conn);
        let sugar = sucrose(&conn);
        add_recipe_item(&conn, v1, &sugar_item(sugar, 4.0)).unwrap();
        brew(&conn, v1, "SELTZER-001");

        let v2 = fork_recipe_version(&conn, v1).unwrap();
        let v3 = fork_recipe_version(&conn, v2).unwrap();
        let err = delete_recipe_version(&conn, v2).unwrap_err();
        assert_eq!(err.code(), "db.recipe_version_not_newest");

        delete_recipe_version(&conn, v3).unwrap();
        delete_recipe_version(&conn, v2).unwrap();
        // The items went with the version, by the schema's cascade.
        assert!(list_recipe_items(&conn, v2).unwrap().is_empty());
        let err = delete_recipe_version(&conn, v1).unwrap_err();
        assert_eq!(err.code(), "db.recipe_version_locked");

        // The next fork is v2 again: numbers stay contiguous.
        let again = fork_recipe_version(&conn, v1).unwrap();
        assert_eq!(get_recipe_version(&conn, again).unwrap().version_no, 2);
        assert_eq!(list_recipe_versions(&conn, recipe).unwrap().len(), 2);
    }

    #[test]
    fn batches_are_counted_per_version() {
        let conn = db();
        let (recipe, v1) = seltzer(&conn);
        let v2 = fork_recipe_version(&conn, v1).unwrap();
        brew(&conn, v1, "SELTZER-001");
        brew(&conn, v1, "SELTZER-002");
        let counts = version_batch_counts(&conn, recipe).unwrap();
        assert_eq!(counts[&v1], 2);
        assert!(!counts.contains_key(&v2));
    }

    #[test]
    fn an_archived_recipe_comes_back() {
        let conn = db();
        let (recipe, _) = seltzer(&conn);
        archive_recipe(&conn, recipe, Timestamp::from_secs(1)).unwrap();
        assert!(restore_recipe(&conn, recipe).unwrap());
        assert!(!restore_recipe(&conn, recipe).unwrap());
        assert_eq!(list_recipes(&conn, false).unwrap().len(), 1);
    }
}
