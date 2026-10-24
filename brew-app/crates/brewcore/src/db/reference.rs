//! Phase 3a. Reference data: vessels, ingredients, yeast strains, lots, water.
//!
//! The first slice of Tier 2 — typed in once and never recoverable from Fermentrack,
//! which is the opposite of everything in [`tier1`](crate::db::tier1). Nothing here
//! is derived, so nothing here is ever deleted: `archived_at` is how a row leaves the
//! pickers while staying readable by every batch that already references it.
//!
//! Same contract as `tier1`: every function takes a borrowed connection and never
//! opens or commits its own transaction. The boundary belongs to the caller.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::enums::db_enum;
use crate::{
    Error, Result,
    units::{Celsius, Currency, Dimension, Grams, Litres, Money, Quantity, Timestamp, Unit},
};

db_enum!(
    /// What a vessel is for. Not a `CHECK` in the schema — see `0002_reference.sql`.
    VesselKind,
    UnknownVesselKind,
    (Fermenter, "fermenter"),
    (Brite, "brite"),
    (Keg, "keg"),
    (Carboy, "carboy"),
    (Kettle, "kettle"),
    (Other, "other"),
);

db_enum!(
    /// The ten categories from docs/BUILDOUT.md 3a.
    Category,
    UnknownCategory,
    (Fermentable, "fermentable"),
    (Sugar, "sugar"),
    (Yeast, "yeast"),
    (Nutrient, "nutrient"),
    (Acid, "acid"),
    (Salt, "salt"),
    (Flavoring, "flavoring"),
    (Fining, "fining"),
    (Gas, "gas"),
    (Water, "water"),
);

db_enum!(
    /// How the yeast was bought. Decides the pitch-rate maths in 3f.
    YeastForm,
    UnknownYeastForm,
    (Dry, "dry"),
    (Liquid, "liquid"),
    (Slurry, "slurry"),
);

// ---------------------------------------------------------------- shared

/// A `UNIQUE` violation as [`Error::Duplicate`]; any other error passes through.
pub(crate) fn duplicate<'a>(
    what: &'static str,
    name: &'a str,
) -> impl FnOnce(rusqlite::Error) -> Error + 'a {
    move |e| match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            Error::Duplicate {
                what,
                name: name.into(),
            }
        }
        other => other.into(),
    }
}

pub(crate) fn required_name<'a>(what: &'static str, name: &'a str) -> Result<&'a str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::NameEmpty { what });
    }
    Ok(name)
}

/// `table` is always a literal from this crate, never input.
pub(crate) fn archive_row(
    conn: &Connection,
    table: &'static str,
    id: i64,
    at: Timestamp,
) -> Result<bool> {
    let changed = conn.execute(
        &format!("UPDATE {table} SET archived_at = ? WHERE id = ? AND archived_at IS NULL"),
        params![at, id],
    )?;
    Ok(changed == 1)
}

pub(crate) fn restore_row(conn: &Connection, table: &'static str, id: i64) -> Result<bool> {
    let changed = conn.execute(
        &format!("UPDATE {table} SET archived_at = NULL WHERE id = ? AND archived_at IS NOT NULL"),
        [id],
    )?;
    Ok(changed == 1)
}

// ---------------------------------------------------------------- vessel

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VesselRow {
    pub id: i64,
    pub name: String,
    pub kind: VesselKind,
    pub capacity_l: Option<Litres>,
    pub notes: Option<String>,
    pub archived_at: Option<Timestamp>,
}

pub fn create_vessel(
    conn: &Connection,
    name: &str,
    kind: VesselKind,
    capacity_l: Option<Litres>,
    notes: Option<&str>,
) -> Result<i64> {
    // Enforced here rather than by a CHECK: this is the only write path, and adding a
    // CHECK to an existing SQLite table means rebuilding it.
    if let Some(capacity) = capacity_l.filter(|c| c.get() <= 0.0) {
        return Err(Error::NotPositive {
            field: "capacity_l",
            value: capacity.get(),
        });
    }
    conn.execute(
        "INSERT INTO vessel (name, kind, capacity_l, notes) VALUES (?, ?, ?, ?)",
        params![name, kind, capacity_l, notes],
    )
    .map_err(duplicate("vessel", name))?;
    Ok(conn.last_insert_rowid())
}

