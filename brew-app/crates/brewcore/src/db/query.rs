//! Phase 5c. Saved queries: a structured [`BatchFilter`] over 5a's metrics, stored as
//! JSON and never as SQL — stored SQL would couple every saved query to the schema
//! and break on the next migration.
//!
//! **Two passes.** A SQL prefilter on the columns that exist — type, status, recipe,
//! lot, archived, and the OG / FG / pitch ranges — decides which batches get 5a's
//! metrics computed at all. The rest is judged in Rust over [`BatchMetrics`], so strain
//! and ferment temperature mean exactly what they mean in the metrics table.
//!
//! **Unknown is not a mismatch.** A batch the filter can't judge — 18 °C asked for, no
//! temperature logged — goes to [`QueryResult::unevaluated`] with the reason instead of
//! being dropped: a result that quietly narrows reads as "only these did".
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};

use super::{
    analytics::{BatchMetrics, metrics_for_batches},
    batch::{BATCH_COLUMNS, BatchRow, BatchStatus, batch_from_row},
    recipe::{BeverageType, list_recipe_versions, list_recipes},
    reference::{Category, list_yeast_strains},
};
use crate::{
    Error, Result,
    calc::{
        batch_metrics::{Computed, Missing},
        stability::StabilityParams,
    },
    units::{Celsius, Sg, Timestamp},
};

/// The `"v"` every stored filter carries. Bump it when a field changes meaning or goes
/// away; an old build then refuses the new filter instead of half-reading it.
pub const FILTER_VERSION: u64 = 1;

/// Inclusive at both ends; `None` leaves that end open.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds<T> {
    pub min: Option<T>,
    pub max: Option<T>,
}

impl<T: PartialOrd + Copy> Bounds<T> {
    pub fn contains(&self, v: T) -> bool {
        self.min.is_none_or(|min| v >= min) && self.max.is_none_or(|max| v <= max)
    }
}

/// Every criterion is ANDed; an empty list or `None` is "any". Canonical units — SG,
/// °C, epoch seconds — so a change of display unit never changes what a saved query
/// means.
///
/// `deny_unknown_fields`, and the lists carry no `default`: a field this build doesn't
/// know, or one a caller forgot, is refused rather than read as "any".
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchFilter {
    pub beverage_types: Vec<BeverageType>,
    pub statuses: Vec<BatchStatus>,
    pub include_archived: bool,
    pub recipe_id: Option<i64>,
    pub recipe_version_id: Option<i64>,
    /// Any of these, from the additions (5a's rule): a mixed-strain batch matches
    /// each of its strains.
    pub strain_ids: Vec<i64>,
    /// Any of these — "which batches used that suspect yeast lot".
    pub lot_ids: Vec<i64>,
    /// The recorded `batch.og` / `batch.fg`.
    pub og: Option<Bounds<Sg>>,
    pub fg: Option<Bounds<Sg>>,
    /// Against 5a's median, the number the metrics table shows.
    pub ferm_temp_c: Option<Bounds<Celsius>>,
    pub pitched: Option<Bounds<Timestamp>>,
}

impl BatchFilter {
    pub fn validate(&self) -> Result<()> {
        fn ordered<T: PartialOrd + Copy>(what: &str, bounds: Option<Bounds<T>>) -> Result<()> {
            match bounds {
                Some(Bounds {
                    min: Some(min),
                    max: Some(max),
                }) if min > max => Err(Error::InvalidFilter {
                    reason: format!("{what}: min is above max"),
                }),
                _ => Ok(()),
            }
        }
        ordered("og", self.og)?;
        ordered("fg", self.fg)?;
        ordered("ferm_temp_c", self.ferm_temp_c)?;
        ordered("pitched", self.pitched)
    }
}

/// The stored form: the filter's fields with `"v"` beside them.
pub fn filter_to_json(filter: &BatchFilter) -> Result<String> {
    let invalid = |e: serde_json::Error| Error::InvalidFilter {
        reason: e.to_string(),
    };
    let mut value = serde_json::to_value(filter).map_err(invalid)?;
    if let Some(fields) = value.as_object_mut() {
        fields.insert("v".into(), FILTER_VERSION.into());
    }
    serde_json::to_string(&value).map_err(invalid)
}

