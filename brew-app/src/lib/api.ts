/**
 * The invoke boundary. Every command goes through here so error handling exists in
 * exactly one place and the Rust types are mirrored in exactly one file.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Currency, GravityDisplay, Money, TempDisplay } from "$lib/units";

/** Mirrors `src-tauri/src/error.rs`. `code` is stable; `message` is for humans. */
export interface ApiError {
	code: string;
	message: string;
	detail?: string | null;
}

/** Mirrors `brewcore::config::Config`. Field names are serde's, so snake_case. */
export interface Config {
	fermentrack: { base_url: string };
	brewd: { base_url: string };
	sync: { poll_interval_secs: number };
	ui: { temp_display: TempDisplay; gravity_display: GravityDisplay };
	measurement: { hydrometer_calibration_c: number; refractometer_wort_correction: number };
	gravity: { ispindel_unit: IspindelUnit | null };
	stability: { window_hours: number; slope_sg_per_day: number; fg_band: number };
	health: { battery_warn_v: number; rssi_warn_dbm: number };
	export: { dialect: ExportDialect };
	money: { default_currency: Currency };
}

/** Mirrors `brewcore::config::IspindelUnit`. `null` in `Config` means not recorded yet. */
export type IspindelUnit = "sg" | "plato";

/** Mirrors `brewcore::config::ExportDialect`. `excel_fr`: `;`, decimal comma, UTF-8 BOM. */
export type ExportDialect = "rfc4180" | "excel_fr";

/** Mirrors `brewcore::db::DbStatus`. */
export interface DbStatus {
	path: string;
	size_bytes: number;
	schema_version: number;
	row_counts: Record<string, number>;
}

/** Mirrors `brewcore::fermentrack::csv_log::HeaderInfo`. */
export interface HeaderInfo {
	header: string;
	gravity_enabled: boolean;
	unknown_columns: string[];
}

/** Mirrors `brewcore::fermentrack::discovery::LogProbe`. */
export interface LogProbe {
	url: string;
	base_url: string;
	remote_path: string;
	file_name: string;
	suggested_name: string;
	device_id: number | null;
	log_id: number | null;
	header: HeaderInfo;
}

/** Mirrors `brewcore::fermentrack::discovery::SourceSummary`. Timestamps are epoch seconds. */
export interface SourceSummary {
	id: number;
	kind: string;
	name: string;
	remote_path: string | null;
	byte_offset: number;
	last_ts: number | null;
	newest_reading: number | null;
	/** Readings this source owns. Named in Rebuild's confirmation. */
	reading_count: number;
	last_error: string | null;
	synced_at: number | null;
}

/** Mirrors `brewcore::journal::JournalEntry`, one row of `brewd`'s `GET /journals/`. */
export interface JournalEntry {
	name: string;
	bytes: number;
	modified: number;
}

/** Mirrors `brewcore::brewd::MappedColumn`. `metric` is `Metric::as_str`. */
export interface MappedColumn {
	column: string;
	metric: string;
}

/** Mirrors `brewcore::brewd::JournalHeader`. */
export interface JournalHeader {
	header: string;
	mapped: MappedColumn[];
	unknown_columns: string[];
	/** A `gravity` column: the journal will not sync until the iSpindle unit is set. */
	needs_ispindel_unit: boolean;
}

/** Mirrors `brewcore::brewd::JournalProbe`. */
export interface JournalProbe {
	journal: string;
	remote_path: string;
	suggested_name: string;
	/** Renamed away by `brewd` on a header change. It will not grow. */
	rotated: boolean;
	header: JournalHeader;
}

/**
 * Tauri rejects with whatever the command returned, so ours arrive as `ApiError` —
 * but argument-deserialisation failures and panics do not. Tauri produces those
 * itself, as plain strings, before our code runs, so anything reading `e.code`
 * straight off a rejection eventually renders `undefined`.
 */
export function toApiError(e: unknown): ApiError {
	if (typeof e === "object" && e !== null && "code" in e && "message" in e) {
		return e as ApiError;
	}
	return { code: "unknown", message: typeof e === "string" ? e : String(e) };
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return await invoke<T>(cmd, args);
	} catch (e) {
		throw toApiError(e);
	}
}

export const getConfig = (): Promise<Config> => call("get_config");

/** Returns the *stored* config, which may differ from `config` after normalisation. */
export const setConfig = (config: Config): Promise<Config> => call("set_config", { config });

export const dbStatus = (): Promise<DbStatus> => call("db_status");

/**
 * Validate a pasted log URL or path without writing anything. Rejects with a code the
 * caller is meant to branch on: `fermentrack.graph_log` and `fermentrack.gravity_log`
 * are two different mistakes.
 */
export const probeLog = (input: string): Promise<LogProbe> => call("probe_log", { input });

/** Validates again server-side before writing; the probe is not taken on trust. */
export const addSource = (input: string, name?: string): Promise<SourceSummary> =>
	call("add_source", { input, name: name ?? null });

export const listSources = (): Promise<SourceSummary[]> => call("list_sources");

/** Device ids from `GET /api/devices/`, purely to confirm the host is a Fermentrack. */
export const checkFermentrack = (baseUrl?: string): Promise<number[]> =>
	call("check_fermentrack", { baseUrl: baseUrl ?? null });

