use std::{collections::HashMap, path::PathBuf};

use brewcore::{
    Error,
    brewd::{self, JournalProbe},
    calc::{
        abv,
        gravity::{display_to_sg, sg_to_display},
        stability::{ExpectedFg, FlatSpan, Stability, flat_spans},
        yan,
    },
    config::{Config, ConfigStore},
    db::{
        Db, DbStatus, Metric, SourceKind,
        analytics::BatchMetrics,
        batch::{
            self, BatchCost, BatchEdit, BatchIngredientRow, BatchRow, BatchStatus, BatchSummary,
            GravityField, NewBatch, NewBatchIngredient, PlanVsActualRow,
        },
        compare::{self, Comparison, RelRange, YAxis, YMode},
        dashboard::{self, BatchDashboard, DisplayGravity},
        health::DeviceHealth,
        home::{self, HomeBatch},
        measurement::{
            self, MeasurementRow, NewMeasurement, QuickEntry, QuickEntryResult, TimelineItem,
        },
        query::{
            self, BatchFilter, QueryOptions, QueryResult, SavedQuery, SavedQuerySummary,
            Unevaluated,
        },
        recipe::{
            self, BeverageType, ItemChange, NewRecipeItem, NewRecipeVersion, RecipeItemRow,
            RecipeRow, Stage,
        },
        reference::{
            self, Category, IngredientRow, LotRow, NewIngredient, NewLot, NewYeastStrain,
            VesselKind, VesselRow, YeastStrainRow,
        },
        rollup::{self, Rollup},
        series::{self, BatchSeries, SeriesRange},
        stability,
        tier1::{self, StreamRow},
        timeline::{self, BatchEventRow, EventKind, NewBatchEvent},
    },
    export::{self, QueryTable},
    fermentrack::{self, Client, LogProbe, SourceSummary, discovery},
    journal::JournalEntry,
    sync::{ReadingSource, SourceStatus, SyncManager},
    units::{Currency, GravityDisplay, Litres, Money, Quantity, Sg, Timestamp, Unit},
};

use crate::error::{ApiError, ApiResult};

#[tauri::command]
pub async fn get_config(config: tauri::State<'_, ConfigStore>) -> ApiResult<Config> {
    Ok(config.get())
}

/// Whole-document replace, not a patch. The settings pane has four fields and already
/// holds the `Config` it got from [`get_config`]; a patch type of nested `Option`s
/// would be more code than the thing it configures. Last-write-wins is fine with one
/// window and no background writer — revisit if Phase 6's rule engine starts writing
/// thresholds.
///
/// Returns the *stored* value, which may differ from the argument (a trimmed URL, a
/// clamped interval). The frontend adopts the return value, so normalisation is
/// visible without a second round trip.
///
/// The write is one small `sync_all`'d file, so it runs inline rather than through
/// `spawn_blocking`.
#[tauri::command]
pub async fn set_config(
    config_store: tauri::State<'_, ConfigStore>,
    config: Config,
) -> ApiResult<Config> {
    Ok(config_store.set(config)?)
}

/// Async so Tauri runs it off the main thread: `rusqlite` is blocking, and even a
/// cheap status query should not stall the webview. Phase 2's sync path will want
/// `spawn_blocking` for the same reason.
#[tauri::command]
pub async fn db_status(db: tauri::State<'_, Db>) -> ApiResult<DbStatus> {
    Ok(db.status()?)
}

// ---------------------------------------------------------------------------
// Phase 2f: sources
// ---------------------------------------------------------------------------

/// Validate a pasted log URL without writing anything.
///
/// `input` is a whole URL or a path relative to the configured base. The error is the
/// point of this command as much as the success: `fermentrack.graph_log` and
/// `fermentrack.gravity_log` are two different mistakes and the pane says so.
#[tauri::command]
pub async fn probe_log(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    input: String,
) -> ApiResult<LogProbe> {
    let base_url = config.get().fermentrack.base_url;
    Ok(discovery::probe_log(&client, &base_url, &input).await?)
}

/// Validate and then create the source, in that order.
///
/// Deliberately re-probes rather than taking a `LogProbe` from the frontend: a write
/// must not trust state the client has been holding, and the round trip costs 2 KB.
#[tauri::command]
pub async fn add_source(
    db: tauri::State<'_, Db>,
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    manager: tauri::State<'_, SyncManager>,
    input: String,
    name: Option<String>,
) -> ApiResult<SourceSummary> {
    let base_url = config.get().fermentrack.base_url;
    let probe = discovery::probe_log(&client, &base_url, &input).await?;

    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| probe.suggested_name.clone());

    // Two small statements in one transaction; `spawn_blocking` would cost more than
    // it saves, for the same reason `db_status` runs inline.
    let id = discovery::add_source(&db, &probe, &name)?;

    // Phase 2g: poll it now rather than at the next launch. Without this, pasting a URL
    // produces a row and no readings until the app is restarted, which reads as
    // "adding a source does nothing".
    manager.spawn(Box::new(fermentrack::build_source(&db, &client, id)?));
    summary(&db, id)
}

fn summary(db: &Db, id: i64) -> ApiResult<SourceSummary> {
    discovery::source_summary(db, id)?.ok_or_else(|| ApiError {
        code: "sync.missing_state".to_owned(),
        message: format!("source {id} vanished immediately after being created"),
        detail: None,
    })
}

/// The poller for a source, by kind. `None` for a kind nothing polls: manual entry,
/// and Fermentrack gravity logs, which nothing reads yet.
pub(crate) fn build_source(
    db: &Db,
    client: &Client,
    config: &ConfigStore,
    id: i64,
    kind: SourceKind,
) -> brewcore::Result<Option<Box<dyn ReadingSource>>> {
    Ok(match kind {
        SourceKind::FermentrackBeerLog => {
            Some(Box::new(fermentrack::build_source(db, client, id)?))
        }
        SourceKind::BrewdJournal => Some(Box::new(brewd::build_source(db, client, config, id)?)),
        SourceKind::FermentrackGravityLog | SourceKind::Manual => None,
    })
}

#[tauri::command]
pub async fn list_sources(db: tauri::State<'_, Db>) -> ApiResult<Vec<SourceSummary>> {
    Ok(discovery::source_summaries(&db)?)
}

/// One call to `GET /api/devices/`, purely to tell "wrong host or port" from "right
/// box, wrong path". It returns device ids and nothing else — no beers, no names —
/// which is why the log URL still has to be pasted.
#[tauri::command]
pub async fn check_fermentrack(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    base_url: Option<String>,
) -> ApiResult<Vec<i64>> {
    // The pane's unsaved field wins when it has one, so "Test" tests what is on screen
    // rather than what was last saved.
    let base_url = base_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| config.get().fermentrack.base_url);
    Ok(discovery::check_fermentrack(&client, &base_url).await?)
}

// ---------------------------------------------------------------------------
// Phase Ve: brewd journals
// ---------------------------------------------------------------------------

/// Every journal `brewd` has. Doubles as the settings pane's "Test connection": only
/// `brewd` answers `/journals/` with a journal list.
#[tauri::command]
pub async fn list_journals(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    base_url: Option<String>,
) -> ApiResult<Vec<JournalEntry>> {
    // As `check_fermentrack`: the unsaved field wins, so "Test" tests what is on screen.
    let base_url = base_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| config.get().brewd.base_url);
    let base_url = brewd::discovery::base_url(&base_url)?;
    Ok(brewd::list_journals(&client, base_url).await?)
}