/// The version is checked before any field is read, so a newer filter is reported
/// as newer rather than as whichever of its fields fails first.
pub fn filter_from_json(json: &str) -> Result<BatchFilter> {
    let invalid = |e: serde_json::Error| Error::InvalidFilter {
        reason: e.to_string(),
    };
    let mut value: serde_json::Value = serde_json::from_str(json).map_err(invalid)?;
    let found = value.as_object_mut().and_then(|fields| fields.remove("v"));
    if found.as_ref().and_then(serde_json::Value::as_u64) != Some(FILTER_VERSION) {
        return Err(Error::FilterVersion {
            found: found.map_or_else(|| "none".into(), |v| v.to_string()),
        });
    }
    serde_json::from_value(value).map_err(invalid)
}

// ------------------------------------------------------------------ run

/// The criteria judged over 5a's metrics, each of which a batch may be unable to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Criterion {
    Og,
    Fg,
    Pitched,
    Strain,
    FermTemp,
}

/// A batch no criterion ruled out, but one couldn't be answered for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Unevaluated {
    pub batch_id: i64,
    pub code: String,
    /// The first criterion that couldn't be answered.
    pub criterion: Criterion,
    pub reason: Missing,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QueryResult {
    /// 5a's table for every match, in batch-code order.
    pub rows: Vec<BatchMetrics>,
    pub unevaluated: Vec<Unevaluated>,
}

pub fn run_query(
    conn: &Connection,
    filter: &BatchFilter,
    params: &StabilityParams,
) -> Result<QueryResult> {
    filter.validate()?;
    let mut result = QueryResult {
        rows: Vec::new(),
        unevaluated: Vec::new(),
    };
    for metrics in metrics_for_batches(conn, &prefilter(conn, filter)?, params)? {
        match judge(filter, &metrics) {
            Verdict::Match => result.rows.push(metrics),
            Verdict::NoMatch => {}
            Verdict::Unknown(criterion, reason) => result.unevaluated.push(Unevaluated {
                batch_id: metrics.batch_id,
                code: metrics.code,
                criterion,
                reason,
            }),
        }
    }
    Ok(result)
}

/// The columns that exist, as SQL. A NULL passes the range clauses: [`judge`] lists
/// that batch as unevaluated instead of it vanishing here.
fn prefilter(conn: &Connection, filter: &BatchFilter) -> Result<Vec<BatchRow>> {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<SqlValue> = Vec::new();

    if !filter.include_archived {
        clauses.push("archived_at IS NULL".into());
    }
    any_of(
        &mut clauses,
        &mut args,
        "beverage_type",
        filter
            .beverage_types
            .iter()
            .map(|t| SqlValue::Text(t.as_str().into())),
    );
    any_of(
        &mut clauses,
        &mut args,
        "status",
        filter
            .statuses
            .iter()
            .map(|s| SqlValue::Text(s.as_str().into())),
    );
    if let Some(id) = filter.recipe_id {
        clauses.push(
            "recipe_version_id IN (SELECT id FROM recipe_version WHERE recipe_id = ?)".into(),
        );
        args.push(id.into());
    }
    if let Some(id) = filter.recipe_version_id {
        clauses.push("recipe_version_id = ?".into());
        args.push(id.into());
    }
    if !filter.lot_ids.is_empty() {
        clauses.push(format!(
            "batch.id IN (SELECT batch_id FROM batch_ingredient WHERE ingredient_lot_id IN ({}))",
            marks(filter.lot_ids.len())
        ));
        args.extend(filter.lot_ids.iter().map(|&id| SqlValue::from(id)));
    }
    let sg = |v: Sg| SqlValue::Real(v.get());
    let ts = |t: Timestamp| SqlValue::Integer(t.as_secs());
    if let Some(b) = filter.og {
        null_or_within(&mut clauses, &mut args, "og", b.min.map(sg), b.max.map(sg));
    }
    if let Some(b) = filter.fg {
        null_or_within(&mut clauses, &mut args, "fg", b.min.map(sg), b.max.map(sg));
    }
    if let Some(b) = filter.pitched {
        null_or_within(
            &mut clauses,
            &mut args,
            "pitched_at",
            b.min.map(ts),
            b.max.map(ts),
        );
    }

    let filtered = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {BATCH_COLUMNS} FROM batch{filtered} ORDER BY code"
    ))?;
    let rows = stmt.query_map(params_from_iter(args), batch_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(", ")
}