// ---------------------------------------------------------------------------
// Phase Ve: brewd
// ---------------------------------------------------------------------------

/** Every journal `brewd` has. Also the brewd "Test connection": an unsaved URL wins. */
export const listJournals = (baseUrl?: string): Promise<JournalEntry[]> =>
	call("list_journals", { baseUrl: baseUrl ?? null });

/** Read a journal's header against the saved brewd URL. Writes nothing. */
export const probeJournal = (journal: string): Promise<JournalProbe> =>
	call("probe_journal", { journal });

/** Probes again server-side, adds, and starts polling. */
export const addJournalSource = (journal: string, name?: string): Promise<SourceSummary> =>
	call("add_journal_source", { journal, name: name ?? null });

// ---------------------------------------------------------------------------
// Phase 2g: sync
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::sync::SyncPhase`. */
export type SyncPhase = "idle" | "syncing" | "error" | "unreachable";

/** Mirrors `brewcore::sync::SourceStatus`. Timestamps are epoch seconds. */
export interface SourceStatus {
	source_id: number;
	phase: SyncPhase;
	last_success: number | null;
	last_error: string | null;
	/** `ApiError.code` of the last failed poll; `null` for a committed fault. */
	last_error_code: string | null;
	rows_added: number;
	newest_reading: number | null;
	consecutive_failures: number;
	next_poll_at: number | null;
}

/**
 * Poll now instead of at the next tick. Resolves as soon as the request is queued —
 * the result arrives on `sync://status`, not here.
 */
export const resync = (sourceId: number): Promise<void> => call("resync", { sourceId });

/**
 * Delete every reading for this source and derive them again from byte zero.
 * Destructive to derived data only, but confirm with the user before calling it.
 */
export const rebuild = (sourceId: number): Promise<void> => call("rebuild", { sourceId });

/** Every source's current state, for first paint before any event has arrived. */
export const syncStatus = (): Promise<SourceStatus[]> => call("sync_status");

/**
 * Subscribe to sync transitions. Returns the unlisten function — call it on destroy,
 * or a re-mounted component gets two of every event.
 *
 * `listen` resolves asynchronously, so an unsubscribe that races the subscribe has to
 * be remembered rather than ignored; hence the flag.
 */
export function onSyncStatus(cb: (status: SourceStatus) => void): () => void {
	let stop: (() => void) | null = null;
	let cancelled = false;

	void listen<SourceStatus>("sync://status", (event) => cb(event.payload)).then((unlisten) => {
		if (cancelled) unlisten();
		else stop = unlisten;
	});

	return () => {
		cancelled = true;
		stop?.();
	};
}

// ---------------------------------------------------------------------------
// Phase Ve: vessels
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::reference::VesselKind`. */
export type VesselKind = "fermenter" | "brite" | "keg" | "carboy" | "kettle" | "other";

/** Mirrors `src-tauri`'s `VesselView`: a `VesselRow` plus its default stream ids. */
export interface Vessel {
	id: number;
	name: string;
	kind: VesselKind;
	capacity_l: number | null;
	notes: string | null;
	archived_at: number | null;
	/** Bound to a batch from its pitch, closed when it is packaged. */
	default_streams: number[];
}

/** Mirrors `brewcore::db::tier1::StreamRow`. */
export interface Stream {
	id: number;
	source_id: number;
	source_name: string;
	source_kind: string;
	external_id: string;
	metric: string;
}

export const listVessels = (): Promise<Vessel[]> => call("list_vessels");

export const createVessel = (
	name: string,
	kind: VesselKind,
	capacityL: number | null,
): Promise<number> => call("create_vessel", { name, kind, capacityL });

export const listStreams = (): Promise<Stream[]> => call("list_streams");

/** Replaces the whole set. Batches already pitched keep what they were bound to. */
export const setVesselStreams = (vesselId: number, streamIds: number[]): Promise<void> =>
	call("set_vessel_streams", { vesselId, streamIds });

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U1: ingredients, yeast strains, lots
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::reference::Category`. */
export type Category =
	| "fermentable"
	| "sugar"
	| "yeast"
	| "nutrient"
	| "acid"
	| "salt"
	| "flavoring"
	| "fining"
	| "gas"
	| "water";

/** Mirrors `brewcore::units::Unit::symbol`. */
export type Unit = "mg" | "g" | "kg" | "oz" | "lb" | "mL" | "L" | "hL" | "gal" | "ea";

/** Mirrors `brewcore::db::reference::YeastForm`. */
export type YeastForm = "dry" | "liquid" | "slurry";

/** Mirrors `brewcore::units::Quantity`: a number as it was typed. */
export interface Quantity {
	value: number;
	unit: Unit;
}

/** Mirrors `brewcore::db::reference::CanonicalQty`: grams, litres or a count. */
export type CanonicalQty = { mass: number } | { volume: number } | { count: number };

/** Mirrors `src-tauri`'s `IngredientView`. */
export interface Ingredient {
	id: number;
	name: string;
	category: Category;
	default_unit: Unit;
	yeast_strain_id: number | null;
	attrs: string | null;
	notes: string | null;
	archived_at: number | null;
	/** "EC-1118 (Lallemand)". */
	yeast_strain: string | null;
	mg_yan_per_g: number | null;
	/** Active lots only. */
	lot_count: number;
}