#[tauri::command]
pub async fn probe_journal(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    journal: String,
) -> ApiResult<JournalProbe> {
    let base_url = config.get().brewd.base_url;
    let base_url = brewd::discovery::base_url(&base_url)?;
    Ok(brewd::probe_journal(&client, base_url, &journal).await?)
}

/// Probe, create, start polling. Re-probes for the reason [`add_source`] does.
#[tauri::command]
pub async fn add_journal_source(
    db: tauri::State<'_, Db>,
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    manager: tauri::State<'_, SyncManager>,
    journal: String,
    name: Option<String>,
) -> ApiResult<SourceSummary> {
    let base_url = config.get().brewd.base_url;
    let base_url = brewd::discovery::base_url(&base_url)?;
    let probe = brewd::probe_journal(&client, base_url, &journal).await?;

    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| probe.suggested_name.clone());
    let id = brewd::add_source(&db, &probe, &name)?;
    manager.spawn(Box::new(brewd::build_source(&db, &client, &config, id)?));
    summary(&db, id)
}

// ---------------------------------------------------------------------------
// Phase 2g: sync
// ---------------------------------------------------------------------------

/// Poll this source now instead of at its next tick.
///
/// Returns as soon as the request is queued. The result arrives as a `sync://status`
/// event like every other transition — holding a command open across a week of backlog
/// would freeze the button that started it.
#[tauri::command]
pub async fn resync(manager: tauri::State<'_, SyncManager>, source_id: i64) -> ApiResult<()> {
    Ok(manager.resync(source_id)?)
}

/// Delete every reading for this source and derive them all again from byte zero.
///
/// Destructive to Tier 1 only, which is derived data — that is why it can be a button
/// at all. The confirmation lives in the UI; by the time this is called the user has
/// already been told the row count.
#[tauri::command]
pub async fn rebuild(manager: tauri::State<'_, SyncManager>, source_id: i64) -> ApiResult<()> {
    Ok(manager.rebuild(source_id)?)
}

/// Every source's current sync state, for first paint.
///
/// A window can open mid-poll, before any event has been emitted into it; without this
/// the pane would render "never synced" for a source that is working fine.
#[tauri::command]
pub async fn sync_status(manager: tauri::State<'_, SyncManager>) -> ApiResult<Vec<SourceStatus>> {
    Ok(manager.statuses())
}

// ---------------------------------------------------------------------------
// Phase Ve: vessels and their default streams
// ---------------------------------------------------------------------------

/// A vessel with the streams a pitch into it binds.
#[derive(Debug, serde::Serialize)]
pub struct VesselView {
    #[serde(flatten)]
    pub vessel: VesselRow,
    pub default_streams: Vec<i64>,
}

#[tauri::command]
pub async fn list_vessels(db: tauri::State<'_, Db>) -> ApiResult<Vec<VesselView>> {
    Ok(db.read(|conn| {
        reference::list_vessels(conn, false)?
            .into_iter()
            .map(|vessel| {
                let default_streams = reference::list_vessel_streams(conn, vessel.id)?;
                Ok(VesselView {
                    vessel,
                    default_streams,
                })
            })
            .collect()
    })?)
}

#[tauri::command]
pub async fn create_vessel(
    db: tauri::State<'_, Db>,
    name: String,
    kind: VesselKind,
    capacity_l: Option<f64>,
) -> ApiResult<i64> {
    let capacity_l = capacity_l.map(Litres::new).transpose()?;
    Ok(db.write(|conn| reference::create_vessel(conn, name.trim(), kind, capacity_l, None))?)
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U1: ingredients, yeast strains, lots
// ---------------------------------------------------------------------------

/// An ingredient as the list shows it: the strain's label and the YAN figure read out
/// here, so the frontend never parses `attrs`.
#[derive(Debug, serde::Serialize)]
pub struct IngredientView {
    #[serde(flatten)]
    pub ingredient: IngredientRow,
    pub yeast_strain: Option<String>,
    pub mg_yan_per_g: Option<f64>,
    /// Active lots only: what is in stock, not what ever was.
    pub lot_count: i64,
}

#[tauri::command]
pub async fn list_ingredients(
    db: tauri::State<'_, Db>,
    include_archived: bool,
) -> ApiResult<Vec<IngredientView>> {
    Ok(db.read(|conn| {
        let strains: HashMap<i64, String> = reference::list_yeast_strains(conn, true)?
            .into_iter()
            .map(|s| {
                let label = match s.lab {
                    Some(lab) => format!("{} ({lab})", s.name),
                    None => s.name,
                };
                (s.id, label)
            })
            .collect();
        let lots = reference::active_lot_counts(conn)?;
        Ok(reference::list_ingredients(conn, include_archived)?
            .into_iter()
            .map(|ingredient| IngredientView {
                yeast_strain: ingredient
                    .yeast_strain_id
                    .and_then(|id| strains.get(&id).cloned()),
                mg_yan_per_g: ingredient.mg_yan_per_g(),
                lot_count: lots.get(&ingredient.id).copied().unwrap_or(0),
                ingredient,
            })
            .collect())
    })?)
}

/// What the ingredient form sends. No `attrs`: the YAN figure is merged into the
/// stored JSON in Rust, keeping any other key.
#[derive(Debug, serde::Deserialize)]
pub struct IngredientInput {
    pub name: String,
    pub category: Category,
    pub default_unit: Unit,
    pub yeast_strain_id: Option<i64>,
    pub mg_yan_per_g: Option<f64>,
    pub notes: Option<String>,
}

impl IngredientInput {
    fn into_new(self, attrs: Option<&str>) -> brewcore::Result<NewIngredient> {
        Ok(NewIngredient {
            attrs: reference::attrs_with_yan(attrs, self.mg_yan_per_g)?,
            name: self.name,
            category: self.category,
            default_unit: self.default_unit,
            yeast_strain_id: self.yeast_strain_id,
            notes: self.notes,
        })
    }
}

#[tauri::command]
pub async fn create_ingredient(
    db: tauri::State<'_, Db>,
    ingredient: IngredientInput,
) -> ApiResult<i64> {
    let ingredient = ingredient.into_new(None)?;
    Ok(db.write(|conn| reference::create_ingredient(conn, &ingredient))?)
}

/// Read-merge-write in one transaction, so a key someone else put in `attrs` survives.
#[tauri::command]
pub async fn update_ingredient(
    db: tauri::State<'_, Db>,
    id: i64,
    ingredient: IngredientInput,
) -> ApiResult<()> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let current = reference::get_ingredient(&tx, id)?;
        let ingredient = ingredient.into_new(current.attrs.as_deref())?;
        reference::update_ingredient(&tx, id, &ingredient)?;
        tx.commit()?;
        Ok(())
    })?)
}

#[tauri::command]
pub async fn archive_ingredient(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| reference::archive_ingredient(conn, id, Timestamp::now()))?)
}

#[tauri::command]
pub async fn restore_ingredient(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| reference::restore_ingredient(conn, id))?)
}