fn any_of(
    clauses: &mut Vec<String>,
    args: &mut Vec<SqlValue>,
    column: &str,
    values: impl ExactSizeIterator<Item = SqlValue>,
) {
    if values.len() > 0 {
        clauses.push(format!("{column} IN ({})", marks(values.len())));
        args.extend(values);
    }
}

fn null_or_within(
    clauses: &mut Vec<String>,
    args: &mut Vec<SqlValue>,
    column: &str,
    min: Option<SqlValue>,
    max: Option<SqlValue>,
) {
    for (op, bound) in [(">=", min), ("<=", max)] {
        if let Some(bound) = bound {
            clauses.push(format!("({column} IS NULL OR {column} {op} ?)"));
            args.push(bound);
        }
    }
}

enum Verdict {
    Match,
    NoMatch,
    Unknown(Criterion, Missing),
}

/// A definite miss on any criterion wins over an unknown on another: a batch of the
/// wrong strain isn't a candidate, whatever its temperature was.
fn judge(filter: &BatchFilter, m: &BatchMetrics) -> Verdict {
    let known = |v: Option<bool>, missing| v.ok_or(missing);
    let checks = [
        filter.og.map(|b| {
            (
                Criterion::Og,
                known(m.og.map(|v| b.contains(v)), Missing::NoOg),
            )
        }),
        filter.fg.map(|b| {
            (
                Criterion::Fg,
                known(m.fg.map(|v| b.contains(v)), Missing::NoFg),
            )
        }),
        filter.pitched.map(|b| {
            let within = m.pitched_at.map(|v| b.contains(v));
            (Criterion::Pitched, known(within, Missing::NoPitch))
        }),
        (!filter.strain_ids.is_empty()).then(|| {
            let any = (!m.strains.is_empty())
                .then(|| m.strains.iter().any(|s| filter.strain_ids.contains(&s.id)));
            (Criterion::Strain, known(any, Missing::NoYeast))
        }),
        filter.ferm_temp_c.map(|b| {
            let within = match &m.ferm_temp {
                Computed::Value(t) => Ok(b.contains(Celsius::new_unchecked(t.spread.median))),
                Computed::Missing(reason) => Err(*reason),
            };
            (Criterion::FermTemp, within)
        }),
    ];

    let mut unknown = None;
    for (criterion, outcome) in checks.into_iter().flatten() {
        match outcome {
            Ok(true) => {}
            Ok(false) => return Verdict::NoMatch,
            Err(reason) => {
                unknown.get_or_insert((criterion, reason));
            }
        }
    }
    unknown.map_or(Verdict::Match, |(c, r)| Verdict::Unknown(c, r))
}

// ---------------------------------------------------------- saved_query

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SavedQuerySummary {
    pub id: i64,
    pub name: String,
    pub created_at: Timestamp,
    pub archived_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SavedQuery {
    #[serde(flatten)]
    pub summary: SavedQuerySummary,
    pub filter: BatchFilter,
}

fn checked_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::SavedQueryNameEmpty);
    }
    Ok(name)
}

/// The partial unique index on active names is the only constraint that can fail.
fn name_taken(name: &str) -> impl FnOnce(rusqlite::Error) -> Error + '_ {
    move |e| match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::SavedQueryNameTaken { name: name.into() }
        }
        e => e.into(),
    }
}

pub fn create_saved_query(
    conn: &Connection,
    name: &str,
    filter: &BatchFilter,
    at: Timestamp,
) -> Result<i64> {
    let name = checked_name(name)?;
    filter.validate()?;
    conn.execute(
        "INSERT INTO saved_query (name, filter, created_at) VALUES (?, ?, ?)",
        params![name, filter_to_json(filter)?, at],
    )
    .map_err(name_taken(name))?;
    Ok(conn.last_insert_rowid())
}