/** Mirrors `src-tauri`'s `IngredientInput`. No `attrs`: the YAN figure is merged in Rust. */
export interface IngredientInput {
	name: string;
	category: Category;
	default_unit: Unit;
	yeast_strain_id: number | null;
	mg_yan_per_g: number | null;
	notes: string | null;
}

/** Mirrors `brewcore::db::reference::NewYeastStrain`. Attenuation and tolerance are fractions. */
export interface YeastStrainInput {
	name: string;
	lab: string | null;
	form: YeastForm | null;
	temp_min_c: number | null;
	temp_max_c: number | null;
	attenuation_min: number | null;
	attenuation_max: number | null;
	alcohol_tolerance: number | null;
	notes: string | null;
}

/** Mirrors `brewcore::db::reference::YeastStrainRow`. */
export interface YeastStrain extends YeastStrainInput {
	id: number;
	form: YeastForm;
	archived_at: number | null;
}

/** Mirrors `src-tauri`'s `YanPreset`, from `calc::yan::PRESETS`. */
export interface YanPreset {
	label: string;
	mg_yan_per_g: number;
}

/** Mirrors `src-tauri`'s `LotView`. */
export interface Lot {
	id: number;
	ingredient_id: number;
	supplier: string | null;
	lot_code: string | null;
	qty: CanonicalQty;
	entry: Quantity;
	cost: Money | null;
	purchased_at: number | null;
	expires_at: number | null;
	notes: string | null;
	archived_at: number | null;
	/** Minor units per gram / litre / unit. */
	cost_per_canonical_unit: number | null;
	batch_count: number;
}

/** Mirrors `src-tauri`'s `LotInput`. `cost` as typed (`12,50`); blank is no cost. */
export interface LotInput {
	ingredient_id: number;
	supplier: string | null;
	lot_code: string | null;
	entry: Quantity;
	cost: string | null;
	currency: Currency | null;
	purchased_at: number | null;
	expires_at: number | null;
	notes: string | null;
}

export const listIngredients = (includeArchived: boolean): Promise<Ingredient[]> =>
	call("list_ingredients", { includeArchived });

export const createIngredient = (ingredient: IngredientInput): Promise<number> =>
	call("create_ingredient", { ingredient });

export const updateIngredient = (id: number, ingredient: IngredientInput): Promise<void> =>
	call("update_ingredient", { id, ingredient });

export const archiveIngredient = (id: number): Promise<boolean> => call("archive_ingredient", { id });

export const restoreIngredient = (id: number): Promise<boolean> => call("restore_ingredient", { id });

export const yanPresets = (): Promise<YanPreset[]> => call("yan_presets");

export const listYeastStrains = (includeArchived: boolean): Promise<YeastStrain[]> =>
	call("list_yeast_strains", { includeArchived });

export const createYeastStrain = (strain: YeastStrainInput): Promise<number> =>
	call("create_yeast_strain", { strain });

export const updateYeastStrain = (id: number, strain: YeastStrainInput): Promise<void> =>
	call("update_yeast_strain", { id, strain });

export const archiveYeastStrain = (id: number): Promise<boolean> =>
	call("archive_yeast_strain", { id });

export const restoreYeastStrain = (id: number): Promise<boolean> =>
	call("restore_yeast_strain", { id });

export const listLots = (ingredientId: number, includeArchived: boolean): Promise<Lot[]> =>
	call("list_lots", { ingredientId, includeArchived });

export const createLot = (lot: LotInput): Promise<number> => call("create_lot", { lot });

export const updateLot = (id: number, lot: LotInput): Promise<void> => call("update_lot", { id, lot });

export const archiveLot = (id: number): Promise<boolean> => call("archive_lot", { id });