#[derive(Debug, serde::Serialize)]
pub struct YanPreset {
    pub label: &'static str,
    pub mg_yan_per_g: f64,
}

/// `calc::yan`'s figures, so `api.ts` carries no copy of the numbers.
#[tauri::command]
pub async fn yan_presets() -> ApiResult<Vec<YanPreset>> {
    Ok(yan::PRESETS
        .iter()
        .map(|&(label, mg_yan_per_g)| YanPreset {
            label,
            mg_yan_per_g,
        })
        .collect())
}

#[tauri::command]
pub async fn list_yeast_strains(
    db: tauri::State<'_, Db>,
    include_archived: bool,
) -> ApiResult<Vec<YeastStrainRow>> {
    Ok(db.read(|conn| reference::list_yeast_strains(conn, include_archived))?)
}

#[tauri::command]
pub async fn create_yeast_strain(
    db: tauri::State<'_, Db>,
    strain: NewYeastStrain,
) -> ApiResult<i64> {
    Ok(db.write(|conn| reference::create_yeast_strain(conn, &strain))?)
}

#[tauri::command]
pub async fn update_yeast_strain(
    db: tauri::State<'_, Db>,
    id: i64,
    strain: NewYeastStrain,
) -> ApiResult<()> {
    Ok(db.write(|conn| reference::update_yeast_strain(conn, id, &strain))?)
}

#[tauri::command]
pub async fn archive_yeast_strain(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| reference::archive_yeast_strain(conn, id, Timestamp::now()))?)
}

#[tauri::command]
pub async fn restore_yeast_strain(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| reference::restore_yeast_strain(conn, id))?)
}

/// A lot with what the list derives from it.
#[derive(Debug, serde::Serialize)]
pub struct LotView {
    #[serde(flatten)]
    pub lot: LotRow,
    /// Minor units per gram / litre / unit — a float, see `LotRow::cost_per_canonical_unit`.
    pub cost_per_canonical_unit: Option<f64>,
    /// Distinct batches that drew from it: the count that links to 5c's lot filter.
    pub batch_count: i64,
}

#[tauri::command]
pub async fn list_lots(
    db: tauri::State<'_, Db>,
    ingredient_id: i64,
    include_archived: bool,
) -> ApiResult<Vec<LotView>> {
    Ok(db.read(|conn| {
        let used = reference::lot_batch_counts(conn, ingredient_id)?;
        Ok(
            reference::list_lots_for(conn, ingredient_id, include_archived)?
                .into_iter()
                .map(|lot| LotView {
                    cost_per_canonical_unit: lot.cost_per_canonical_unit(),
                    batch_count: used.get(&lot.id).copied().unwrap_or(0),
                    lot,
                })
                .collect(),
        )
    })?)
}

/// What the lot form sends. The cost arrives as typed (`12,50`) and is parsed by
/// `Money::parse`, the one parser with the tests; an empty one is no cost.
#[derive(Debug, serde::Deserialize)]
pub struct LotInput {
    pub ingredient_id: i64,
    pub supplier: Option<String>,
    pub lot_code: Option<String>,
    pub entry: Quantity,
    pub cost: Option<String>,
    pub currency: Option<String>,
    pub purchased_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    pub notes: Option<String>,
}

impl LotInput {
    fn into_new(self) -> brewcore::Result<NewLot> {
        let cost = match self.cost.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => {
                let currency = Currency::new(self.currency.as_deref().unwrap_or(""))?;
                Some(Money::parse(text, currency)?)
            }
        };
        Ok(NewLot {
            ingredient_id: self.ingredient_id,
            supplier: self.supplier,
            lot_code: self.lot_code,
            entry: self.entry,
            cost,
            purchased_at: self.purchased_at,
            expires_at: self.expires_at,
            notes: self.notes,
        })
    }
}

#[tauri::command]
pub async fn create_lot(db: tauri::State<'_, Db>, lot: LotInput) -> ApiResult<i64> {
    let lot = lot.into_new()?;
    Ok(db.write(|conn| reference::create_lot(conn, &lot))?)
}

#[tauri::command]
pub async fn update_lot(db: tauri::State<'_, Db>, id: i64, lot: LotInput) -> ApiResult<()> {
    let lot = lot.into_new()?;
    Ok(db.write(|conn| reference::update_lot(conn, id, &lot))?)
}

#[tauri::command]
pub async fn archive_lot(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| reference::archive_lot(conn, id, Timestamp::now()))?)
}

#[tauri::command]
pub async fn restore_lot(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| reference::restore_lot(conn, id))?)
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U2: recipes
// ---------------------------------------------------------------------------
//
// Target gravities cross this boundary in `ui.gravity_display`, converted here by
// `calc::gravity` — 4d's rule: `api.ts` has no copy of the fits.

#[derive(Debug, serde::Serialize)]
pub struct RecipeSummary {
    #[serde(flatten)]
    pub recipe: RecipeRow,
    pub version_count: i64,
    pub latest_version_no: Option<i64>,
}

#[tauri::command]
pub async fn list_recipes(
    db: tauri::State<'_, Db>,
    include_archived: bool,
) -> ApiResult<Vec<RecipeSummary>> {
    Ok(db.read(|conn| {
        recipe::list_recipes(conn, include_archived)?
            .into_iter()
            .map(|recipe| {
                let versions = recipe::list_recipe_versions(conn, recipe.id)?;
                Ok(RecipeSummary {
                    version_count: versions.len() as i64,
                    latest_version_no: versions.last().map(|v| v.version_no),
                    recipe,
                })
            })
            .collect()
    })?)
}

#[derive(Debug, serde::Deserialize)]
pub struct RecipeInput {
    pub name: String,
    pub beverage_type: BeverageType,
    pub notes: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CreatedRecipe {
    pub recipe_id: i64,
    pub version_id: i64,
}

/// The recipe and an empty v1 together: a recipe with no version has nothing to edit.
#[tauri::command]
pub async fn create_recipe(
    db: tauri::State<'_, Db>,
    recipe: RecipeInput,
) -> ApiResult<CreatedRecipe> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let recipe_id = recipe::create_recipe(
            &tx,
            &recipe.name,
            recipe.beverage_type,
            recipe.notes.as_deref(),
        )?;
        let version_id =
            recipe::create_recipe_version(&tx, recipe_id, &NewRecipeVersion::default())?;
        tx.commit()?;
        Ok(CreatedRecipe {
            recipe_id,
            version_id,
        })
    })?)
}

#[tauri::command]
pub async fn update_recipe(
    db: tauri::State<'_, Db>,
    id: i64,
    recipe: RecipeInput,
) -> ApiResult<()> {
    Ok(db.write(|conn| {
        recipe::update_recipe(
            conn,
            id,
            &recipe.name,
            recipe.beverage_type,
            recipe.notes.as_deref(),
        )
    })?)
}

#[tauri::command]
pub async fn archive_recipe(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| recipe::archive_recipe(conn, id, Timestamp::now()))?)
}

#[tauri::command]
pub async fn restore_recipe(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| recipe::restore_recipe(conn, id))?)
}