/// Replaces the name and filter in place. Nothing references a saved query, so there
/// is no history to keep — unlike a recipe version.
pub fn update_saved_query(
    conn: &Connection,
    id: i64,
    name: &str,
    filter: &BatchFilter,
) -> Result<()> {
    let name = checked_name(name)?;
    filter.validate()?;
    let changed = conn
        .execute(
            "UPDATE saved_query SET name = ?, filter = ? WHERE id = ?",
            params![name, filter_to_json(filter)?, id],
        )
        .map_err(name_taken(name))?;
    if changed == 0 {
        return Err(Error::NoSuchSavedQuery { id });
    }
    Ok(())
}

/// Returns whether a row changed.
pub fn archive_saved_query(conn: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE saved_query SET archived_at = ? WHERE id = ? AND archived_at IS NULL",
        params![at, id],
    )?;
    Ok(changed > 0)
}

pub fn get_saved_query(conn: &Connection, id: i64) -> Result<SavedQuery> {
    let (summary, json) = conn
        .query_row(
            "SELECT id, name, created_at, archived_at, filter FROM saved_query WHERE id = ?",
            [id],
            |row| {
                Ok((
                    SavedQuerySummary {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        created_at: row.get(2)?,
                        archived_at: row.get(3)?,
                    },
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(Error::NoSuchSavedQuery { id })?;
    Ok(SavedQuery {
        summary,
        filter: filter_from_json(&json)?,
    })
}

/// Names only, no filters: one unreadable filter fails when that query is opened, not
/// the whole list.
pub fn list_saved_queries(
    conn: &Connection,
    include_archived: bool,
) -> Result<Vec<SavedQuerySummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at, archived_at FROM saved_query \
         WHERE ?1 OR archived_at IS NULL ORDER BY name, id",
    )?;
    let rows = stmt.query_map([include_archived], |row| {
        Ok(SavedQuerySummary {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            archived_at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// -------------------------------------------------------------- options

/// What the filter form offers.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QueryOptions {
    pub recipes: Vec<RecipeOption>,
    pub strains: Vec<StrainOption>,
    pub lots: Vec<LotOption>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RecipeOption {
    pub id: i64,
    pub name: String,
    pub beverage_type: BeverageType,
    pub archived: bool,
    pub versions: Vec<VersionOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct VersionOption {
    pub id: i64,
    pub version_no: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StrainOption {
    pub id: i64,
    pub name: String,
    pub lab: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LotOption {
    pub id: i64,
    pub ingredient: String,
    pub category: Category,
    pub lot_code: Option<String>,
    pub supplier: Option<String>,
    pub purchased_at: Option<Timestamp>,
    pub batch_count: i64,
}

/// Recipes by their locked versions only, and lots only once some batch used them: a
/// batch points only at a locked version, so any other choice has no answer to give.
pub fn query_options(conn: &Connection) -> Result<QueryOptions> {
    let mut recipes = Vec::new();
    for recipe in list_recipes(conn, true)? {
        let versions: Vec<_> = list_recipe_versions(conn, recipe.id)?
            .into_iter()
            .filter(|v| v.is_locked())
            .map(|v| VersionOption {
                id: v.id,
                version_no: v.version_no,
            })
            .collect();
        if !versions.is_empty() {
            recipes.push(RecipeOption {
                id: recipe.id,
                name: recipe.name,
                beverage_type: recipe.beverage_type,
                archived: recipe.archived_at.is_some(),
                versions,
            });
        }
    }

    let strains = list_yeast_strains(conn)?
        .into_iter()
        .map(|s| StrainOption {
            id: s.id,
            name: s.name,
            lab: s.lab,
        })
        .collect();

    let mut stmt = conn.prepare(
        "SELECT l.id, i.name, i.category, l.lot_code, l.supplier, l.purchased_at, \
                COUNT(DISTINCT bi.batch_id) \
           FROM ingredient_lot l \
           JOIN ingredient i ON i.id = l.ingredient_id \
           JOIN batch_ingredient bi ON bi.ingredient_lot_id = l.id \
          GROUP BY l.id \
          ORDER BY i.category, i.name, l.purchased_at, l.id",
    )?;
    let lots = stmt
        .query_map([], |row| {
            Ok(LotOption {
                id: row.get(0)?,
                ingredient: row.get(1)?,
                category: row.get(2)?,
                lot_code: row.get(3)?,
                supplier: row.get(4)?,
                purchased_at: row.get(5)?,
                batch_count: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(QueryOptions {
        recipes,
        strains,
        lots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{
            Metric, SourceKind,
            batch::{
                NewBatch, NewBatchIngredient, add_batch_ingredient, archive_batch, create_batch,
                set_pitched_at, transition_batch,
            },
            recipe::{NewRecipeVersion, Stage, create_recipe, create_recipe_version},
            reference::{
                NewIngredient, NewLot, NewYeastStrain, create_ingredient, create_lot,
                create_yeast_strain,
            },
            tier1::{create_source, create_stream, insert_readings},
            timeline::bind_stream,
        },
        units::{Litres, Quantity, Unit},
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

    fn run(conn: &Connection, filter: &BatchFilter) -> QueryResult {
        run_query(conn, filter, &StabilityParams::default()).unwrap()
    }

    fn codes(result: &QueryResult) -> Vec<String> {
        result.rows.iter().map(|m| m.code.clone()).collect()
    }

    fn batch(conn: &Connection, code: &str, kind: BeverageType, og: Option<f64>) -> i64 {
        batch_of(conn, code, kind, og, None)
    }

    fn batch_of(
        conn: &Connection,
        code: &str,
        kind: BeverageType,
        og: Option<f64>,
        version: Option<i64>,
    ) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: version,
                beverage_type: kind,
                vessel_id: None,
                volume_l: Some(Litres::new(20.0).unwrap()),
                og: og.map(sg),
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    /// A strain, its ingredient, and one lot of it.
    fn yeast(conn: &Connection, name: &str) -> (i64, i64, i64) {
        let strain = create_yeast_strain(
            conn,
            &NewYeastStrain {
                name: name.into(),
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
        (strain, ingredient, lot(conn, ingredient))
    }

    fn lot(conn: &Connection, ingredient: i64) -> i64 {
        create_lot(
            conn,
            &NewLot {
                ingredient_id: ingredient,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(500.0, Unit::G).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap()
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

    /// A sensor stream of `metric` bound from `T0`: `f(hours)` every `interval` for 14 days.
    fn stream(
        conn: &mut Connection,
        batch_id: i64,
        metric: Metric,
        interval: i64,
        f: impl Fn(f64) -> f64,
    ) {
        let name = format!("{batch_id}-{metric}");
        let source_id = create_source(conn, SourceKind::BrewdJournal, &name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, &name, metric).unwrap();
        let rows: Vec<_> = (0..14 * DAY)
            .step_by(interval as usize)
            .map(|t| (stream_id, at(t), f(t as f64 / HOUR as f64)))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        bind_stream(conn, batch_id, stream_id, T0, None).unwrap();
    }

    /// Pitched at `T0` with an iSpindle curve falling from `og` and, given a
    /// temperature, a vessel probe holding it.
    fn fermented(
        conn: &mut Connection,
        code: &str,
        kind: BeverageType,
        og: f64,
        lot: i64,
        temp_c: Option<f64>,
    ) -> i64 {
        let id = batch(conn, code, kind, Some(og));
        set_pitched_at(conn, id, T0).unwrap();
        pitch(conn, id, lot);
        let offset = gravity_at(0.0) - og;
        stream(conn, id, Metric::Sg, 15 * 60, |h| gravity_at(h) - offset);
        if let Some(temp_c) = temp_c {
            stream(conn, id, Metric::TempC, HOUR, |_| temp_c);
        }
        id
    }

    fn within<T>(min: T, max: T) -> Option<Bounds<T>> {
        Some(Bounds {
            min: Some(min),
            max: Some(max),
        })
    }

    /// ARCHITECTURE.md's question, and 5c's acceptance test: "all EC-1118 seltzers,
    /// OG 1.035–1.040, fermented at 18 °C".
    #[test]
    fn ec1118_seltzers_at_18_c() {
        let mut conn = db();
        let (ec1118, _, ec) = yeast(&conn, "EC-1118");
        let (_, _, us05) = yeast(&conn, "US-05");
        let seltzer = BeverageType::Seltzer;
        let beer = BeverageType::Beer;
        fermented(&mut conn, "SELTZER-001", seltzer, 1.037, ec, Some(18.0));
        fermented(&mut conn, "SELTZER-002", seltzer, 1.038, ec, Some(18.2));
        fermented(&mut conn, "SELTZER-003", seltzer, 1.037, ec, Some(22.0));
        fermented(&mut conn, "SELTZER-004", seltzer, 1.045, ec, Some(18.0));
        fermented(&mut conn, "SELTZER-005", seltzer, 1.037, us05, Some(18.0));
        fermented(&mut conn, "BEER-006", beer, 1.037, ec, Some(18.0));
        let unlogged = fermented(&mut conn, "SELTZER-007", seltzer, 1.037, ec, None);

        let got = run(
            &conn,
            &BatchFilter {
                beverage_types: vec![seltzer],
                include_archived: true,
                strain_ids: vec![ec1118],
                og: within(sg(1.035), sg(1.040)),
                ferm_temp_c: within(Celsius::new_unchecked(17.5), Celsius::new_unchecked(18.5)),
                ..Default::default()
            },
        );

        assert_eq!(codes(&got), ["SELTZER-001", "SELTZER-002"]);
        for m in &got.rows {
            assert!(matches!(m.lag, Computed::Value(_)), "{:?}", m.lag);
            assert!(
                matches!(m.peak_rate, Computed::Value(_)),
                "{:?}",
                m.peak_rate
            );
            assert!(matches!(m.terminal, Computed::Value(_)), "{:?}", m.terminal);
        }
        assert_eq!(
            got.unevaluated,
            [Unevaluated {
                batch_id: unlogged,
                code: "SELTZER-007".into(),
                criterion: Criterion::FermTemp,
                reason: Missing::NoTemp,
            }]
        );
    }

    /// 3a's traceability question: which batches used that lot, and only that lot.
    #[test]
    fn a_lot_finds_the_batches_that_used_it() {
        let conn = db();
        let (_, ingredient, first) = yeast(&conn, "EC-1118");
        let second = lot(&conn, ingredient);
        let a = batch(&conn, "SELTZER-001", BeverageType::Seltzer, None);
        let b = batch(&conn, "SELTZER-002", BeverageType::Seltzer, None);
        batch(&conn, "SELTZER-003", BeverageType::Seltzer, None);
        pitch(&conn, a, first);
        pitch(&conn, a, second);
        pitch(&conn, b, second);

        let by_lot = |lot_ids: Vec<i64>| {
            codes(&run(
                &conn,
                &BatchFilter {
                    lot_ids,
                    ..Default::default()
                },
            ))
        };
        assert_eq!(by_lot(vec![first]), ["SELTZER-001"]);
        assert_eq!(by_lot(vec![second]), ["SELTZER-001", "SELTZER-002"]);

        let options = query_options(&conn).unwrap();
        let counts: Vec<_> = options.lots.iter().map(|l| (l.id, l.batch_count)).collect();
        assert_eq!(counts, [(first, 1), (second, 2)]);
    }

    #[test]
    fn the_columns_filter_in_sql() {
        let conn = db();
        let recipe = create_recipe(&conn, "Citron-Verveine", BeverageType::Seltzer, None).unwrap();
        let v1 = create_recipe_version(&conn, recipe, &NewRecipeVersion::default()).unwrap();
        let v2 = create_recipe_version(&conn, recipe, &NewRecipeVersion::default()).unwrap();
        let seltzer = BeverageType::Seltzer;
        let a = batch_of(&conn, "SELTZER-001", seltzer, Some(1.040), Some(v1));
        let b = batch_of(&conn, "SELTZER-002", seltzer, Some(1.050), Some(v2));
        batch(&conn, "CIDER-003", BeverageType::Cider, Some(1.055));
        let archived = batch(&conn, "SELTZER-004", seltzer, Some(1.040));
        archive_batch(&conn, archived, at(DAY)).unwrap();
        set_pitched_at(&conn, a, at(DAY)).unwrap();
        transition_batch(&conn, a, BatchStatus::Fermenting, at(DAY)).unwrap();
        set_pitched_at(&conn, b, at(10 * DAY)).unwrap();

        let q = |filter: BatchFilter| codes(&run(&conn, &filter));
        assert_eq!(
            q(BatchFilter::default()),
            ["CIDER-003", "SELTZER-001", "SELTZER-002"]
        );
        assert_eq!(
            q(BatchFilter {
                include_archived: true,
                beverage_types: vec![seltzer],
                ..Default::default()
            }),
            ["SELTZER-001", "SELTZER-002", "SELTZER-004"]
        );
        assert_eq!(
            q(BatchFilter {
                statuses: vec![BatchStatus::Fermenting],
                ..Default::default()
            }),
            ["SELTZER-001"]
        );
        assert_eq!(
            q(BatchFilter {
                recipe_id: Some(recipe),
                ..Default::default()
            }),
            ["SELTZER-001", "SELTZER-002"]
        );
        assert_eq!(
            q(BatchFilter {
                recipe_version_id: Some(v2),
                ..Default::default()
            }),
            ["SELTZER-002"]
        );
        assert_eq!(
            q(BatchFilter {
                og: Some(Bounds {
                    min: Some(sg(1.045)),
                    max: None,
                }),
                ..Default::default()
            }),
            ["CIDER-003", "SELTZER-002"]
        );
        assert_eq!(
            q(BatchFilter {
                pitched: within(T0, at(2 * DAY)),
                beverage_types: vec![seltzer],
                ..Default::default()
            }),
            ["SELTZER-001"]
        );
    }

    /// Asked about something the batch doesn't record: listed with the reason, neither
    /// dropped nor matched.
    #[test]
    fn an_unanswerable_criterion_is_listed_not_dropped() {
        let conn = db();
        let (ec1118, _, ec) = yeast(&conn, "EC-1118");
        let (_, _, us05) = yeast(&conn, "US-05");
        let no_og = batch(&conn, "SELTZER-001", BeverageType::Seltzer, None);
        let no_yeast = batch(&conn, "SELTZER-002", BeverageType::Seltzer, Some(1.040));
        let wrong_yeast = batch(&conn, "SELTZER-003", BeverageType::Seltzer, None);
        let mixed = batch(&conn, "SELTZER-004", BeverageType::Seltzer, Some(1.040));
        pitch(&conn, no_og, ec);
        pitch(&conn, wrong_yeast, us05);
        pitch(&conn, mixed, ec);
        pitch(&conn, mixed, us05);

        let got = run(
            &conn,
            &BatchFilter {
                strain_ids: vec![ec1118],
                og: within(sg(1.035), sg(1.045)),
                ..Default::default()
            },
        );
        // A mixed batch matches each of its strains.
        assert_eq!(codes(&got), ["SELTZER-004"]);
        // The wrong strain rules SELTZER-003 out whatever its OG; the others can't say.
        let why: Vec<_> = got
            .unevaluated
            .iter()
            .map(|u| (u.batch_id, u.criterion, u.reason))
            .collect();
        assert_eq!(
            why,
            [
                (no_og, Criterion::Og, Missing::NoOg),
                (no_yeast, Criterion::Strain, Missing::NoYeast),
            ]
        );

        let unpitched = run(
            &conn,
            &BatchFilter {
                pitched: within(T0, at(DAY)),
                ..Default::default()
            },
        );
        assert!(unpitched.rows.is_empty());
        assert_eq!(unpitched.unevaluated.len(), 4);
        assert!(
            unpitched
                .unevaluated
                .iter()
                .all(|u| u.reason == Missing::NoPitch)
        );
    }

    #[test]
    fn an_upside_down_range_is_refused() {
        let conn = db();
        let filter = BatchFilter {
            fg: within(sg(1.010), sg(1.000)),
            ..Default::default()
        };
        let err = run_query(&conn, &filter, &StabilityParams::default()).unwrap_err();
        assert!(matches!(err, Error::InvalidFilter { .. }), "{err}");
    }

    #[test]
    fn a_filter_round_trips_with_its_version() {
        let filter = BatchFilter {
            beverage_types: vec![BeverageType::Seltzer],
            statuses: vec![BatchStatus::Packaged],
            include_archived: true,
            strain_ids: vec![3],
            og: within(sg(1.035), sg(1.040)),
            ferm_temp_c: Some(Bounds {
                min: None,
                max: Some(Celsius::new_unchecked(18.5)),
            }),
            ..Default::default()
        };
        let json = filter_to_json(&filter).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["og"]["min"], 1.035);
        assert_eq!(filter_from_json(&json).unwrap(), filter);
    }

    /// Dropping what it doesn't know would widen the result, so it refuses.
    #[test]
    fn an_unreadable_filter_is_refused_not_widened() {
        let mut value: serde_json::Value =
            serde_json::from_str(&filter_to_json(&BatchFilter::default()).unwrap()).unwrap();
        let without = |v: &serde_json::Value, key: &str| {
            let mut v = v.clone();
            v.as_object_mut().unwrap().remove(key);
            v.to_string()
        };

        let mut extra = value.clone();
        extra["min_abv"] = 0.05.into();
        let err = filter_from_json(&extra.to_string()).unwrap_err();
        assert!(matches!(err, Error::InvalidFilter { .. }), "{err}");
        assert!(err.to_string().contains("min_abv"), "{err}");

        let err = filter_from_json(&without(&value, "strain_ids")).unwrap_err();
        assert!(err.to_string().contains("strain_ids"), "{err}");

        let err = filter_from_json(&without(&value, "v")).unwrap_err();
        assert!(
            matches!(err, Error::FilterVersion { ref found } if found == "none"),
            "{err}"
        );

        value["v"] = 2.into();
        let err = filter_from_json(&value.to_string()).unwrap_err();
        assert!(
            matches!(err, Error::FilterVersion { ref found } if found == "2"),
            "{err}"
        );
    }

    #[test]
    fn saved_queries_are_named_updated_and_archived() {
        let conn = db();
        let filter = BatchFilter {
            beverage_types: vec![BeverageType::Seltzer],
            ..Default::default()
        };
        let id = create_saved_query(&conn, "  EC-1118 seltzers ", &filter, T0).unwrap();
        let got = get_saved_query(&conn, id).unwrap();
        assert_eq!(got.summary.name, "EC-1118 seltzers");
        assert_eq!(got.summary.created_at, T0);
        assert_eq!(got.filter, filter);

        let err = create_saved_query(&conn, "EC-1118 seltzers", &filter, T0).unwrap_err();
        assert!(matches!(err, Error::SavedQueryNameTaken { .. }), "{err}");
        let err = create_saved_query(&conn, "  ", &filter, T0).unwrap_err();
        assert!(matches!(err, Error::SavedQueryNameEmpty), "{err}");

        let wider = BatchFilter::default();
        update_saved_query(&conn, id, "All batches", &wider).unwrap();
        assert_eq!(get_saved_query(&conn, id).unwrap().filter, wider);
        let err = update_saved_query(&conn, 404, "x", &wider).unwrap_err();
        assert!(matches!(err, Error::NoSuchSavedQuery { id: 404 }), "{err}");

        assert!(archive_saved_query(&conn, id, at(DAY)).unwrap());
        assert!(!archive_saved_query(&conn, id, at(DAY)).unwrap());
        assert!(list_saved_queries(&conn, false).unwrap().is_empty());
        // An archived name is free again.
        let again = create_saved_query(&conn, "All batches", &wider, at(DAY)).unwrap();
        let listed: Vec<_> = list_saved_queries(&conn, true)
            .unwrap()
            .into_iter()
            .map(|s| (s.id, s.archived_at.is_some()))
            .collect();
        assert_eq!(listed, [(id, true), (again, false)]);
    }

    #[test]
    fn a_newer_stored_filter_fails_on_open_not_on_list() {
        let conn = db();
        let id = create_saved_query(&conn, "q", &BatchFilter::default(), T0).unwrap();
        conn.execute(
            "UPDATE saved_query SET filter = json_set(filter, '$.v', 2) WHERE id = ?",
            [id],
        )
        .unwrap();
        assert_eq!(list_saved_queries(&conn, false).unwrap().len(), 1);
        let err = get_saved_query(&conn, id).unwrap_err();
        assert_eq!(err.code(), "query.filter_version");
    }
}