export const restoreLot = (id: number): Promise<boolean> => call("restore_lot", { id });

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U2: recipes
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::recipe::Stage`. */
export type Stage = "prep" | "pitch" | "fermentation" | "conditioning" | "packaging";

/** Mirrors `brewcore::db::recipe::RecipeRow`. */
export interface Recipe {
	id: number;
	name: string;
	beverage_type: BeverageType;
	notes: string | null;
	archived_at: number | null;
}

/** Mirrors `src-tauri`'s `RecipeSummary`. */
export interface RecipeSummary extends Recipe {
	version_count: number;
	latest_version_no: number | null;
}

/** Mirrors `src-tauri`'s `RecipeInput`. */
export interface RecipeInput {
	name: string;
	beverage_type: BeverageType;
	notes: string | null;
}

/** Mirrors `src-tauri`'s `VersionTab`. */
export interface VersionTab {
	id: number;
	version_no: number;
	/** Non-null: a batch points at it, so edits fork instead. */
	locked_at: number | null;
	batch_count: number;
}

/** Mirrors `src-tauri`'s `RecipeDetail`. */
export interface RecipeDetail {
	recipe: Recipe;
	versions: VersionTab[];
}

/** Mirrors `brewcore::db::recipe::RecipeItemRow`. */
export interface RecipeItem {
	id: number;
	recipe_version_id: number;
	ingredient_id: number;
	qty: CanonicalQty;
	entry: Quantity;
	stage: Stage;
	/** Hours from pitch; negative is before it. */
	offset_hours: number | null;
	notes: string | null;
}

/** Mirrors `src-tauri`'s `RecipeItemView`. */
export interface RecipeItemView extends RecipeItem {
	ingredient: string;
	ingredient_archived: boolean;
}

/** Mirrors `src-tauri`'s `RecipeVersionView`. Gravities in `gravity_unit`; ABV a fraction. */
export interface RecipeVersion {
	id: number;
	recipe_id: number;
	version_no: number;
	locked_at: number | null;
	batch_count: number;
	target_volume_l: number | null;
	target_og: number | null;
	target_fg: number | null;
	target_abv: number | null;
	abv_alternate: number | null;
	notes: string | null;
	gravity_unit: string;
	items: RecipeItemView[];
}

/** Mirrors `src-tauri`'s `RecipeVersionInput`. Gravities in the display unit. */
export interface RecipeVersionInput {
	target_volume_l: number | null;
	target_og: number | null;
	target_fg: number | null;
	target_abv: number | null;
	notes: string | null;
}

/** Mirrors `brewcore::db::recipe::NewRecipeItem`. */
export interface RecipeItemInput {
	ingredient_id: number;
	entry: Quantity;
	stage: Stage;
	offset_hours: number | null;
	notes: string | null;
}

/** Mirrors `src-tauri`'s `TargetChange`. */
export interface TargetChange {
	field: "target_volume_l" | "target_og" | "target_fg" | "target_abv";
	from: number | null;
	to: number | null;
}

/** Mirrors `src-tauri`'s `ItemChangeView`. */
export interface ItemChange {
	change: "added" | "removed" | "changed";
	ingredient: string;
	stage: Stage;
	from: RecipeItem | null;
	to: RecipeItem | null;
}

/** Mirrors `src-tauri`'s `RecipeDiffView`. */
export interface RecipeDiff {
	from_version_no: number;
	to_version_no: number;
	gravity_unit: string;
	targets: TargetChange[];
	items: ItemChange[];
}

export const listRecipes = (includeArchived: boolean): Promise<RecipeSummary[]> =>
	call("list_recipes", { includeArchived });

export const createRecipe = (recipe: RecipeInput): Promise<{ recipe_id: number; version_id: number }> =>
	call("create_recipe", { recipe });

export const updateRecipe = (id: number, recipe: RecipeInput): Promise<void> =>
	call("update_recipe", { id, recipe });

export const archiveRecipe = (id: number): Promise<boolean> => call("archive_recipe", { id });

export const restoreRecipe = (id: number): Promise<boolean> => call("restore_recipe", { id });

export const getRecipe = (id: number): Promise<RecipeDetail> => call("get_recipe", { id });

export const getRecipeVersion = (id: number): Promise<RecipeVersion> =>
	call("get_recipe_version", { id });

export const updateRecipeVersion = (id: number, version: RecipeVersionInput): Promise<void> =>
	call("update_recipe_version", { id, version });

/** Copies the version and its items to the next number, unlocked. */
export const forkRecipeVersion = (id: number): Promise<VersionTab> =>
	call("fork_recipe_version", { id });

/** Newest unlocked version only. */
export const deleteRecipeVersion = (id: number): Promise<void> =>
	call("delete_recipe_version", { id });

export const addRecipeItem = (versionId: number, item: RecipeItemInput): Promise<number> =>
	call("add_recipe_item", { versionId, item });

export const updateRecipeItem = (id: number, item: RecipeItemInput): Promise<void> =>
	call("update_recipe_item", { id, item });

export const deleteRecipeItem = (id: number): Promise<boolean> => call("delete_recipe_item", { id });

export const diffRecipeVersions = (fromId: number, toId: number): Promise<RecipeDiff> =>
	call("diff_recipe_versions", { fromId, toId });

// ---------------------------------------------------------------------------
// Phase 4d: the batch curve
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::batch::BatchStatus`. */
export type BatchStatus = "planned" | "fermenting" | "conditioning" | "packaged";

/** Mirrors `brewcore::db::batch::BatchRow`. Timestamps are epoch seconds, gravity SG. */
export interface Batch {
	id: number;
	code: string;
	recipe_version_id: number | null;
	beverage_type: string;
	vessel_id: number | null;
	status: BatchStatus;
	volume_l: number | null;
	og: number | null;
	fg: number | null;
	/** t=0. `null` until pitched. */
	pitched_at: number | null;
	archived_at: number | null;
	notes: string | null;
}

/** Mirrors `brewcore::db::series::SeriesRange`: half-open `[start, end)`, epoch seconds. */
export interface SeriesRange {
	start: number;
	end: number;
}

/**
 * Mirrors `brewcore::db::series::Series`: one stream, columnar. `null` in a value
 * column is a gap — break the line there, never interpolate across it.
 */