/// One tab per version: enough to number it, badge it and route to it.
#[derive(Debug, serde::Serialize)]
pub struct VersionTab {
    pub id: i64,
    pub version_no: i64,
    pub locked_at: Option<Timestamp>,
    pub batch_count: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct RecipeDetail {
    pub recipe: RecipeRow,
    pub versions: Vec<VersionTab>,
}

#[tauri::command]
pub async fn get_recipe(db: tauri::State<'_, Db>, id: i64) -> ApiResult<RecipeDetail> {
    Ok(db.read(|conn| {
        let recipe = recipe::get_recipe(conn, id)?;
        let used = recipe::version_batch_counts(conn, id)?;
        let versions = recipe::list_recipe_versions(conn, id)?
            .into_iter()
            .map(|v| VersionTab {
                batch_count: used.get(&v.id).copied().unwrap_or(0),
                id: v.id,
                version_no: v.version_no,
                locked_at: v.locked_at,
            })
            .collect();
        Ok(RecipeDetail { recipe, versions })
    })?)
}

#[derive(Debug, serde::Serialize)]
pub struct RecipeItemView {
    #[serde(flatten)]
    pub item: RecipeItemRow,
    pub ingredient: String,
    /// Still shown on the line it is already on; gone from the picker.
    pub ingredient_archived: bool,
}

/// A version in the display gravity unit, with its items named.
#[derive(Debug, serde::Serialize)]
pub struct RecipeVersionView {
    pub id: i64,
    pub recipe_id: i64,
    pub version_no: i64,
    pub locked_at: Option<Timestamp>,
    pub batch_count: i64,
    pub target_volume_l: Option<Litres>,
    pub target_og: Option<f64>,
    pub target_fg: Option<f64>,
    /// A fraction, as stored.
    pub target_abv: Option<f64>,
    /// From the target OG and FG, by 3f's `alternate` — labelled as such on screen.
    pub abv_alternate: Option<f64>,
    pub notes: Option<String>,
    pub gravity_unit: &'static str,
    pub items: Vec<RecipeItemView>,
}

fn item_view(item: RecipeItemRow, names: &HashMap<i64, (String, bool)>) -> RecipeItemView {
    let (ingredient, ingredient_archived) = names
        .get(&item.ingredient_id)
        .cloned()
        .unwrap_or_else(|| (format!("#{}", item.ingredient_id), false));
    RecipeItemView {
        item,
        ingredient,
        ingredient_archived,
    }
}

#[tauri::command]
pub async fn get_recipe_version(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    id: i64,
) -> ApiResult<RecipeVersionView> {
    let display = config.get().ui.gravity_display;
    Ok(db.read(|conn| {
        let v = recipe::get_recipe_version(conn, id)?;
        let batch_count = recipe::version_batch_counts(conn, v.recipe_id)?
            .get(&id)
            .copied()
            .unwrap_or(0);
        let names = reference::ingredient_names(conn)?;
        let items = recipe::list_recipe_items(conn, id)?
            .into_iter()
            .map(|item| item_view(item, &names))
            .collect();
        let abv_alternate = match (v.target_og, v.target_fg) {
            (Some(og), Some(fg)) => abv::alternate(og, fg).ok(),
            _ => None,
        };
        Ok(RecipeVersionView {
            id: v.id,
            recipe_id: v.recipe_id,
            version_no: v.version_no,
            locked_at: v.locked_at,
            batch_count,
            target_volume_l: v.target_volume_l,
            target_og: v.target_og.map(|sg| sg_to_display(sg, display)),
            target_fg: v.target_fg.map(|sg| sg_to_display(sg, display)),
            target_abv: v.target_abv,
            abv_alternate,
            notes: v.notes,
            gravity_unit: display.unit(),
            items,
        })
    })?)
}

/// What the targets form sends: gravities in the display unit, ABV as a fraction.
#[derive(Debug, serde::Deserialize)]
pub struct RecipeVersionInput {
    pub target_volume_l: Option<f64>,
    pub target_og: Option<f64>,
    pub target_fg: Option<f64>,
    pub target_abv: Option<f64>,
    pub notes: Option<String>,
}

impl RecipeVersionInput {
    fn into_new(self, display: GravityDisplay) -> brewcore::Result<NewRecipeVersion> {
        let sg = |v: Option<f64>| v.map(|v| display_to_sg(v, display)).transpose();
        Ok(NewRecipeVersion {
            target_volume_l: self.target_volume_l.map(Litres::new).transpose()?,
            target_og: sg(self.target_og)?,
            target_fg: sg(self.target_fg)?,
            target_abv: self.target_abv,
            notes: self.notes,
        })
    }
}

#[tauri::command]
pub async fn update_recipe_version(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    id: i64,
    version: RecipeVersionInput,
) -> ApiResult<()> {
    let version = version.into_new(config.get().ui.gravity_display)?;
    Ok(db.write(|conn| recipe::update_recipe_version(conn, id, &version))?)
}

/// The fork and its items in one transaction. Returns the new version's tab, so the
/// screen can route to `v{n}`.
#[tauri::command]
pub async fn fork_recipe_version(db: tauri::State<'_, Db>, id: i64) -> ApiResult<VersionTab> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let new_id = recipe::fork_recipe_version(&tx, id)?;
        let v = recipe::get_recipe_version(&tx, new_id)?;
        tx.commit()?;
        Ok(VersionTab {
            id: v.id,
            version_no: v.version_no,
            locked_at: v.locked_at,
            batch_count: 0,
        })
    })?)
}

#[tauri::command]
pub async fn delete_recipe_version(db: tauri::State<'_, Db>, id: i64) -> ApiResult<()> {
    Ok(db.write(|conn| recipe::delete_recipe_version(conn, id))?)
}

#[tauri::command]
pub async fn add_recipe_item(
    db: tauri::State<'_, Db>,
    version_id: i64,
    item: NewRecipeItem,
) -> ApiResult<i64> {
    Ok(db.write(|conn| recipe::add_recipe_item(conn, version_id, &item))?)
}

#[tauri::command]
pub async fn update_recipe_item(
    db: tauri::State<'_, Db>,
    id: i64,
    item: NewRecipeItem,
) -> ApiResult<()> {
    Ok(db.write(|conn| recipe::update_recipe_item(conn, id, &item))?)
}

#[tauri::command]
pub async fn delete_recipe_item(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| recipe::delete_recipe_item(conn, id))?)
}

#[derive(Debug, serde::Serialize)]
pub struct TargetChange {
    pub field: &'static str,
    pub from: Option<f64>,
    pub to: Option<f64>,
}

/// One row of a diff. `from` and `to` are `None` on the side the item is missing from.
#[derive(Debug, serde::Serialize)]
pub struct ItemChangeView {
    pub change: &'static str,
    pub ingredient: String,
    pub stage: Stage,
    pub from: Option<RecipeItemRow>,
    pub to: Option<RecipeItemRow>,
}

#[derive(Debug, serde::Serialize)]
pub struct RecipeDiffView {
    pub from_version_no: i64,
    pub to_version_no: i64,
    pub gravity_unit: &'static str,
    /// Only the targets that differ. Gravities in the display unit, ABV a fraction.
    pub targets: Vec<TargetChange>,
    pub items: Vec<ItemChangeView>,
}