/// Active vessels, or all of them. Archived rows stay readable because a batch from
/// two years ago still points at one.
pub fn list_vessels(conn: &Connection, include_archived: bool) -> Result<Vec<VesselRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, capacity_l, notes, archived_at FROM vessel \
         WHERE ?1 OR archived_at IS NULL ORDER BY name",
    )?;
    let rows = stmt.query_map([include_archived], |row| {
        Ok(VesselRow {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
            capacity_l: row.get(3)?,
            notes: row.get(4)?,
            archived_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Returns whether a row changed — archiving an already-archived vessel is `false`,
/// not an error.
pub fn archive_vessel(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE vessel SET archived_at = ? WHERE id = ? AND archived_at IS NULL",
        params![at, id],
    )?;
    Ok(changed == 1)
}

/// Replace the streams a pitch into this vessel binds by default. Replaces rather than
/// adds, so the Vessels pane's checkboxes are the whole state and not a diff.
///
/// Touches no existing binding: a batch already in the vessel keeps what it was
/// pitched with. See `0008_vessel_stream.sql`.
pub fn set_vessel_streams(conn: &Connection, vessel_id: i64, stream_ids: &[i64]) -> Result<()> {
    conn.execute("DELETE FROM vessel_stream WHERE vessel_id = ?", [vessel_id])?;
    let mut insert =
        conn.prepare("INSERT OR IGNORE INTO vessel_stream (vessel_id, stream_id) VALUES (?, ?)")?;
    for stream_id in stream_ids {
        insert.execute([vessel_id, *stream_id])?;
    }
    Ok(())
}

pub fn list_vessel_streams(conn: &Connection, vessel_id: i64) -> Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT stream_id FROM vessel_stream WHERE vessel_id = ? ORDER BY stream_id")?;
    let rows = stmt.query_map([vessel_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------- yeast_strain

/// Attenuation and tolerance are fractions, not percents. The schema's `CHECK`
/// enforces it; this is the type that makes the caller notice.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewYeastStrain {
    pub name: String,
    pub lab: Option<String>,
    pub form: Option<YeastForm>,
    pub temp_min_c: Option<Celsius>,
    pub temp_max_c: Option<Celsius>,
    pub attenuation_min: Option<f64>,
    pub attenuation_max: Option<f64>,
    pub alcohol_tolerance: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct YeastStrainRow {
    pub id: i64,
    pub name: String,
    pub lab: Option<String>,
    pub form: YeastForm,
    pub temp_min_c: Option<Celsius>,
    pub temp_max_c: Option<Celsius>,
    pub attenuation_min: Option<f64>,
    pub attenuation_max: Option<f64>,
    pub alcohol_tolerance: Option<f64>,
    pub notes: Option<String>,
    pub archived_at: Option<Timestamp>,
}

impl YeastStrainRow {
    /// The band Phase 6's finish detector compares against, as `(min, max)` — only
    /// when both ends are known, because a half-open band cannot say "this stalled".
    pub fn attenuation_band(&self) -> Option<(f64, f64)> {
        self.attenuation_min.zip(self.attenuation_max)
    }
}

/// The fractions and both ranges, before the schema's `CHECK` sees them. The `CHECK`
/// stays the backstop; this is what names the field.
fn validate_strain(strain: &NewYeastStrain) -> Result<()> {
    for (field, value) in [
        ("attenuation_min", strain.attenuation_min),
        ("attenuation_max", strain.attenuation_max),
        ("alcohol_tolerance", strain.alcohol_tolerance),
    ] {
        let Some(value) = value else { continue };
        if !value.is_finite() {
            return Err(Error::NotFinite { field, value });
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::NotAFraction { field, value });
        }
    }
    if let (Some(min), Some(max)) = (strain.attenuation_min, strain.attenuation_max) {
        if min > max {
            return Err(Error::RangeInverted {
                field: "attenuation",
            });
        }
    }
    if let (Some(min), Some(max)) = (strain.temp_min_c, strain.temp_max_c) {
        if min.get() > max.get() {
            return Err(Error::RangeInverted { field: "temp" });
        }
    }
    Ok(())
}

/// `UNIQUE (lab, name)` never fires with no lab — SQLite treats every NULL as
/// distinct — so a second lab-less "EC-1118" is caught here instead. `IS` is the
/// comparison that matches NULL to NULL.
fn check_strain_unique(
    conn: &Connection,
    name: &str,
    lab: Option<&str>,
    except: Option<i64>,
) -> Result<()> {
    let taken: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM yeast_strain WHERE name = ? AND lab IS ? AND id IS NOT ?)",
        params![name, lab, except],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Error::Duplicate {
            what: "yeast strain",
            name: strain_label(name, lab),
        });
    }
    Ok(())
}

fn strain_label(name: &str, lab: Option<&str>) -> String {
    match lab {
        Some(lab) => format!("{name} ({lab})"),
        None => name.to_owned(),
    }
}

/// A blank lab is no lab: `""` and NULL would otherwise be two different labs.
fn strain_lab(strain: &NewYeastStrain) -> Option<&str> {
    strain
        .lab
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

pub fn create_yeast_strain(conn: &Connection, strain: &NewYeastStrain) -> Result<i64> {
    let name = required_name("yeast strain", &strain.name)?;
    let lab = strain_lab(strain);
    validate_strain(strain)?;
    check_strain_unique(conn, name, lab, None)?;
    conn.execute(
        "INSERT INTO yeast_strain \
         (name, lab, form, temp_min_c, temp_max_c, attenuation_min, attenuation_max, \
          alcohol_tolerance, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            name,
            lab,
            strain.form.unwrap_or(YeastForm::Dry),
            strain.temp_min_c,
            strain.temp_max_c,
            strain.attenuation_min,
            strain.attenuation_max,
            strain.alcohol_tolerance,
            strain.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Replaces every field. A band edited here moves 4c's expected FG and 5d's
/// band check for every past batch of the strain — both are computed on read, and a
/// corrected record is meant to.
pub fn update_yeast_strain(conn: &Connection, id: i64, strain: &NewYeastStrain) -> Result<()> {
    let name = required_name("yeast strain", &strain.name)?;
    let lab = strain_lab(strain);
    validate_strain(strain)?;
    check_strain_unique(conn, name, lab, Some(id))?;
    let changed = conn.execute(
        "UPDATE yeast_strain SET name = ?, lab = ?, form = ?, temp_min_c = ?, temp_max_c = ?, \
         attenuation_min = ?, attenuation_max = ?, alcohol_tolerance = ?, notes = ? \
         WHERE id = ?",
        params![
            name,
            lab,
            strain.form.unwrap_or(YeastForm::Dry),
            strain.temp_min_c,
            strain.temp_max_c,
            strain.attenuation_min,
            strain.attenuation_max,
            strain.alcohol_tolerance,
            strain.notes,
            id,
        ],
    )?;
    if changed == 0 {
        return Err(Error::NoSuchYeastStrain {
            yeast_strain_id: id,
        });
    }
    Ok(())
}

pub fn archive_yeast_strain(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    archive_row(conn, "yeast_strain", id, at)
}

pub fn restore_yeast_strain(conn: &Connection, id: i64) -> Result<bool> {
    restore_row(conn, "yeast_strain", id)
}

/// Active strains, or all of them. Analytics pass `true`: a strain retired since is
/// still what last year's batches were pitched with.
pub fn list_yeast_strains(
    conn: &Connection,
    include_archived: bool,
) -> Result<Vec<YeastStrainRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, lab, form, temp_min_c, temp_max_c, attenuation_min, \
                attenuation_max, alcohol_tolerance, notes, archived_at \
         FROM yeast_strain WHERE ?1 OR archived_at IS NULL ORDER BY lab, name",
    )?;
    let rows = stmt.query_map([include_archived], |row| {
        Ok(YeastStrainRow {
            id: row.get(0)?,
            name: row.get(1)?,
            lab: row.get(2)?,
            form: row.get(3)?,
            temp_min_c: row.get(4)?,
            temp_max_c: row.get(5)?,
            attenuation_min: row.get(6)?,
            attenuation_max: row.get(7)?,
            alcohol_tolerance: row.get(8)?,
            notes: row.get(9)?,
            archived_at: row.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ------------------------------------------------------------ ingredient

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewIngredient {
    pub name: String,
    pub category: Category,
    /// Its dimension decides which canonical column every lot of it fills.
    pub default_unit: Unit,
    pub yeast_strain_id: Option<i64>,
    /// Opaque JSON, written and read by the caller.
    pub attrs: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IngredientRow {
    pub id: i64,
    pub name: String,
    pub category: Category,
    pub default_unit: Unit,
    pub yeast_strain_id: Option<i64>,
    pub attrs: Option<String>,
    pub notes: Option<String>,
    pub archived_at: Option<Timestamp>,
}

/// The `attrs` key for a nutrient's mg of YAN per gram — `calc::yan`'s
/// `DAP_MG_YAN_PER_G` and its neighbours are the values to put there. In `attrs`
/// rather than a column: 0002 reserves it for "ppm contribution" and its kin, and
/// nothing filters on it.
pub const YAN_ATTR: &str = "mg_yan_per_g";

/// [`YAN_ATTR`] out of an ingredient's `attrs`. Missing, malformed or not positive is
/// `None`: 5a reports that batch's nitrogen as unknown rather than failing the table.
pub fn mg_yan_per_g(attrs: Option<&str>) -> Option<f64> {
    let attrs: serde_json::Value = serde_json::from_str(attrs?).ok()?;
    attrs
        .get(YAN_ATTR)?
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
}

impl IngredientRow {
    pub fn mg_yan_per_g(&self) -> Option<f64> {
        mg_yan_per_g(self.attrs.as_deref())
    }
}

/// `attrs` with [`YAN_ATTR`] set to `mg`, or removed for `None`, keeping every other
/// key. The one place `attrs` is written as JSON, so the frontend never builds it.
pub fn attrs_with_yan(attrs: Option<&str>, mg: Option<f64>) -> Result<Option<String>> {
    let mut map = match attrs {
        None => serde_json::Map::new(),
        Some(text) => match serde_json::from_str(text) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => return Err(Error::InvalidAttrs(text.to_owned())),
        },
    };
    match mg {
        Some(value) if !value.is_finite() => {
            return Err(Error::NotFinite {
                field: YAN_ATTR,
                value,
            });
        }
        Some(value) if value <= 0.0 => {
            return Err(Error::NotPositive {
                field: YAN_ATTR,
                value,
            });
        }
        Some(value) => {
            map.insert(YAN_ATTR.into(), value.into());
        }
        None => {
            map.remove(YAN_ATTR);
        }
    }
    Ok((!map.is_empty()).then(|| serde_json::Value::Object(map).to_string()))
}

/// A yeast must name its strain and nothing else may. Enforced here, at the only
/// write path, rather than in a form: a yeast with no strain is a batch 5a and 5d
/// report as `no_yeast`, invisible to every strain query.
fn check_strain_link(conn: &Connection, ingredient: &NewIngredient) -> Result<()> {
    match (ingredient.category, ingredient.yeast_strain_id) {
        (Category::Yeast, None) => Err(Error::YeastNeedsStrain),
        (Category::Yeast, Some(yeast_strain_id)) => {
            let exists: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM yeast_strain WHERE id = ?)",
                [yeast_strain_id],
                |row| row.get(0),
            )?;
            if exists {
                Ok(())
            } else {
                Err(Error::NoSuchYeastStrain { yeast_strain_id })
            }
        }
        (category, Some(_)) => Err(Error::StrainNotYeast { category }),
        (_, None) => Ok(()),
    }
}

/// Whether any lot or plan line reads this ingredient's canonical column. Actuals
/// point at a lot, so `batch_ingredient` is covered by `ingredient_lot`.
fn ingredient_in_use(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM ingredient_lot WHERE ingredient_id = ?1) \
             OR EXISTS (SELECT 1 FROM recipe_item WHERE ingredient_id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

pub fn create_ingredient(conn: &Connection, ingredient: &NewIngredient) -> Result<i64> {
    let name = required_name("ingredient", &ingredient.name)?;
    check_strain_link(conn, ingredient)?;
    conn.execute(
        "INSERT INTO ingredient (name, category, default_unit, yeast_strain_id, attrs, notes) \
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            name,
            ingredient.category,
            ingredient.default_unit,
            ingredient.yeast_strain_id,
            ingredient.attrs,
            ingredient.notes,
        ],
    )
    .map_err(duplicate("ingredient", name))?;
    Ok(conn.last_insert_rowid())
}

/// Replaces every field. `default_unit` may change within its dimension (`g` → `kg`:
/// the canonical column is the same), but not across one once anything uses it.
pub fn update_ingredient(conn: &Connection, id: i64, ingredient: &NewIngredient) -> Result<()> {
    let name = required_name("ingredient", &ingredient.name)?;
    let current: Unit = conn
        .query_row(
            "SELECT default_unit FROM ingredient WHERE id = ?",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NoSuchIngredient { ingredient_id: id })?;

    let (from, to) = (current.dimension(), ingredient.default_unit.dimension());
    if from != to && ingredient_in_use(conn, id)? {
        return Err(Error::IngredientDimensionLocked {
            ingredient_id: id,
            from,
            to,
        });
    }
    check_strain_link(conn, ingredient)?;

    conn.execute(
        "UPDATE ingredient SET name = ?, category = ?, default_unit = ?, yeast_strain_id = ?, \
         attrs = ?, notes = ? WHERE id = ?",
        params![
            name,
            ingredient.category,
            ingredient.default_unit,
            ingredient.yeast_strain_id,
            ingredient.attrs,
            ingredient.notes,
            id,
        ],
    )
    .map_err(duplicate("ingredient", name))?;
    Ok(())
}

pub fn get_ingredient(conn: &Connection, id: i64) -> Result<IngredientRow> {
    conn.query_row(
        "SELECT id, name, category, default_unit, yeast_strain_id, attrs, notes, archived_at \
         FROM ingredient WHERE id = ?",
        [id],
        ingredient_row,
    )
    .optional()?
    .ok_or(Error::NoSuchIngredient { ingredient_id: id })
}

fn ingredient_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IngredientRow> {
    Ok(IngredientRow {
        id: row.get(0)?,
        name: row.get(1)?,
        category: row.get(2)?,
        default_unit: row.get(3)?,
        yeast_strain_id: row.get(4)?,
        attrs: row.get(5)?,
        notes: row.get(6)?,
        archived_at: row.get(7)?,
    })
}

pub fn list_ingredients(conn: &Connection, include_archived: bool) -> Result<Vec<IngredientRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, category, default_unit, yeast_strain_id, attrs, notes, archived_at \
         FROM ingredient WHERE ?1 OR archived_at IS NULL ORDER BY category, name",
    )?;
    let rows = stmt.query_map([include_archived], ingredient_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn archive_ingredient(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    archive_row(conn, "ingredient", id, at)
}

pub fn restore_ingredient(conn: &Connection, id: i64) -> Result<bool> {
    restore_row(conn, "ingredient", id)
}

/// Every ingredient's name and whether it is archived, by id — what a recipe line or
/// a diff row needs to say which ingredient it is.
pub fn ingredient_names(conn: &Connection) -> Result<HashMap<i64, (String, bool)>> {
    let mut stmt = conn.prepare("SELECT id, name, archived_at IS NOT NULL FROM ingredient")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Active lots per ingredient, for the list's "3 lots". An ingredient with none is
/// absent, not zero.
pub fn active_lot_counts(conn: &Connection) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare(
        "SELECT ingredient_id, COUNT(*) FROM ingredient_lot \
         WHERE archived_at IS NULL GROUP BY ingredient_id",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ------------------------------------------------- canonical quantities

/// The canonical quantity of a row, in whichever dimension its ingredient uses.
///
/// An enum rather than three `Option`s because exactly one is ever set — the same
/// invariant the schema's `CHECK` states, expressed so the Rust side cannot violate
/// it in the first place.
///
/// Shared with [`recipe`](crate::db::recipe): `ingredient_lot` and `recipe_item`
/// carry the same `qty_g`/`qty_l`/`qty_each` triple under the same `CHECK`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CanonicalQty {
    Mass(Grams),
    Volume(Litres),
    Count(f64),
}

impl CanonicalQty {
    /// The number, in grams / litres / units. What a cost divides by.
    pub fn amount(self) -> f64 {
        match self {
            CanonicalQty::Mass(g) => g.get(),
            CanonicalQty::Volume(l) => l.get(),
            CanonicalQty::Count(n) => n,
        }
    }

    pub fn dimension(self) -> Dimension {
        match self {
            CanonicalQty::Mass(_) => Dimension::Mass,
            CanonicalQty::Volume(_) => Dimension::Volume,
            CanonicalQty::Count(_) => Dimension::Count,
        }
    }
}

/// The three canonical columns, derived from what was typed and the ingredient's
/// own `default_unit`.
///
/// The dimension check is the point of it: a caller hands over one [`Quantity`] and
/// never chooses a column, so a volume can never land in `qty_g` where every later
/// `SUM` would read it as grams. Shared by `create_lot` and `recipe::add_item`.
pub(crate) fn canonical_for_ingredient(
    conn: &Connection,
    ingredient_id: i64,
    entry: Quantity,
) -> Result<(Option<Grams>, Option<Litres>, Option<f64>)> {
    let default_unit: Unit = conn
        .query_row(
            "SELECT default_unit FROM ingredient WHERE id = ?",
            [ingredient_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NoSuchIngredient { ingredient_id })?;

    let wanted = default_unit.dimension();
    if entry.unit.dimension() != wanted {
        return Err(Error::DimensionMismatch {
            unit: entry.unit.symbol(),
            from: entry.unit.dimension(),
            to: wanted,
        });
    }

    Ok(match wanted {
        Dimension::Mass => (Some(entry.to_grams()?), None, None),
        Dimension::Volume => (None, Some(entry.to_litres()?), None),
        Dimension::Count => (None, None, Some(entry.to_count()?)),
    })
}

/// The read-back half: three nullable columns to the one that is set.
///
/// `column` is the index of `qty_g` in the `SELECT`, so the error points at a real
/// column. The schema's `CHECK` makes the fallthrough unreachable; reading it back
/// as an error rather than picking one beats a silent zero if it ever is reached.
pub(crate) fn canonical_from_columns(
    column: usize,
    g: Option<Grams>,
    l: Option<Litres>,
    each: Option<f64>,
) -> rusqlite::Result<CanonicalQty> {
    match (g, l, each) {
        (Some(g), None, None) => Ok(CanonicalQty::Mass(g)),
        (None, Some(l), None) => Ok(CanonicalQty::Volume(l)),
        (None, None, Some(n)) => Ok(CanonicalQty::Count(n)),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Real,
            Box::new(Error::UnknownUnit("row has no single canonical qty".into())),
        )),
    }
}

// -------------------------------------------------------- ingredient_lot

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewLot {
    pub ingredient_id: i64,
    pub supplier: Option<String>,
    pub lot_code: Option<String>,
    /// As typed: `2.5 kg`. [`create_lot`] derives the canonical column from it.
    pub entry: Quantity,
    /// What was paid for the whole lot, not a unit price. See `0002_reference.sql`.
    pub cost: Option<Money>,
    pub purchased_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LotRow {
    pub id: i64,
    pub ingredient_id: i64,
    pub supplier: Option<String>,
    pub lot_code: Option<String>,
    pub qty: CanonicalQty,
    pub entry: Quantity,
    pub cost: Option<Money>,
    pub purchased_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    pub notes: Option<String>,
    pub archived_at: Option<Timestamp>,
}

impl LotRow {
    /// Minor units per gram / litre / unit, as a float.
    ///
    /// Deliberately *not* a stored column and deliberately not `Money`: EUR 12.50
    /// over 5 kg is 0.25 minor units per gram, and any integer type rounds that to
    /// zero. The division happens here, once, at the point something wants to
    /// multiply it back up by a batch's usage.
    pub fn cost_per_canonical_unit(&self) -> Option<f64> {
        let cost = self.cost?;
        let amount = self.qty.amount();
        (amount != 0.0).then(|| cost.minor as f64 / amount)
    }
}

/// A positive amount, in the ingredient's canonical column. Lots and recipe lines both
/// go through here: a lot of nothing has no cost per gram, and a line of nothing plans
/// nothing — a negative one is a typo either way.
pub(crate) fn positive_canonical(
    conn: &Connection,
    ingredient_id: i64,
    entry: Quantity,
) -> Result<(Option<Grams>, Option<Litres>, Option<f64>)> {
    if entry.value <= 0.0 {
        return Err(Error::NotPositive {
            field: "quantity",
            value: entry.value,
        });
    }
    canonical_for_ingredient(conn, ingredient_id, entry)
}

/// Insert a lot, converting the entered quantity to the ingredient's canonical unit.
pub fn create_lot(conn: &Connection, lot: &NewLot) -> Result<i64> {
    let (qty_g, qty_l, qty_each) = positive_canonical(conn, lot.ingredient_id, lot.entry)?;

    conn.execute(
        "INSERT INTO ingredient_lot \
         (ingredient_id, supplier, lot_code, qty_g, qty_l, qty_each, entry_qty, entry_unit, \
          total_cost_minor, currency, purchased_at, expires_at, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            lot.ingredient_id,
            lot.supplier,
            lot.lot_code,
            qty_g,
            qty_l,
            qty_each,
            lot.entry.value,
            lot.entry.unit,
            lot.cost.map(|c| c.minor),
            lot.cost.map(|c| c.currency),
            lot.purchased_at,
            lot.expires_at,
            lot.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Replaces every field but the ingredient, re-deriving the canonical column from the
/// new entry. Allowed on a lot batches already used: cost and quantity are read at
/// query time, so a corrected receipt corrects every `batch_cost` — which is the point
/// of correcting it.
pub fn update_lot(conn: &Connection, lot_id: i64, lot: &NewLot) -> Result<()> {
    let ingredient_id: i64 = conn
        .query_row(
            "SELECT ingredient_id FROM ingredient_lot WHERE id = ?",
            [lot_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NoSuchLot { lot_id })?;
    if ingredient_id != lot.ingredient_id {
        return Err(Error::LotIngredientChanged { lot_id });
    }
    let (qty_g, qty_l, qty_each) = positive_canonical(conn, ingredient_id, lot.entry)?;

    conn.execute(
        "UPDATE ingredient_lot SET supplier = ?, lot_code = ?, qty_g = ?, qty_l = ?, \
         qty_each = ?, entry_qty = ?, entry_unit = ?, total_cost_minor = ?, currency = ?, \
         purchased_at = ?, expires_at = ?, notes = ? WHERE id = ?",
        params![
            lot.supplier,
            lot.lot_code,
            qty_g,
            qty_l,
            qty_each,
            lot.entry.value,
            lot.entry.unit,
            lot.cost.map(|c| c.minor),
            lot.cost.map(|c| c.currency),
            lot.purchased_at,
            lot.expires_at,
            lot.notes,
            lot_id,
        ],
    )?;
    Ok(())
}

/// Until Phase 7's lot consumption, this is how a lot is marked used up.
pub fn archive_lot(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    archive_row(conn, "ingredient_lot", id, at)
}

pub fn restore_lot(conn: &Connection, id: i64) -> Result<bool> {
    restore_row(conn, "ingredient_lot", id)
}

/// Distinct batches per lot of one ingredient — the count that links to 5c's lot
/// filter. A lot no batch used is absent, not zero.
pub fn lot_batch_counts(conn: &Connection, ingredient_id: i64) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare(
        "SELECT bi.ingredient_lot_id, COUNT(DISTINCT bi.batch_id) \
         FROM batch_ingredient bi \
         JOIN ingredient_lot l ON l.id = bi.ingredient_lot_id \
         WHERE l.ingredient_id = ? GROUP BY bi.ingredient_lot_id",
    )?;
    let rows = stmt.query_map([ingredient_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn list_lots_for(
    conn: &Connection,
    ingredient_id: i64,
    include_archived: bool,
) -> Result<Vec<LotRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, ingredient_id, supplier, lot_code, qty_g, qty_l, qty_each, \
                entry_qty, entry_unit, total_cost_minor, currency, purchased_at, expires_at, \
                notes, archived_at \
         FROM ingredient_lot WHERE ingredient_id = ?1 AND (?2 OR archived_at IS NULL) \
         ORDER BY purchased_at, id",
    )?;
    let rows = stmt.query_map(params![ingredient_id, include_archived], |row| {
        let qty = canonical_from_columns(4, row.get(4)?, row.get(5)?, row.get(6)?)?;
        let cost = row
            .get::<_, Option<i64>>(9)?
            .zip(row.get::<_, Option<Currency>>(10)?)
            .map(|(minor, currency)| Money::new(minor, currency));

        Ok(LotRow {
            id: row.get(0)?,
            ingredient_id: row.get(1)?,
            supplier: row.get(2)?,
            lot_code: row.get(3)?,
            qty,
            entry: Quantity {
                value: row.get(7)?,
                unit: row.get(8)?,
            },
            cost,
            purchased_at: row.get(11)?,
            expires_at: row.get(12)?,
            notes: row.get(13)?,
            archived_at: row.get(14)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// --------------------------------------------------------- water_profile

/// Ions in ppm — mg/L, which for water is the same number. Every field optional:
/// a municipal report leaves most of them blank, and a profile with three known
/// ions is still worth recording.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WaterProfile {
    pub name: String,
    pub calcium_ppm: Option<f64>,
    pub magnesium_ppm: Option<f64>,
    pub sodium_ppm: Option<f64>,
    pub chloride_ppm: Option<f64>,
    pub sulfate_ppm: Option<f64>,
    pub bicarbonate_ppm: Option<f64>,
    pub ph: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WaterProfileRow {
    pub id: i64,
    #[serde(flatten)]
    pub profile: WaterProfile,
}

pub fn create_water_profile(conn: &Connection, profile: &WaterProfile) -> Result<i64> {
    conn.execute(
        "INSERT INTO water_profile \
         (name, calcium_ppm, magnesium_ppm, sodium_ppm, chloride_ppm, sulfate_ppm, \
          bicarbonate_ppm, ph, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            profile.name,
            profile.calcium_ppm,
            profile.magnesium_ppm,
            profile.sodium_ppm,
            profile.chloride_ppm,
            profile.sulfate_ppm,
            profile.bicarbonate_ppm,
            profile.ph,
            profile.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_water_profiles(conn: &Connection) -> Result<Vec<WaterProfileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, calcium_ppm, magnesium_ppm, sodium_ppm, chloride_ppm, \
                sulfate_ppm, bicarbonate_ppm, ph, notes \
         FROM water_profile ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(WaterProfileRow {
            id: row.get(0)?,
            profile: WaterProfile {
                name: row.get(1)?,
                calcium_ppm: row.get(2)?,
                magnesium_ppm: row.get(3)?,
                sodium_ppm: row.get(4)?,
                chloride_ppm: row.get(5)?,
                sulfate_ppm: row.get(6)?,
                bicarbonate_ppm: row.get(7)?,
                ph: row.get(8)?,
                notes: row.get(9)?,
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn sucrose(conn: &Connection) -> i64 {
        create_ingredient(
            conn,
            &NewIngredient {
                name: "Sucrose".into(),
                category: Category::Sugar,
                default_unit: Unit::Kg,
                yeast_strain_id: None,
                attrs: None,
                notes: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn a_lot_entered_in_kg_is_stored_in_grams_and_prints_back_in_kg() {
        let conn = db();
        let id = sucrose(&conn);
        create_lot(
            &conn,
            &NewLot {
                ingredient_id: id,
                supplier: Some("Metro".into()),
                lot_code: Some("L-2026-04".into()),
                entry: Quantity::new(5.0, Unit::Kg).unwrap(),
                cost: Some(Money::parse("12,50", Currency::EUR).unwrap()),
                purchased_at: Some(Timestamp::from_secs(1_700_000_000)),
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();

        let lots = list_lots_for(&conn, id, false).unwrap();
        assert_eq!(lots.len(), 1);
        assert_eq!(
            lots[0].qty,
            CanonicalQty::Mass(Grams::new_unchecked(5000.0))
        );
        // The entry pair survives, so the lot reads back the way it was typed.
        assert_eq!(lots[0].entry, Quantity::new(5.0, Unit::Kg).unwrap());
    }

    /// The case that decided against a stored `unit_cost_minor`: as an integer, this
    /// is 0.
    #[test]
    fn cost_per_gram_of_a_cheap_bulk_lot_does_not_round_to_zero() {
        let conn = db();
        let id = sucrose(&conn);
        create_lot(
            &conn,
            &NewLot {
                ingredient_id: id,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(5.0, Unit::Kg).unwrap(),
                cost: Some(Money::parse("12.50", Currency::EUR).unwrap()),
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();

        let lot = &list_lots_for(&conn, id, false).unwrap()[0];
        assert_eq!(lot.cost, Some(Money::new(1250, Currency::EUR)));
        // 1250 minor units over 5000 g.
        assert!(crate::units::close(
            lot.cost_per_canonical_unit().unwrap(),
            0.25
        ));
    }

    #[test]
    fn a_volume_against_a_mass_ingredient_is_refused() {
        let conn = db();
        let id = sucrose(&conn);
        let err = create_lot(
            &conn,
            &NewLot {
                ingredient_id: id,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(5.0, Unit::L).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .expect_err("litres of a kg ingredient");
        assert_eq!(err.code(), "units.dimension_mismatch");
    }

    #[test]
    fn a_lot_of_an_ingredient_that_does_not_exist_names_the_ingredient() {
        let conn = db();
        let err = create_lot(
            &conn,
            &NewLot {
                ingredient_id: 404,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(1.0, Unit::Kg).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .expect_err("dangling ingredient_id");
        assert_eq!(err.code(), "db.no_such_ingredient");
    }

    #[test]
    fn a_lot_with_two_canonical_quantities_is_refused_by_the_schema() {
        let conn = db();
        let id = sucrose(&conn);
        let err = conn
            .execute(
                "INSERT INTO ingredient_lot (ingredient_id, qty_g, qty_l, entry_qty, entry_unit) \
                 VALUES (?, 1000.0, 1.0, 1.0, 'kg')",
                [id],
            )
            .expect_err("two canonical columns");
        assert!(err.to_string().contains("CHECK"), "unexpected: {err}");
    }

    #[test]
    fn an_amount_without_a_currency_is_refused_by_the_schema() {
        let conn = db();
        let id = sucrose(&conn);
        let err = conn
            .execute(
                "INSERT INTO ingredient_lot \
                 (ingredient_id, qty_g, entry_qty, entry_unit, total_cost_minor) \
                 VALUES (?, 1000.0, 1.0, 'kg', 1250)",
                [id],
            )
            .expect_err("cost with no currency");
        assert!(err.to_string().contains("CHECK"), "unexpected: {err}");
    }

    #[test]
    fn a_count_ingredient_fills_qty_each() {
        let conn = db();
        let id = create_ingredient(
            &conn,
            &NewIngredient {
                name: "Campden tablets".into(),
                category: Category::Fining,
                default_unit: Unit::Each,
                yeast_strain_id: None,
                attrs: None,
                notes: None,
            },
        )
        .unwrap();
        create_lot(
            &conn,
            &NewLot {
                ingredient_id: id,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(100.0, Unit::Each).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        assert_eq!(
            list_lots_for(&conn, id, false).unwrap()[0].qty,
            CanonicalQty::Count(100.0)
        );
    }

    #[test]
    fn archiving_a_vessel_hides_it_without_deleting_it() {
        let conn = db();
        let id = create_vessel(
            &conn,
            "FV1",
            VesselKind::Fermenter,
            Some(Litres::new(60.0).unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(list_vessels(&conn, false).unwrap().len(), 1);

        let at = Timestamp::from_secs(1_700_000_000);
        assert!(archive_vessel(&conn, id, at).unwrap());
        // Archiving twice is a no-op, not an error.
        assert!(!archive_vessel(&conn, id, at).unwrap());

        assert!(list_vessels(&conn, false).unwrap().is_empty());
        let all = list_vessels(&conn, true).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].archived_at, Some(at));
    }

    #[test]
    fn a_vessel_capacity_must_be_positive() {
        let conn = db();
        for capacity in [0.0, -5.0] {
            let err = create_vessel(
                &conn,
                "FV1",
                VesselKind::Fermenter,
                Some(Litres::new(capacity).unwrap()),
                None,
            )
            .unwrap_err();
            assert_eq!(err.code(), "units.not_positive", "{capacity}");
        }
        assert!(list_vessels(&conn, true).unwrap().is_empty());
    }

    #[test]
    fn a_vessels_default_streams_are_replaced_not_appended() {
        let mut conn = db();
        let vessel = create_vessel(&conn, "FV1", VesselKind::Fermenter, None, None).unwrap();
        let source = crate::db::tier1::create_source(
            &conn,
            crate::db::SourceKind::BrewdJournal,
            "esp32-1",
            None,
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let a =
            crate::db::tier1::create_stream(&tx, source, "vessel_temp", crate::db::Metric::TempC)
                .unwrap();
        let b = crate::db::tier1::create_stream(
            &tx,
            source,
            "chamber_temp",
            crate::db::Metric::FridgeTempC,
        )
        .unwrap();
        tx.commit().unwrap();

        set_vessel_streams(&conn, vessel, &[a, b, a]).unwrap();
        assert_eq!(list_vessel_streams(&conn, vessel).unwrap(), [a, b]);
        set_vessel_streams(&conn, vessel, &[b]).unwrap();
        assert_eq!(list_vessel_streams(&conn, vessel).unwrap(), [b]);
    }

    #[test]
    fn a_strain_carries_the_band_the_finish_detector_needs() {
        let conn = db();
        create_yeast_strain(
            &conn,
            &NewYeastStrain {
                name: "EC-1118".into(),
                lab: Some("Lallemand".into()),
                form: Some(YeastForm::Dry),
                temp_min_c: Some(Celsius::new(10.0).unwrap()),
                temp_max_c: Some(Celsius::new(30.0).unwrap()),
                attenuation_min: Some(0.90),
                attenuation_max: Some(1.0),
                alcohol_tolerance: Some(0.18),
                ..Default::default()
            },
        )
        .unwrap();

        let strains = list_yeast_strains(&conn, false).unwrap();
        assert_eq!(strains[0].attenuation_band(), Some((0.90, 1.0)));
    }

    /// Fractions, not percents — a `78` here would sail through as a plausible band.
    #[test]
    fn an_attenuation_typed_as_a_percent_is_refused() {
        let conn = db();
        let err = create_yeast_strain(
            &conn,
            &NewYeastStrain {
                name: "EC-1118".into(),
                attenuation_min: Some(90.0),
                ..Default::default()
            },
        )
        .expect_err("90 is not a fraction");
        assert_eq!(err.code(), "units.not_a_fraction");
        assert!(err.to_string().contains("attenuation_min"), "{err}");

        // The schema's CHECK is still the backstop under the Rust check.
        let err = conn
            .execute(
                "INSERT INTO yeast_strain (name, form, alcohol_tolerance) VALUES ('x', 'dry', 14)",
                [],
            )
            .expect_err("14 is not a fraction");
        assert!(err.to_string().contains("CHECK"), "unexpected: {err}");
    }

    #[test]
    fn a_strain_with_an_inverted_range_is_refused() {
        let conn = db();
        for (strain, field) in [
            (
                NewYeastStrain {
                    name: "A".into(),
                    attenuation_min: Some(0.9),
                    attenuation_max: Some(0.8),
                    ..Default::default()
                },
                "attenuation",
            ),
            (
                NewYeastStrain {
                    name: "B".into(),
                    temp_min_c: Some(Celsius::new(25.0).unwrap()),
                    temp_max_c: Some(Celsius::new(15.0).unwrap()),
                    ..Default::default()
                },
                "temp",
            ),
        ] {
            let err = create_yeast_strain(&conn, &strain).unwrap_err();
            assert!(
                matches!(err, Error::RangeInverted { field: f } if f == field),
                "{err}"
            );
        }
        assert!(list_yeast_strains(&conn, true).unwrap().is_empty());
    }

    /// `UNIQUE (lab, name)` treats two NULL labs as different, so this is the Rust
    /// check doing what the index cannot.
    #[test]
    fn a_strain_is_unique_by_lab_and_name_even_with_no_lab() {
        let conn = db();
        let strain = |name: &str, lab: Option<&str>| NewYeastStrain {
            name: name.into(),
            lab: lab.map(str::to_owned),
            ..Default::default()
        };
        create_yeast_strain(&conn, &strain("EC-1118", None)).unwrap();
        let err = create_yeast_strain(&conn, &strain("EC-1118", Some("  "))).unwrap_err();
        assert_eq!(err.code(), "db.duplicate");

        let lallemand = create_yeast_strain(&conn, &strain("EC-1118", Some("Lallemand"))).unwrap();
        let err = create_yeast_strain(&conn, &strain("EC-1118", Some("Lallemand"))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yeast strain 'EC-1118 (Lallemand)' already exists"
        );

        // Saving a strain under its own name is not a collision with itself.
        update_yeast_strain(&conn, lallemand, &strain("EC-1118", Some("Lallemand"))).unwrap();
        let err = update_yeast_strain(&conn, lallemand, &strain("EC-1118", None)).unwrap_err();
        assert_eq!(err.code(), "db.duplicate");
        let err = update_yeast_strain(&conn, 404, &strain("X", None)).unwrap_err();
        assert_eq!(err.code(), "db.no_such_yeast_strain");
    }

    #[test]
    fn an_archived_strain_leaves_the_list_and_comes_back() {
        let conn = db();
        let id = create_yeast_strain(
            &conn,
            &NewYeastStrain {
                name: "US-05".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let at = Timestamp::from_secs(1_700_000_000);
        assert!(archive_yeast_strain(&conn, id, at).unwrap());
        assert!(!archive_yeast_strain(&conn, id, at).unwrap());
        assert!(list_yeast_strains(&conn, false).unwrap().is_empty());
        assert_eq!(
            list_yeast_strains(&conn, true).unwrap()[0].archived_at,
            Some(at)
        );

        assert!(restore_yeast_strain(&conn, id).unwrap());
        assert!(!restore_yeast_strain(&conn, id).unwrap());
        assert_eq!(list_yeast_strains(&conn, false).unwrap().len(), 1);
    }

    fn ingredient(name: &str, category: Category, strain: Option<i64>) -> NewIngredient {
        NewIngredient {
            name: name.into(),
            category,
            default_unit: Unit::G,
            yeast_strain_id: strain,
            attrs: None,
            notes: None,
        }
    }

    #[test]
    fn a_yeast_names_its_strain_and_nothing_else_does() {
        let conn = db();
        let err =
            create_ingredient(&conn, &ingredient("EC-1118", Category::Yeast, None)).unwrap_err();
        assert_eq!(err.code(), "db.yeast_needs_strain");
        let err = create_ingredient(&conn, &ingredient("EC-1118", Category::Yeast, Some(404)))
            .unwrap_err();
        assert_eq!(err.code(), "db.no_such_yeast_strain");

        let strain = create_yeast_strain(
            &conn,
            &NewYeastStrain {
                name: "EC-1118".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let err = create_ingredient(&conn, &ingredient("Sucrose", Category::Sugar, Some(strain)))
            .unwrap_err();
        assert_eq!(err.code(), "db.strain_not_yeast");

        let id = create_ingredient(&conn, &ingredient("EC-1118", Category::Yeast, Some(strain)))
            .unwrap();
        // Recategorising keeps the rule: a yeast turned sugar must drop its strain.
        let err = update_ingredient(
            &conn,
            id,
            &ingredient("EC-1118", Category::Sugar, Some(strain)),
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.strain_not_yeast");
    }

    #[test]
    fn an_ingredient_name_is_trimmed_required_and_unique() {
        let conn = db();
        let err = create_ingredient(&conn, &ingredient("  ", Category::Sugar, None)).unwrap_err();
        assert_eq!(err.code(), "db.name_empty");

        let id = create_ingredient(&conn, &ingredient(" Sucrose ", Category::Sugar, None)).unwrap();
        assert_eq!(get_ingredient(&conn, id).unwrap().name, "Sucrose");
        let err =
            create_ingredient(&conn, &ingredient("Sucrose", Category::Sugar, None)).unwrap_err();
        assert_eq!(err.to_string(), "ingredient 'Sucrose' already exists");

        // Unique across archived rows too, which is why edits exist at all.
        archive_ingredient(&conn, id, Timestamp::from_secs(1)).unwrap();
        let err =
            create_ingredient(&conn, &ingredient("Sucrose", Category::Sugar, None)).unwrap_err();
        assert_eq!(err.code(), "db.duplicate");

        let dextrose =
            create_ingredient(&conn, &ingredient("Dextrose", Category::Sugar, None)).unwrap();
        let err = update_ingredient(
            &conn,
            dextrose,
            &ingredient("Sucrose", Category::Sugar, None),
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.duplicate");
        let err =
            update_ingredient(&conn, 404, &ingredient("X", Category::Sugar, None)).unwrap_err();
        assert_eq!(err.code(), "db.no_such_ingredient");
    }

    #[test]
    fn a_duplicate_vessel_name_is_named() {
        let conn = db();
        create_vessel(&conn, "FV1", VesselKind::Fermenter, None, None).unwrap();
        let err = create_vessel(&conn, "FV1", VesselKind::Keg, None, None).unwrap_err();
        assert_eq!(err.to_string(), "vessel 'FV1' already exists");
    }

    fn unit_edit(unit: Unit) -> NewIngredient {
        NewIngredient {
            default_unit: unit,
            ..ingredient("Sucrose", Category::Sugar, None)
        }
    }

    fn one_kg_lot(conn: &Connection, ingredient_id: i64) -> i64 {
        create_lot(
            conn,
            &NewLot {
                ingredient_id,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(1.0, Unit::Kg).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn an_unused_ingredient_may_change_dimension() {
        let conn = db();
        let id = sucrose(&conn);
        update_ingredient(&conn, id, &unit_edit(Unit::L)).unwrap();
        assert_eq!(get_ingredient(&conn, id).unwrap().default_unit, Unit::L);
    }

    #[test]
    fn a_lot_locks_the_dimension_but_not_the_unit() {
        let conn = db();
        let id = sucrose(&conn);
        one_kg_lot(&conn, id);

        // g and kg share the canonical column, so this changes nothing stored.
        update_ingredient(&conn, id, &unit_edit(Unit::G)).unwrap();
        let err = update_ingredient(&conn, id, &unit_edit(Unit::L)).unwrap_err();
        assert_eq!(err.code(), "db.ingredient_dimension_locked");
        assert!(err.to_string().contains("from mass to volume"), "{err}");
    }

    #[test]
    fn a_recipe_line_locks_the_dimension() {
        use crate::db::recipe::{
            BeverageType, NewRecipeItem, NewRecipeVersion, Stage, add_recipe_item, create_recipe,
            create_recipe_version,
        };
        let conn = db();
        let id = sucrose(&conn);
        let recipe = create_recipe(&conn, "Citron", BeverageType::Seltzer, None).unwrap();
        let version = create_recipe_version(&conn, recipe, &NewRecipeVersion::default()).unwrap();
        add_recipe_item(
            &conn,
            version,
            &NewRecipeItem {
                ingredient_id: id,
                entry: Quantity::new(4.0, Unit::Kg).unwrap(),
                stage: Stage::Prep,
                offset_hours: None,
                notes: None,
            },
        )
        .unwrap();
        let err = update_ingredient(&conn, id, &unit_edit(Unit::Each)).unwrap_err();
        assert_eq!(err.code(), "db.ingredient_dimension_locked");
    }

    /// The batch's actual points at the lot, so an archived lot still locks — the
    /// batch still reads its `qty_g`.
    #[test]
    fn an_archived_lot_a_batch_used_still_locks_the_dimension() {
        use crate::db::{
            batch::{NewBatch, NewBatchIngredient, add_batch_ingredient, create_batch},
            recipe::{BeverageType, Stage},
        };
        let conn = db();
        let id = sucrose(&conn);
        let lot = one_kg_lot(&conn, id);
        let batch = create_batch(
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
            Timestamp::from_secs(1),
        )
        .unwrap();
        add_batch_ingredient(
            &conn,
            batch,
            &NewBatchIngredient {
                ingredient_lot_id: lot,
                entry: Quantity::new(0.5, Unit::Kg).unwrap(),
                stage: Stage::Prep,
                added_at: None,
                notes: None,
            },
        )
        .unwrap();
        archive_lot(&conn, lot, Timestamp::from_secs(2)).unwrap();

        let err = update_ingredient(&conn, id, &unit_edit(Unit::L)).unwrap_err();
        assert_eq!(err.code(), "db.ingredient_dimension_locked");
        assert_eq!(lot_batch_counts(&conn, id).unwrap()[&lot], 1);
        // Archived lots are not counted as stock.
        assert!(active_lot_counts(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_lot_edit_rederives_the_canonical_column() {
        let conn = db();
        let id = sucrose(&conn);
        let lot = one_kg_lot(&conn, id);
        let edit = NewLot {
            ingredient_id: id,
            supplier: Some("Metro".into()),
            lot_code: None,
            entry: Quantity::new(2.5, Unit::Kg).unwrap(),
            cost: Some(Money::new(1250, Currency::EUR)),
            purchased_at: None,
            expires_at: None,
            notes: None,
        };
        update_lot(&conn, lot, &edit).unwrap();

        let row = &list_lots_for(&conn, id, false).unwrap()[0];
        assert_eq!(row.qty, CanonicalQty::Mass(Grams::new_unchecked(2500.0)));
        assert_eq!(row.entry, Quantity::new(2.5, Unit::Kg).unwrap());
        assert_eq!(row.cost, Some(Money::new(1250, Currency::EUR)));
        assert_eq!(row.supplier.as_deref(), Some("Metro"));

        let err = update_lot(
            &conn,
            lot,
            &NewLot {
                entry: Quantity::new(1.0, Unit::L).unwrap(),
                ..edit.clone()
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "units.dimension_mismatch");
        let err = update_lot(
            &conn,
            lot,
            &NewLot {
                entry: Quantity::new(0.0, Unit::Kg).unwrap(),
                ..edit.clone()
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "units.not_positive");

        let dextrose =
            create_ingredient(&conn, &ingredient("Dextrose", Category::Sugar, None)).unwrap();
        let err = update_lot(
            &conn,
            lot,
            &NewLot {
                ingredient_id: dextrose,
                ..edit.clone()
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.lot_ingredient_changed");
        let err = update_lot(&conn, 404, &edit).unwrap_err();
        assert_eq!(err.code(), "db.no_such_lot");
    }

    #[test]
    fn an_archived_lot_leaves_the_list_and_comes_back() {
        let conn = db();
        let id = sucrose(&conn);
        let lot = one_kg_lot(&conn, id);
        one_kg_lot(&conn, id);
        assert_eq!(active_lot_counts(&conn).unwrap()[&id], 2);

        assert!(archive_lot(&conn, lot, Timestamp::from_secs(5)).unwrap());
        assert_eq!(list_lots_for(&conn, id, false).unwrap().len(), 1);
        assert_eq!(list_lots_for(&conn, id, true).unwrap().len(), 2);
        assert_eq!(active_lot_counts(&conn).unwrap()[&id], 1);

        assert!(restore_lot(&conn, lot).unwrap());
        assert_eq!(list_lots_for(&conn, id, false).unwrap().len(), 2);
    }

    #[test]
    fn the_nitrogen_yield_is_merged_into_attrs_keeping_other_keys() {
        let merged = attrs_with_yan(Some(r#"{"alpha": 5}"#), Some(212.0)).unwrap();
        assert_eq!(mg_yan_per_g(merged.as_deref()), Some(212.0));
        assert!(merged.as_deref().unwrap().contains(r#""alpha":5"#));

        let removed = attrs_with_yan(merged.as_deref(), None).unwrap();
        assert_eq!(removed.as_deref(), Some(r#"{"alpha":5}"#));
        // Nothing left is no attrs, not `{}`.
        assert_eq!(
            attrs_with_yan(Some(r#"{"mg_yan_per_g": 1}"#), None).unwrap(),
            None
        );
        assert_eq!(attrs_with_yan(None, None).unwrap(), None);

        assert_eq!(
            attrs_with_yan(None, Some(0.0)).unwrap_err().code(),
            "units.not_positive"
        );
        assert_eq!(
            attrs_with_yan(None, Some(f64::NAN)).unwrap_err().code(),
            "units.not_finite"
        );
        assert_eq!(
            attrs_with_yan(Some("[1]"), Some(1.0)).unwrap_err().code(),
            "db.invalid_attrs"
        );
    }

    #[test]
    fn the_nitrogen_yield_is_read_from_attrs_or_is_none() {
        assert_eq!(mg_yan_per_g(Some(r#"{"mg_yan_per_g": 212}"#)), Some(212.0));
        assert_eq!(mg_yan_per_g(Some(r#"{"mg_yan_per_g": 0}"#)), None);
        assert_eq!(mg_yan_per_g(Some(r#"{"mg_yan_per_g": "212"}"#)), None);
        assert_eq!(mg_yan_per_g(Some(r#"{"alpha": 5}"#)), None);
        assert_eq!(mg_yan_per_g(Some("not json")), None);
        assert_eq!(mg_yan_per_g(None), None);
    }

    /// The three enums have no `CHECK` behind them, so this is the whole guarantee.
    #[test]
    fn an_unknown_enum_string_in_the_database_is_an_error_not_a_panic() {
        let conn = db();
        conn.execute(
            "INSERT INTO ingredient (name, category, default_unit) VALUES ('x', 'beer', 'kg')",
            [],
        )
        .unwrap();
        let err = list_ingredients(&conn, true).expect_err("unknown category");
        assert_eq!(err.code(), "db.sqlite");
        assert!(err.to_string().contains("beer"), "unexpected: {err}");

        assert_eq!(Category::parse("sugar"), Some(Category::Sugar));
        assert_eq!(Category::parse("beer"), None);
        assert_eq!(VesselKind::parse("fermenter"), Some(VesselKind::Fermenter));
        assert_eq!(YeastForm::parse("dry"), Some(YeastForm::Dry));
    }

    #[test]
    fn a_water_profile_round_trips_with_most_ions_missing() {
        let conn = db();
        let profile = WaterProfile {
            name: "Valenciennes tap".into(),
            bicarbonate_ppm: Some(180.0),
            ph: Some(7.7),
            notes: Some("SUEZ, most ions not reported".into()),
            ..Default::default()
        };
        create_water_profile(&conn, &profile).unwrap();

        let stored = list_water_profiles(&conn).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].profile, profile);
    }
}