export interface Series {
	stream_id: number;
	source_id: number;
	/** `Metric::as_str`: `sg`, `temp_c`, `ph`, … */
	metric: string;
	source_kind: string;
	label: string;
	/** `null` when these are raw rows, one per reading. */
	bucket_secs: number | null;
	ts: number[];
	avg: (number | null)[];
	/** Empty when raw. */
	min: (number | null)[];
	/** Empty when raw. */
	max: (number | null)[];
	/** Sensor `sg` only, empty otherwise. */
	smoothed: (number | null)[];
	smoothed_partial: boolean[];
}

/** Mirrors `src-tauri`'s `CurveView`. Gravity is already in `gravity_unit`. */
export interface Curve {
	/** The range queried. `null` for a batch with no pitch and no bindings. */
	range: SeriesRange | null;
	series: Series[];
	gravity_unit: string;
}

/** Mirrors `brewcore::db::timeline::EventKind`. */
export type EventKind =
	| "pitch"
	| "nutrient_addition"
	| "flavoring_addition"
	| "temp_ramp"
	| "cold_crash"
	| "transfer"
	| "fining"
	| "carbonation"
	| "package"
	| "note";

/** Mirrors `brewcore::db::timeline::BatchEventRow`. */
export interface BatchEvent {
	id: number;
	batch_id: number;
	ts: number;
	kind: EventKind;
	/** Opaque JSON. */
	payload: string | null;
	note: string | null;
}

/** Mirrors `brewcore::calc::stability::Stability`. */
export type Stability =
	| { state: "active" }
	| { state: "stable"; since: number; fg_unchecked: boolean }
	| { state: "stalled"; since: number }
	| {
			state: "insufficient";
			reason: "empty" | "too_few_points" | "under_filled" | "not_started";
	  };

/** Mirrors `brewcore::calc::stability::FlatSpan`. */
export interface FlatSpan {
	start: number;
	end: number;
	state: "stable" | "stalled";
}

/** Mirrors `brewcore::calc::stability::ExpectedFg`. SG. */
export interface ExpectedFg {
	lo: number;
	hi: number;
	source: "recipe" | "strain";
}

/** Mirrors `src-tauri`'s `StabilityView`. */
export interface StabilityView {
	stream_id: number | null;
	current: Stability;
	expected_fg: ExpectedFg | null;
	spans: FlatSpan[];
}

/** Mirrors `brewcore::db::batch::BatchSummary`: a batch and the names a list shows. */
export interface BatchSummary extends Batch {
	recipe_id: number | null;
	recipe_name: string | null;
	version_no: number | null;
	vessel_name: string | null;
}

export const listBatches = (includeArchived = false): Promise<BatchSummary[]> =>
	call("list_batches", { includeArchived });

export const getBatch = (batchId: number): Promise<Batch> => call("get_batch", { batchId });

/**
 * `range: null` is the default: pitch to now, or the bindings' span before a pitch.
 * `maxPoints` is per sensor series; manual series are never bucketed.
 */
export const batchSeries = (
	batchId: number,
	range: SeriesRange | null,
	maxPoints: number,
): Promise<Curve> => call("batch_series", { batchId, range, maxPoints });

/** Oldest first. */
export const listBatchEvents = (batchId: number): Promise<BatchEvent[]> =>
	call("list_batch_events", { batchId });

export const batchStability = (batchId: number): Promise<StabilityView> =>
	call("batch_stability", { batchId });