#[tauri::command]
pub async fn diff_recipe_versions(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    from_id: i64,
    to_id: i64,
) -> ApiResult<RecipeDiffView> {
    let display = config.get().ui.gravity_display;
    Ok(db.read(|conn| {
        let diff = recipe::diff_recipe_versions(conn, from_id, to_id)?;
        let names = reference::ingredient_names(conn)?;
        let name = |id: i64| {
            names
                .get(&id)
                .map_or_else(|| format!("#{id}"), |(n, _)| n.clone())
        };
        let gravity = |pair: (Option<Sg>, Option<Sg>)| {
            let to_display = |v: Option<Sg>| v.map(|sg| sg_to_display(sg, display));
            (to_display(pair.0), to_display(pair.1))
        };

        let mut targets = Vec::new();
        let mut push = |field, (from, to): (Option<f64>, Option<f64>)| {
            targets.push(TargetChange { field, from, to });
        };
        if let Some((a, b)) = diff.target_volume_l {
            push("target_volume_l", (a.map(Litres::get), b.map(Litres::get)));
        }
        if let Some(pair) = diff.target_og {
            push("target_og", gravity(pair));
        }
        if let Some(pair) = diff.target_fg {
            push("target_fg", gravity(pair));
        }
        if let Some(pair) = diff.target_abv {
            push("target_abv", pair);
        }

        let items = diff
            .items
            .into_iter()
            .map(|change| {
                let (change, from, to) = match change {
                    ItemChange::Added(to) => ("added", None, Some(to)),
                    ItemChange::Removed(from) => ("removed", Some(from), None),
                    ItemChange::Changed { from, to } => ("changed", Some(from), Some(to)),
                };
                let any = from.as_ref().or(to.as_ref()).expect("one side is set");
                ItemChangeView {
                    change,
                    ingredient: name(any.ingredient_id),
                    stage: any.stage,
                    from,
                    to,
                }
            })
            .collect();

        Ok(RecipeDiffView {
            from_version_no: diff.from.version_no,
            to_version_no: diff.to.version_no,
            gravity_unit: display.unit(),
            targets,
            items,
        })
    })?)
}

#[tauri::command]
pub async fn list_streams(db: tauri::State<'_, Db>) -> ApiResult<Vec<StreamRow>> {
    Ok(db.read(tier1::list_streams)?)
}

/// Replace a vessel's default streams. Existing bindings are untouched; the next pitch
/// is what changes.
#[tauri::command]
pub async fn set_vessel_streams(
    db: tauri::State<'_, Db>,
    vessel_id: i64,
    stream_ids: Vec<i64>,
) -> ApiResult<()> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        reference::set_vessel_streams(&tx, vessel_id, &stream_ids)?;
        tx.commit()?;
        Ok(())
    })?)
}

// ---------------------------------------------------------------------------
// Phase 4d: the batch curve
// ---------------------------------------------------------------------------

/// What the curve draws. `battery_v`, `rssi_dbm`, `tilt_deg` and `state` are device
/// health, not the ferment, and stay off it.
const CURVE_METRICS: [Metric; 7] = [
    Metric::Sg,
    Metric::TempC,
    Metric::FridgeTempC,
    Metric::RoomTempC,
    Metric::GravityTempC,
    Metric::SetpointC,
    Metric::Ph,
];

#[tauri::command]
pub async fn list_batches(
    db: tauri::State<'_, Db>,
    include_archived: bool,
) -> ApiResult<Vec<BatchSummary>> {
    Ok(db.read(|conn| batch::list_batch_summaries(conn, include_archived))?)
}

#[tauri::command]
pub async fn get_batch(db: tauri::State<'_, Db>, batch_id: i64) -> ApiResult<BatchRow> {
    Ok(db.read(|conn| batch::get_batch(conn, batch_id))?)
}

#[derive(Debug, serde::Serialize)]
pub struct CurveView {
    #[serde(flatten)]
    pub series: BatchSeries,
    /// The gravity axis label: `ui.gravity_display` is applied here, since the fits
    /// live in `calc` and the frontend has no copy of them.
    pub gravity_unit: &'static str,
}

#[tauri::command]
pub async fn batch_series(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
    range: Option<SeriesRange>,
    max_points: usize,
) -> ApiResult<CurveView> {
    let display = config.get().ui.gravity_display;
    let series = db.read(|conn| {
        series::batch_series(conn, batch_id, &CURVE_METRICS, range, max_points)?
            .in_gravity_display(display)
    })?;
    Ok(CurveView {
        series,
        gravity_unit: display.unit(),
    })
}

#[tauri::command]
pub async fn list_batch_events(
    db: tauri::State<'_, Db>,
    batch_id: i64,
) -> ApiResult<Vec<BatchEventRow>> {
    Ok(db.read(|conn| timeline::list_batch_events(conn, batch_id))?)
}

/// 4c's detector with its history collapsed to spans: the history is one entry per
/// raw reading since pitch, which is the payload 4a exists to avoid.
#[derive(Debug, serde::Serialize)]
pub struct StabilityView {
    pub stream_id: Option<i64>,
    pub current: Stability,
    pub expected_fg: Option<ExpectedFg>,
    pub spans: Vec<FlatSpan>,
}

#[tauri::command]
pub async fn batch_stability(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
) -> ApiResult<StabilityView> {
    let params = config.get().stability.params();
    let s = db.read(|conn| stability::batch_stability(conn, batch_id, &params))?;
    Ok(StabilityView {
        stream_id: s.stream_id,
        current: s.current,
        expected_fg: s.expected_fg,
        spans: flat_spans(&s.history),
    })
}

// ---------------------------------------------------------------------------
// Phase 4e: the status card
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct DashboardView {
    #[serde(flatten)]
    pub dashboard: BatchDashboard,
    /// The card's gravities in `ui.gravity_display`; the SG ones stay beside them.
    pub display: DisplayGravity,
    pub gravity_unit: &'static str,
}

#[tauri::command]
pub async fn batch_dashboard(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
) -> ApiResult<DashboardView> {
    let config = config.get();
    let (params, health) = (config.stability.params(), config.health.params());
    let dashboard = db.read(|conn| dashboard::batch_dashboard(conn, batch_id, &params, &health))?;
    Ok(DashboardView {
        display: dashboard.gravity_in(config.ui.gravity_display),
        dashboard,
        gravity_unit: config.ui.gravity_display.unit(),
    })
}

/// 3e's "use this as the OG". SG, not the display unit: the card sends back the
/// candidate's canonical value. `None` clears it.
#[tauri::command]
pub async fn set_batch_og(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    og: Option<f64>,
) -> ApiResult<()> {
    let og = og.map(Sg::new).transpose()?;
    Ok(db.write(|conn| batch::set_batch_gravity(conn, batch_id, GravityField::Og, og))?)
}

/// U4's "use as FG", the same way round: SG in, `None` clears.
#[tauri::command]
pub async fn set_batch_fg(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    fg: Option<f64>,
) -> ApiResult<()> {
    let fg = fg.map(Sg::new).transpose()?;
    Ok(db.write(|conn| batch::set_batch_gravity(conn, batch_id, GravityField::Fg, fg))?)
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U3: batch create and lifecycle
// ---------------------------------------------------------------------------
//
// Times the user picks arrive as epoch seconds and default to now. A future one is
// refused here, since only the command knows what now is.

fn not_in_future(at: Option<Timestamp>, what: &'static str) -> brewcore::Result<Timestamp> {
    let now = Timestamp::now();
    match at {
        Some(at) if at > now => Err(Error::InFuture { what }),
        Some(at) => Ok(at),
        None => Ok(now),
    }
}

#[tauri::command]
pub async fn suggest_batch_code(
    db: tauri::State<'_, Db>,
    beverage_type: BeverageType,
) -> ApiResult<String> {
    Ok(db.read(|conn| batch::suggest_batch_code(conn, beverage_type))?)
}

/// What the create form sends: OG in the display unit, like the recipe targets.
#[derive(Debug, serde::Deserialize)]
pub struct BatchInput {
    pub code: String,
    pub recipe_version_id: Option<i64>,
    pub beverage_type: BeverageType,
    pub vessel_id: Option<i64>,
    pub volume_l: Option<f64>,
    pub og: Option<f64>,
    pub notes: Option<String>,
}

impl BatchInput {
    fn into_new(self, display: GravityDisplay) -> brewcore::Result<NewBatch> {
        Ok(NewBatch {
            code: self.code,
            recipe_version_id: self.recipe_version_id,
            beverage_type: self.beverage_type,
            vessel_id: self.vessel_id,
            volume_l: self.volume_l.map(Litres::new).transpose()?,
            og: self.og.map(|v| display_to_sg(v, display)).transpose()?,
            notes: self.notes,
        })
    }
}

/// The batch and the lock on its recipe version, in one transaction.
#[tauri::command]
pub async fn create_batch(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch: BatchInput,
) -> ApiResult<i64> {
    let batch = batch.into_new(config.get().ui.gravity_display)?;
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let id = batch::create_batch(&tx, &batch, Timestamp::now())?;
        tx.commit()?;
        Ok(id)
    })?)
}

#[tauri::command]
pub async fn update_batch(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    batch: BatchEdit,
) -> ApiResult<()> {
    Ok(db.write(|conn| batch::update_batch(conn, batch_id, &batch))?)
}

#[tauri::command]
pub async fn archive_batch(db: tauri::State<'_, Db>, batch_id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let changed = batch::archive_batch(&tx, batch_id, Timestamp::now())?;
        tx.commit()?;
        Ok(changed)
    })?)
}

#[tauri::command]
pub async fn restore_batch(db: tauri::State<'_, Db>, batch_id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| batch::restore_batch(conn, batch_id))?)
}

/// The event, `pitched_at`, the move to fermenting and the vessel's sensor bindings
/// commit together or not at all.
#[tauri::command]
pub async fn record_pitch(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    at: Option<Timestamp>,
    note: Option<String>,
) -> ApiResult<i64> {
    let at = not_in_future(at, "pitch")?;
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let id = timeline::record_pitch(&tx, batch_id, at, note.as_deref())?;
        tx.commit()?;
        Ok(id)
    })?)
}

#[tauri::command]
pub async fn undo_pitch(db: tauri::State<'_, Db>, batch_id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let removed = timeline::undo_pitch(&tx, batch_id)?;
        tx.commit()?;
        Ok(removed)
    })?)
}

#[tauri::command]
pub async fn transition_batch(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    to: BatchStatus,
    at: Option<Timestamp>,
) -> ApiResult<bool> {
    let at = not_in_future(at, "status change")?;
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let moved = batch::transition_batch(&tx, batch_id, to, at)?;
        tx.commit()?;
        Ok(moved)
    })?)
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U4: timeline and quick entry
// ---------------------------------------------------------------------------
//
// Readings are typed in their instrument's own scale (`entry_unit`) and converted by
// `db::measurement`; gravities come back in `ui.gravity_display` beside the SG.

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimelineItemView {
    Event(BatchEventRow),
    Measurement {
        #[serde(flatten)]
        row: MeasurementRow,
        /// A gravity in `gravity_unit`; `None` for every other metric.
        display: Option<f64>,
    },
}

#[derive(Debug, serde::Serialize)]
pub struct TimelineView {
    pub pitched_at: Option<Timestamp>,
    pub gravity_unit: &'static str,
    pub items: Vec<TimelineItemView>,
}

#[tauri::command]
pub async fn batch_timeline(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
) -> ApiResult<TimelineView> {
    let display = config.get().ui.gravity_display;
    let (pitched_at, items) = db.read(|conn| {
        Ok((
            batch::get_batch(conn, batch_id)?.pitched_at,
            measurement::batch_timeline(conn, batch_id)?,
        ))
    })?;
    let items = items
        .into_iter()
        .map(|item| {
            Ok(match item {
                TimelineItem::Event(e) => TimelineItemView::Event(e),
                TimelineItem::Measurement(row) => TimelineItemView::Measurement {
                    display: (row.metric == Metric::Sg)
                        .then(|| Sg::new(row.value).map(|sg| sg_to_display(sg, display)))
                        .transpose()?,
                    row,
                },
            })
        })
        .collect::<brewcore::Result<_>>()?;
    Ok(TimelineView {
        pitched_at,
        gravity_unit: display.unit(),
        items,
    })
}

/// What the quick-entry form sends. `ts` defaults to now.
#[derive(Debug, serde::Deserialize)]
pub struct QuickEntryInput {
    pub ts: Option<Timestamp>,
    pub measurements: Vec<NewMeasurement>,
    pub note: Option<String>,
}

#[tauri::command]
pub async fn record_quick_entry(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    entry: QuickEntryInput,
) -> ApiResult<QuickEntryResult> {
    let entry = QuickEntry {
        ts: not_in_future(entry.ts, "reading")?,
        measurements: entry.measurements,
        note: entry.note,
    };
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let result = measurement::record_quick_entry(&tx, batch_id, &entry)?;
        tx.commit()?;
        Ok(result)
    })?)
}

#[tauri::command]
pub async fn delete_measurement(db: tauri::State<'_, Db>, measurement_id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        let removed = measurement::delete_measurement(&tx, measurement_id)?;
        tx.commit()?;
        Ok(removed)
    })?)
}

/// What the event form sends. `ts` defaults to now; the payload is not the form's.
#[derive(Debug, serde::Deserialize)]
pub struct EventInput {
    pub ts: Option<Timestamp>,
    pub kind: EventKind,
    pub note: Option<String>,
}

impl EventInput {
    fn into_new(self) -> brewcore::Result<NewBatchEvent> {
        Ok(NewBatchEvent {
            ts: not_in_future(self.ts, "event")?,
            kind: self.kind,
            payload: None,
            note: self.note,
        })
    }
}

#[tauri::command]
pub async fn add_event(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    event: EventInput,
) -> ApiResult<i64> {
    let event = event.into_new()?;
    Ok(db.write(|conn| timeline::add_event(conn, batch_id, &event))?)
}

/// Keeps an existing payload: an accepted suggestion's origin is not the form's to drop.
#[tauri::command]
pub async fn update_event(
    db: tauri::State<'_, Db>,
    event_id: i64,
    event: EventInput,
) -> ApiResult<()> {
    let mut event = event.into_new()?;
    Ok(db.write(|conn| {
        event.payload = timeline::get_event(conn, event_id)?.payload;
        timeline::update_event(conn, event_id, &event)
    })?)
}