// ---------------------------------------------------------------------------
// Phase 4e: the status card
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::dashboard::Gravity`. `sg` is SG whatever the display unit. */
export interface DashboardGravity {
	ts: number;
	sg: number;
	/** 4b's rolling median rather than a reading. */
	smoothed: boolean;
	source_kind: string;
	label: string;
	/** Manual readings only: `hydrometer`, `refractometer`. */
	instrument: string | null;
}

/** Mirrors `brewcore::db::dashboard::Reading`. Temperature is °C. */
export interface DashboardReading {
	metric: string;
	ts: number;
	value: number;
	source_kind: string;
	label: string;
	stale: boolean;
}

/** Mirrors `brewcore::db::dashboard::Estimate`. Fractions, not percent. */
export type Estimate =
	| { state: "estimated"; abv_alternate: number; apparent_attenuation: number }
	| { state: "no_og" | "no_gravity" | "not_started" | "og_too_low" };

/** Mirrors `src-tauri`'s `DashboardView`: SG fields, plus `display` in `gravity_unit`. */
export interface Dashboard {
	batch_id: number;
	og: number | null;
	/** The earliest gravity, offered while `og` is null. */
	og_candidate: DashboardGravity | null;
	/** `batch.fg`, set deliberately. */
	fg: number | null;
	gravity: DashboardGravity | null;
	estimate: Estimate;
	ph: DashboardReading | null;
	temp: DashboardReading | null;
	stability: Stability;
	expected_fg: ExpectedFg | null;
	source_ids: number[];
	/** Bound devices that report a battery or a signal. */
	devices: DeviceHealth[];
	display: {
		og: number | null;
		og_candidate: number | null;
		fg: number | null;
		gravity: number | null;
		expected_fg_lo: number | null;
		expected_fg_hi: number | null;
	};
	gravity_unit: string;
}

export const batchDashboard = (batchId: number): Promise<Dashboard> =>
	call("batch_dashboard", { batchId });

/** `og` in SG: send back `og_candidate.sg`, not the display value. `null` clears it; an OG
 * that post-pitch refractometer readings were corrected against is refused. */
export const setBatchOg = (batchId: number, og: number | null): Promise<void> =>
	call("set_batch_og", { batchId, og });

/** `fg` in SG; `null` clears it. Must not be above the OG. */
export const setBatchFg = (batchId: number, fg: number | null): Promise<void> =>
	call("set_batch_fg", { batchId, fg });

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U3: batch create and lifecycle
// ---------------------------------------------------------------------------

/** Mirrors `src-tauri`'s `BatchInput`. `og` in the display gravity unit. */
export interface BatchInput {
	code: string;
	recipe_version_id: number | null;
	beverage_type: BeverageType;
	vessel_id: number | null;
	volume_l: number | null;
	og: number | null;
	notes: string | null;
}

/** Mirrors `brewcore::db::batch::BatchEdit`. Replaces all four; `null` clears. */
export interface BatchEdit {
	code: string;
	vessel_id: number | null;
	volume_l: number | null;
	notes: string | null;
}

export const suggestBatchCode = (beverageType: BeverageType): Promise<string> =>
	call("suggest_batch_code", { beverageType });

/** Locks the recipe version, if any. */
export const createBatch = (batch: BatchInput): Promise<number> => call("create_batch", { batch });

export const updateBatch = (batchId: number, batch: BatchEdit): Promise<void> =>
	call("update_batch", { batchId, batch });

/** Closes the batch's open sensor bindings, so the vessel is free for the next pitch. */
export const archiveBatch = (batchId: number): Promise<boolean> => call("archive_batch", { batchId });

export const restoreBatch = (batchId: number): Promise<boolean> => call("restore_batch", { batchId });

/** `at` in epoch seconds, `null` for now; a future time is refused. */
export const recordPitch = (batchId: number, at: number | null, note: string | null): Promise<number> =>
	call("record_pitch", { batchId, at, note });

export const undoPitch = (batchId: number): Promise<boolean> => call("undo_pitch", { batchId });

/** One step forward. `packaged` closes the sensor bindings at `at`. */
export const transitionBatch = (batchId: number, to: BatchStatus, at: number | null): Promise<boolean> =>
	call("transition_batch", { batchId, to, at });

// ---------------------------------------------------------------------------
// Phase 4f: home
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::health::Level`. */
export interface Level {
	ts: number;
	value: number;
	/** Under the configured threshold. */
	warn: boolean;
}

/** Mirrors `brewcore::calc::health::SilenceGuess`. */
export type SilenceGuess = "battery" | "signal";

/** Mirrors `brewcore::db::health::DeviceHealth`. */
export interface DeviceHealth {
	source_id: number;
	name: string;
	kind: string;
	newest: number | null;
	stale: boolean;
	battery: Level | null;
	rssi: Level | null;
	/** `[ts, volts]`, oldest first. */
	battery_7d: [number, number][];
	/** Only when stale, and a guess. */
	guess: SilenceGuess | null;
}

/** Mirrors `brewcore::db::series::Freshness`. */
export interface Freshness {
	stream_id: number;
	source_id: number;
	metric: string;
	source_kind: string;
	label: string;
	newest: number | null;
	stale: boolean;
}

/** Mirrors `src-tauri`'s `HomeBatchView`. `display` is in `HomeSummary.gravity_unit`. */
export interface HomeBatch {
	code: string;
	status: string;
	vessel: string | null;
	dashboard: Omit<Dashboard, "display" | "gravity_unit">;
	streams: Freshness[];
	newest: number | null;
	display: Dashboard["display"];
}

/** Mirrors `src-tauri`'s `HomeView`. */
export interface HomeSummary {
	batches: HomeBatch[];
	devices: DeviceHealth[];
	gravity_unit: string;
}

export const homeSummary = (): Promise<HomeSummary> => call("home_summary");

// ---------------------------------------------------------------------------
// Phase 5b: the comparison overlay
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::calc::batch_metrics::Missing`: why a number isn't there. */
export type Missing =
	| "no_pitch"
	| "no_og"
	| "no_fg"
	| "implausible"
	| "no_sensor_gravity"
	| "manual_only"
	| "not_started"
	| "never_stable"
	| "too_few_points"
	| "no_temp"
	| "no_yeast"
	| "no_volume"
	| "no_cost"
	| "mixed_currency"
	| "no_nutrient_yield";

/** Mirrors `brewcore::db::compare::RelRange`: seconds since each batch's pitch. */
export interface RelRange {
	start: number;
	end: number;
}

export type YMode = "raw" | "fraction";

/** Mirrors `brewcore::db::compare::YAxis`. `reason` is why a fraction fell back to raw. */
export type YAxis = { kind: "fraction" } | { kind: "raw"; reason: Missing | null };

export interface ComparedBatch {
	batch_id: number;
	code: string;
	pitched_at: number;
	og: number | null;
	y: YAxis;
	/** Absolute `ts`, like the curve's; hours since pitch is `toX`. */
	gravity: Series[];
	temp: Series | null;
}

export interface Excluded {
	batch_id: number;
	code: string;
	reason: Missing;
}

export interface CompareView {
	/** `null` when no batch has anything bound. */
	span: RelRange | null;
	/** In the order asked for. */
	batches: ComparedBatch[];
	excluded: Excluded[];
	/** The unit of any batch left raw. */
	gravity_unit: string;
}

export const compareBatches = (
	batchIds: number[],
	yMode: YMode,
	withTemp: boolean,
	range: RelRange | null,
	maxPoints: number,
): Promise<CompareView> =>
	call("compare_batches", { batchIds, yMode, withTemp, range, maxPoints });

// ---------------------------------------------------------------------------
// Phase 5c: saved queries
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::recipe::BeverageType`. */
export type BeverageType =
	| "seltzer"
	| "beer"
	| "cider"
	| "wine"
	| "mead"
	| "kombucha"
	| "other";