#[tauri::command]
pub async fn delete_event(db: tauri::State<'_, Db>, event_id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| timeline::delete_event(conn, event_id))?)
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U5: actuals and cost
// ---------------------------------------------------------------------------

/// A lot as the additions picker shows it: which ingredient, and in what dimension.
#[derive(Debug, serde::Serialize)]
pub struct LotChoice {
    #[serde(flatten)]
    pub lot: LotRow,
    pub ingredient: String,
    pub category: Category,
    pub default_unit: Unit,
}

/// Active lots of active ingredients, by ingredient name.
#[tauri::command]
pub async fn list_lot_choices(db: tauri::State<'_, Db>) -> ApiResult<Vec<LotChoice>> {
    Ok(db.read(|conn| {
        let ingredients: HashMap<i64, IngredientRow> = reference::list_ingredients(conn, false)?
            .into_iter()
            .map(|i| (i.id, i))
            .collect();
        let mut lots: Vec<LotChoice> = reference::list_all_lots(conn, false)?
            .into_iter()
            .filter_map(|lot| {
                let i = ingredients.get(&lot.ingredient_id)?;
                Some(LotChoice {
                    ingredient: i.name.clone(),
                    category: i.category,
                    default_unit: i.default_unit,
                    lot,
                })
            })
            .collect();
        lots.sort_by(|a, b| a.ingredient.cmp(&b.ingredient));
        Ok(lots)
    })?)
}

/// An addition with the names and the money beside it.
#[derive(Debug, serde::Serialize)]
pub struct ActualView {
    #[serde(flatten)]
    pub row: BatchIngredientRow,
    pub ingredient: String,
    /// The lot code, else its supplier, else its number.
    pub lot_label: String,
    pub lot_archived: bool,
    /// What this addition cost, rounded once; `None` from an unpriced lot.
    pub line_cost: Option<Money>,
}

/// 3c's `PlanVsActualRow`, named. `delta_entry` is `delta` in the planned line's own
/// unit, so half a kilo short reads as 0.5 kg and not as 500 g.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PlanLineView {
    Missing {
        planned: RecipeItemRow,
        ingredient: String,
    },
    Unplanned {
        actual: ActualView,
        ingredient: String,
    },
    Both {
        planned: RecipeItemRow,
        actual: ActualView,
        ingredient: String,
        delta: Option<f64>,
        delta_entry: Option<f64>,
    },
}

#[derive(Debug, serde::Serialize)]
pub struct ActualsView {
    pub recipe_version_id: Option<i64>,
    pub lines: Vec<PlanLineView>,
    /// `None` with `cost_error` set when the lots are in more than one currency: the
    /// lines still show, the total does not exist.
    pub cost: Option<BatchCost>,
    pub cost_error: Option<ApiError>,
}

#[tauri::command]
pub async fn batch_actuals(db: tauri::State<'_, Db>, batch_id: i64) -> ApiResult<ActualsView> {
    Ok(db.read(|conn| {
        let plan = batch::plan_vs_actual(conn, batch_id)?;
        let names = reference::ingredient_names(conn)?;
        let name = |id: i64| names.get(&id).map(|(n, _)| n.clone()).unwrap_or_default();
        let lots: HashMap<i64, LotRow> = reference::list_all_lots(conn, true)?
            .into_iter()
            .map(|l| (l.id, l))
            .collect();
        let view = |row: BatchIngredientRow| {
            let lot = lots.get(&row.ingredient_lot_id);
            let line_cost = lot.and_then(|l| {
                let per_unit = l.cost_per_canonical_unit()?;
                let cost = l.cost?;
                Some(Money::new(
                    (per_unit * row.qty.amount()).round() as i64,
                    cost.currency,
                ))
            });
            ActualView {
                ingredient: name(row.ingredient_id),
                lot_label: lot
                    .and_then(|l| l.lot_code.clone().or_else(|| l.supplier.clone()))
                    .unwrap_or_else(|| format!("lot {}", row.ingredient_lot_id)),
                lot_archived: lot.is_some_and(|l| l.archived_at.is_some()),
                line_cost,
                row,
            }
        };
        let lines = plan
            .rows
            .into_iter()
            .map(|r| match r {
                PlanVsActualRow::Missing(planned) => PlanLineView::Missing {
                    ingredient: name(planned.ingredient_id),
                    planned,
                },
                PlanVsActualRow::Unplanned(actual) => PlanLineView::Unplanned {
                    ingredient: name(actual.ingredient_id),
                    actual: view(actual),
                },
                PlanVsActualRow::Both {
                    planned,
                    actual,
                    delta,
                } => PlanLineView::Both {
                    ingredient: name(planned.ingredient_id),
                    delta_entry: delta.map(|d| d / planned.entry.unit.factor()),
                    delta,
                    planned,
                    actual: view(actual),
                },
            })
            .collect();
        let (cost, cost_error) = match batch::batch_cost(conn, batch_id) {
            Ok(cost) => (Some(cost), None),
            Err(e @ Error::CurrencyMismatch { .. }) => (None, Some(e.into())),
            Err(e) => return Err(e),
        };
        Ok(ActualsView {
            recipe_version_id: plan.recipe_version_id,
            lines,
            cost,
            cost_error,
        })
    })?)
}

/// What the additions form sends. `added_at: None` is untimed, not now: "part of the
/// stage" is a real answer for a prep addition.
#[derive(Debug, serde::Deserialize)]
pub struct ActualInput {
    pub ingredient_lot_id: i64,
    pub entry: Quantity,
    pub stage: Stage,
    pub added_at: Option<Timestamp>,
    pub notes: Option<String>,
}

impl ActualInput {
    fn into_new(self) -> brewcore::Result<NewBatchIngredient> {
        Ok(NewBatchIngredient {
            added_at: self
                .added_at
                .map(|at| not_in_future(Some(at), "addition"))
                .transpose()?,
            ingredient_lot_id: self.ingredient_lot_id,
            entry: Quantity::new(self.entry.value, self.entry.unit)?,
            stage: self.stage,
            notes: self.notes,
        })
    }
}

#[tauri::command]
pub async fn add_batch_ingredient(
    db: tauri::State<'_, Db>,
    batch_id: i64,
    item: ActualInput,
) -> ApiResult<i64> {
    let item = item.into_new()?;
    Ok(db.write(|conn| batch::add_batch_ingredient(conn, batch_id, &item))?)
}

#[tauri::command]
pub async fn update_batch_ingredient(
    db: tauri::State<'_, Db>,
    id: i64,
    item: ActualInput,
) -> ApiResult<()> {
    let item = item.into_new()?;
    Ok(db.write(|conn| batch::update_batch_ingredient(conn, id, &item))?)
}

#[tauri::command]
pub async fn delete_batch_ingredient(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| batch::delete_batch_ingredient(conn, id))?)
}

// ---------------------------------------------------------------------------
// Phase 4f: home
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct HomeView {
    pub batches: Vec<HomeBatchView>,
    pub devices: Vec<DeviceHealth>,
    pub gravity_unit: &'static str,
}