/** Mirrors `brewcore::db::query::Bounds`. Inclusive; `null` leaves that end open. */
export interface Bounds {
	min: number | null;
	max: number | null;
}

/**
 * Mirrors `brewcore::db::query::BatchFilter`. Canonical units: SG, °C, epoch seconds.
 * Every field is required — the backend refuses a filter with one missing rather than
 * guess, so build from `emptyFilter` in `$lib/query`.
 */
export interface BatchFilter {
	beverage_types: BeverageType[];
	statuses: BatchStatus[];
	include_archived: boolean;
	recipe_id: number | null;
	recipe_version_id: number | null;
	strain_ids: number[];
	lot_ids: number[];
	og: Bounds | null;
	fg: Bounds | null;
	ferm_temp_c: Bounds | null;
	pitched: Bounds | null;
}

/** Mirrors `brewcore::calc::batch_metrics::Computed`. */
export type Computed<T> = { value: T } | { missing: Missing };

/** Mirrors `src-tauri`'s `QueryRowView`: 5a's `BatchMetrics` plus OG / FG for display. */
export interface QueryRow {
	batch_id: number;
	code: string;
	beverage_type: BeverageType;
	status: BatchStatus;
	archived_at: number | null;
	pitched_at: number | null;
	/** SG. */
	og: number | null;
	fg: number | null;
	lag: Computed<{ at: number; secs: number }>;
	peak_rate: Computed<{ sg_per_day: number; at: number; secs_from_pitch: number }>;
	terminal: Computed<{ at: number; secs_from_pitch: number; fg_unchecked: boolean }>;
	/** Fractions, not percent. */
	apparent_attenuation: Computed<number>;
	abv_alternate: Computed<number>;
	/** °C. */
	ferm_temp: Computed<{
		median: number;
		min: number;
		max: number;
		n: number;
		stream_id: number;
		to_terminal: boolean;
	}>;
	yan_added_mg_per_l: Computed<number>;
	cost_per_litre: Computed<Money>;
	cost_lines_without_cost: number;
	strains: { id: number; name: string }[];
	/** In `QueryView.gravity_unit`. */
	display_og: number | null;
	display_fg: number | null;
}

/** Mirrors `brewcore::db::query::Criterion`. */
export type Criterion = "og" | "fg" | "pitched" | "strain" | "ferm_temp";

/** A batch the filter couldn't judge, and why. Neither a match nor a mismatch. */
export interface Unevaluated {
	batch_id: number;
	code: string;
	criterion: Criterion;
	reason: Missing;
}

/** Mirrors `brewcore::calc::batch_metrics::Spread`. */
export interface Spread {
	median: number;
	min: number;
	max: number;
	/** How many batches had the value. */
	n: number;
}

/** Mirrors `brewcore::db::rollup::RollupGroup`. */
export type RollupGroup =
	| { kind: "strain"; id: number; name: string }
	| { kind: "mixed"; strains: { id: number; name: string }[] };

export type BandCheck = "below" | "within" | "above";

/** Mirrors `brewcore::db::rollup::StrainRollup`. */
export interface StrainRollup {
	group: RollupGroup;
	batch_ids: number[];
	n: number;
	/** `n` under 3: shown, greyed. */
	thin: boolean;
	/** Fractions. */
	attenuation: Spread | null;
	lag_secs: Spread | null;
	terminal_secs: Spread | null;
	/** °C. */
	ferm_temp_c: Spread | null;
	/** The strain record's datasheet band, fractions. */
	band: { min: number; max: number } | null;
	band_check: BandCheck | null;
}

/** Mirrors `brewcore::db::rollup::Rollup`: 5d, over the same rows as the table. */
export interface Rollup {
	rows: StrainRollup[];
	no_yeast: number[];
}

/** Mirrors `src-tauri`'s `QueryView`. */
export interface QueryView {
	rows: QueryRow[];
	unevaluated: Unevaluated[];
	rollup: Rollup;
	gravity_unit: string;
}

export interface SavedQuerySummary {
	id: number;
	name: string;
	created_at: number;
	archived_at: number | null;
}

export interface SavedQuery extends SavedQuerySummary {
	filter: BatchFilter;
}