#[derive(Debug, serde::Serialize)]
pub struct HomeBatchView {
    #[serde(flatten)]
    pub batch: HomeBatch,
    /// As [`DashboardView::display`].
    pub display: DisplayGravity,
}

#[tauri::command]
pub async fn home_summary(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
) -> ApiResult<HomeView> {
    let config = config.get();
    let (params, health) = (config.stability.params(), config.health.params());
    let summary = db.read(|conn| home::home_summary(conn, &params, &health))?;
    let display = config.ui.gravity_display;
    Ok(HomeView {
        batches: summary
            .batches
            .into_iter()
            .map(|batch| HomeBatchView {
                display: batch.dashboard.gravity_in(display),
                batch,
            })
            .collect(),
        devices: summary.devices,
        gravity_unit: display.unit(),
    })
}

// ---------------------------------------------------------------------------
// Phase 5b: the comparison overlay
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct CompareView {
    #[serde(flatten)]
    pub comparison: Comparison,
    /// The raw gravity axis's label. A fraction has no unit, so only batches left raw
    /// are converted.
    pub gravity_unit: &'static str,
}

#[tauri::command]
pub async fn compare_batches(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_ids: Vec<i64>,
    y_mode: YMode,
    with_temp: bool,
    range: Option<RelRange>,
    max_points: usize,
) -> ApiResult<CompareView> {
    let display = config.get().ui.gravity_display;
    let mut comparison = db.read(|conn| {
        compare::compare_batches(conn, &batch_ids, y_mode, with_temp, range, max_points)
    })?;
    for b in &mut comparison.batches {
        if let YAxis::Raw { .. } = b.y {
            for s in &mut b.gravity {
                s.in_gravity_display(display)?;
            }
        }
    }
    Ok(CompareView {
        comparison,
        gravity_unit: display.unit(),
    })
}

// ---------------------------------------------------------------------------
// Phase 5c: saved queries
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct QueryView {
    pub rows: Vec<QueryRowView>,
    pub unevaluated: Vec<Unevaluated>,
    /// 5d: the same rows grouped by strain, so the two views can't disagree.
    pub rollup: Rollup,
    pub gravity_unit: &'static str,
}

#[derive(Debug, serde::Serialize)]
pub struct QueryRowView {
    #[serde(flatten)]
    pub metrics: BatchMetrics,
    /// OG and FG in the display unit. The filter itself stays in SG.
    pub display_og: Option<f64>,
    pub display_fg: Option<f64>,
}

fn query_view(result: QueryResult, rollup: Rollup, display: GravityDisplay) -> QueryView {
    QueryView {
        rows: result
            .rows
            .into_iter()
            .map(|metrics| QueryRowView {
                display_og: metrics.og.map(|sg| sg_to_display(sg, display)),
                display_fg: metrics.fg.map(|sg| sg_to_display(sg, display)),
                metrics,
            })
            .collect(),
        unevaluated: result.unevaluated,
        rollup,
        gravity_unit: display.unit(),
    }
}

/// An unsaved filter: the form runs this on every change.
#[tauri::command]
pub async fn run_batch_query(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    filter: BatchFilter,
) -> ApiResult<QueryView> {
    let config = config.get();
    let params = config.stability.params();
    let (result, rollup) = db.read(|conn| {
        let result = query::run_query(conn, &filter, &params)?;
        let rollup = rollup::strain_rollup(conn, &result.rows)?;
        Ok((result, rollup))
    })?;
    Ok(query_view(result, rollup, config.ui.gravity_display))
}

#[derive(Debug, serde::Serialize)]
pub struct SavedQueryRun {
    pub query: SavedQuery,
    pub result: QueryView,
}

/// Opens and runs in one read, so the filter shown is the one that ran.
#[tauri::command]
pub async fn run_saved_query(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    id: i64,
) -> ApiResult<SavedQueryRun> {
    let config = config.get();
    let params = config.stability.params();
    let (query, result, rollup) = db.read(|conn| {
        let query = query::get_saved_query(conn, id)?;
        let result = query::run_query(conn, &query.filter, &params)?;
        let rollup = rollup::strain_rollup(conn, &result.rows)?;
        Ok((query, result, rollup))
    })?;
    Ok(SavedQueryRun {
        query,
        result: query_view(result, rollup, config.ui.gravity_display),
    })
}

#[tauri::command]
pub async fn list_saved_queries(
    db: tauri::State<'_, Db>,
    include_archived: bool,
) -> ApiResult<Vec<SavedQuerySummary>> {
    Ok(db.read(|conn| query::list_saved_queries(conn, include_archived))?)
}

#[tauri::command]
pub async fn create_saved_query(
    db: tauri::State<'_, Db>,
    name: String,
    filter: BatchFilter,
) -> ApiResult<SavedQuery> {
    Ok(db.write(|conn| {
        let id = query::create_saved_query(conn, &name, &filter, Timestamp::now())?;
        query::get_saved_query(conn, id)
    })?)
}

/// Rename and replace the filter in one write; the form always sends both.
#[tauri::command]
pub async fn update_saved_query(
    db: tauri::State<'_, Db>,
    id: i64,
    name: String,
    filter: BatchFilter,
) -> ApiResult<SavedQuery> {
    Ok(db.write(|conn| {
        query::update_saved_query(conn, id, &name, &filter)?;
        query::get_saved_query(conn, id)
    })?)
}

#[tauri::command]
pub async fn archive_saved_query(db: tauri::State<'_, Db>, id: i64) -> ApiResult<bool> {
    Ok(db.write(|conn| query::archive_saved_query(conn, id, Timestamp::now()))?)
}

#[tauri::command]
pub async fn query_options(db: tauri::State<'_, Db>) -> ApiResult<QueryOptions> {
    Ok(db.read(query::query_options)?)
}

// ---------------------------------------------------------------------------
// Phase 5e: CSV export
// ---------------------------------------------------------------------------
//
// The frontend picks `path` with the save dialog and never touches the file. Each
// reads the table, then writes outside the read so the file I/O holds no connection.
// Canonical units regardless of `ui`: only `export.dialect` is read from the config.

/// `#/query`'s result, as the batches table or the rollup.
#[tauri::command]
pub async fn export_query(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    filter: BatchFilter,
    table: QueryTable,
    path: PathBuf,
) -> ApiResult<()> {
    let config = config.get();
    let params = config.stability.params();
    let table = db.read(|conn| export::query(conn, &filter, &params, table))?;
    Ok(export::write_file(&path, &table, config.export.dialect)?)
}

/// The overlay's batches and span, every point.
#[tauri::command]
pub async fn export_comparison(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_ids: Vec<i64>,
    y_mode: YMode,
    with_temp: bool,
    range: Option<RelRange>,
    path: PathBuf,
) -> ApiResult<()> {
    let table = db.read(|conn| export::comparison(conn, &batch_ids, y_mode, with_temp, range))?;
    Ok(export::write_file(
        &path,
        &table,
        config.get().export.dialect,
    )?)
}

/// Every reading bound to the batch.
#[tauri::command]
pub async fn export_batch_readings(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
    path: PathBuf,
) -> ApiResult<()> {
    let table = db.read(|conn| export::readings(conn, batch_id))?;
    Ok(export::write_file(
        &path,
        &table,
        config.get().export.dialect,
    )?)
}