/** Mirrors `brewcore::db::query::QueryOptions`: what the filter form can pick from. */
export interface QueryOptions {
	recipes: {
		id: number;
		name: string;
		beverage_type: BeverageType;
		archived: boolean;
		/** Locked versions only. */
		versions: { id: number; version_no: number }[];
	}[];
	strains: { id: number; name: string; lab: string | null }[];
	/** Only lots some batch used. */
	lots: {
		id: number;
		ingredient: string;
		category: string;
		lot_code: string | null;
		supplier: string | null;
		purchased_at: number | null;
		batch_count: number;
	}[];
}

export const runBatchQuery = (filter: BatchFilter): Promise<QueryView> =>
	call("run_batch_query", { filter });

/** Fails with `query.filter_version` when a newer build saved it. */
export const runSavedQuery = (id: number): Promise<{ query: SavedQuery; result: QueryView }> =>
	call("run_saved_query", { id });

export const listSavedQueries = (includeArchived = false): Promise<SavedQuerySummary[]> =>
	call("list_saved_queries", { includeArchived });

export const createSavedQuery = (name: string, filter: BatchFilter): Promise<SavedQuery> =>
	call("create_saved_query", { name, filter });

/** Rename and replace the filter in place. */
export const updateSavedQuery = (
	id: number,
	name: string,
	filter: BatchFilter,
): Promise<SavedQuery> => call("update_saved_query", { id, name, filter });

export const archiveSavedQuery = (id: number): Promise<boolean> =>
	call("archive_saved_query", { id });

export const queryOptions = (): Promise<QueryOptions> => call("query_options");

// ---------------------------------------------------------------------------
// Phase 5e: CSV export
// ---------------------------------------------------------------------------
// `path` comes from the save dialog (`$lib/export`); Rust writes the file, in
// canonical units and the configured `export.dialect`. Fails with `export.write`.

export type QueryTable = "batches" | "strains";

export const exportQuery = (filter: BatchFilter, table: QueryTable, path: string): Promise<void> =>
	call("export_query", { filter, table, path });

export const exportComparison = (
	batchIds: number[],
	yMode: YMode,
	withTemp: boolean,
	range: RelRange | null,
	path: string,
): Promise<void> => call("export_comparison", { batchIds, yMode, withTemp, range, path });

export const exportBatchReadings = (batchId: number, path: string): Promise<void> =>
	call("export_batch_readings", { batchId, path });

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U4: timeline and quick entry
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::measurement::Instrument`. */
export type Instrument = "hydrometer" | "refractometer" | "ph_meter" | "thermometer" | "titration";

/** Mirrors `brewcore::db::measurement::EntryUnit`: the scale typed in. */
export type EntryUnit = "sg" | "plato" | "brix" | "c" | "f" | "ph" | "g_per_l";

/** The four metrics a person measures by hand. */
export type ManualMetric = "sg" | "temp_c" | "ph" | "ta";

/** Mirrors `brewcore::db::measurement::NewMeasurement`. Temperatures °C. */
export interface NewMeasurement {
	metric: ManualMetric;
	entry_value: number;
	entry_unit: EntryUnit;
	instrument: Instrument | null;
	sample_temp_c: number | null;
	calibration_temp_c: number | null;
	wort_correction: number | null;
}

/** Mirrors `brewcore::db::measurement::MeasurementRow`. `value` is canonical and corrected. */
export interface Measurement {
	id: number;
	batch_id: number;
	stream_id: number;
	ts: number;
	metric: ManualMetric;
	value: number;
	entry_value: number;
	entry_unit: EntryUnit;
	instrument: Instrument | null;
	sample_temp_c: number | null;
	calibration_temp_c: number | null;
	wort_correction: number | null;
}

/** Mirrors `src-tauri`'s `TimelineItemView`. `display` is a gravity in `gravity_unit`. */
export type TimelineItem =
	| ({ type: "event" } & BatchEvent)
	| ({ type: "measurement"; display: number | null } & Measurement);

/** Mirrors `src-tauri`'s `TimelineView`. Oldest first. */
export interface Timeline {
	pitched_at: number | null;
	gravity_unit: string;
	items: TimelineItem[];
}

/** Mirrors `src-tauri`'s `QuickEntryInput`. `ts: null` is now. */
export interface QuickEntryInput {
	ts: number | null;
	measurements: NewMeasurement[];
	note: string | null;
}

/** Mirrors `src-tauri`'s `EventInput`. `ts: null` is now. */
export interface EventInput {
	ts: number | null;
	kind: EventKind;
	note: string | null;
}

export const batchTimeline = (batchId: number): Promise<Timeline> => call("batch_timeline", { batchId });

/** All at one `ts`; a note becomes a `note` event. All or nothing. */
export const recordQuickEntry = (
	batchId: number,
	entry: QuickEntryInput,
): Promise<{ measurements: Measurement[]; event_id: number | null }> =>
	call("record_quick_entry", { batchId, entry });

/** Takes its reading off the curve too. */
export const deleteMeasurement = (measurementId: number): Promise<boolean> =>
	call("delete_measurement", { measurementId });

/** Not `pitch`: that is `recordPitch`. */
export const addEvent = (batchId: number, event: EventInput): Promise<number> =>
	call("add_event", { batchId, event });

export const updateEvent = (eventId: number, event: EventInput): Promise<void> =>
	call("update_event", { eventId, event });

export const deleteEvent = (eventId: number): Promise<boolean> => call("delete_event", { eventId });
