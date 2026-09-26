//! Core error type. The Tauri layer converts this into a serialisable
//! `{code, message, detail}` at its boundary; nothing here knows that exists.

use std::path::PathBuf;

use crate::units::{Currency, Dimension};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("connection pool error: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    // Separate variants from `Io` because `std::io::Error` carries no path: on
    // Windows a failed save reads "Access is denied. (os error 5)" with no hint as
    // to what was denied. Read and write are separate in turn because this message
    // reaches the settings pane verbatim, and "could not read" on a failed Save
    // sends the user looking in the wrong place.
    #[error("could not read config at {path}: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write config to {path}: {source}")]
    ConfigWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Phase 5e. Its own variant for the same reason as [`Error::ConfigWrite`]: the
    /// path is the part the user chose, and the only part that helps.
    #[error("could not write export to {path}: {source}")]
    ExportWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    // `#[source]` rather than `#[from]`: a bare `#[from]` would let `?` drop which
    // file failed to parse, and Phase 2 reads more than one.
    #[error("{path} is not valid TOML: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not serialise config: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    #[error("{field}: {reason}")]
    ConfigInvalid { field: &'static str, reason: String },

    #[error("could not enable WAL on {path}: journal_mode is '{mode}'")]
    JournalMode { path: PathBuf, mode: String },

    #[error(
        "database is at schema version {found}, but this build only knows up to \
         {head}; a newer version of the app wrote this file"
    )]
    SchemaAhead { found: i32, head: i32 },

    /// A table-rebuild migration left rows pointing at nothing. It was run with
    /// `foreign_keys=OFF`, so SQLite itself checked nothing; `foreign_key_check` is the
    /// only thing standing between a bad rebuild and a committed dangling reference.
    #[error(
        "migration {version} would leave {violations} dangling foreign key(s);          rolled back, the database is still at the previous version"
    )]
    MigrationForeignKeys { version: i32, violations: usize },

    // Phase 2b. File-level problems only: a single bad row is warned about and
    // skipped (see `fermentrack::ParseReport`), because one malformed line must not
    // cost a week of backfill. A bad *file* is fatal, because half-ingesting one
    // writes wrong numbers that look right.
    #[error("this does not look like a Fermentrack log: {detail}")]
    NotAFermentrackLog { detail: String },

    #[error(
        "{path_hint} is a _graph.csv; use the _full.csv instead: the graph file writes 0 for a missing reading, which would be stored as a real 0.0 °C"
    )]
    GraphLogRefused { path_hint: String },

    // A gravity sensor's own log, not a beer log. `source.kind` has carried
    // 'fermentrack_gravity_log' since 0001, but nothing reads one yet, and saying so
    // is the difference between a user fixing the URL and a user filing a bug.
    #[error("{path_hint} is a gravity-sensor log; this app only ingests a beer _full.csv so far")]
    GravityLogUnsupported { path_hint: String },

    #[error("could not read the log as CSV: {0}")]
    Csv(#[from] csv::Error),

    // Phase 2c. Transport and protocol only; once the bytes are in hand, everything
    // about their content is the parser's problem — except the one check the parser
    // cannot make, which is whether they are CSV at all (`NotCsv`).
    //
    // Every variant here means the same thing to the sync loop: write `last_error`,
    // keep the source, keep its readings. "The log was renamed" must never cost a
    // week of backfill.
    #[error("the log is missing or was renamed: {url}")]
    LogMissing { url: String },

    #[error("{url} returned {status}: {preview}")]
    HttpStatus {
        url: String,
        status: u16,
        preview: String,
    },

    // A misdirected URL answers `200` with an HTML dashboard. Caught here rather than
    // in the parser because a resume calls `parse_with_header`, which would read that
    // page as zero rows and a clean report — and the offset would then advance past it.
    #[error("{url} served {content_type}, not a CSV: {preview}")]
    NotCsv {
        url: String,
        content_type: String,
        preview: String,
    },

    // `#[source]` rather than `#[from]` for the same reason as `ConfigParse`: a bare
    // `#[from]` would let `?` drop which URL failed, and Phase 2f fetches more than one.
    #[error("could not reach {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    // Split from `Http` because "Fermentrack is not responding" and "could not reach
    // Fermentrack" send the user to different places, `is_timeout()` is the only way
    // to tell them apart, and 2g's backoff should treat a slow box unlike a dead one.
    #[error("{url} did not respond in time")]
    Timeout {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    // A range answer we cannot act on: a `206` starting somewhere other than where we
    // asked, or a `416` claiming the file is longer than our offset. Both are refused
    // rather than guessed at, so the source wedges visibly — the same principle as
    // `ParseReport::stopped_early`.
    #[error("{url} answered {status} with an unusable Content-Range: {detail}")]
    RangeMismatch {
        url: String,
        status: u16,
        detail: String,
    },

    #[error("{url} is not a usable log URL: {reason}")]
    InvalidUrl { url: String, reason: String },

    // Phase 2f. The host answered, but not as a Fermentrack. Distinct from `NotCsv`
    // because that means "this URL is not a log" while this means "this box is not the
    // right box", and the two send the user to different fields in the settings pane.
    #[error("{url} does not look like a Fermentrack: {detail}")]
    NotFermentrack { url: String, detail: String },

    // Phase Vd. `brewd`'s journals. File-level refusals only, as with Fermentrack's.
    #[error("this does not look like a brewd journal: {detail}")]
    NotABrewdJournal { detail: String },

    // Nothing in an iSpindle payload says whether `gravity` is SG or °P. Refused rather
    // than guessed, and before anything is consumed, so recording the unit later
    // ingests every row the refusal held back.
    #[error("{journal} has a gravity column, but whether the iSpindle reports SG or °P is not set")]
    IspindelUnitUnset { journal: String },

    #[error("{url} does not look like brewd: {detail}")]
    NotBrewd { url: String, detail: String },

    #[error("no brewd address is set")]
    BrewdUnconfigured,

    #[error("unknown metric '{0}' in the database")]
    UnknownMetric(String),

    #[error("unknown source kind '{0}' in the database")]
    UnknownSourceKind(String),

    // Phase 3a. These three columns carry no CHECK (see `0002_reference.sql`), so the
    // enum is the only thing standing between a typo and a row nothing can read back.
    // A row written by a newer build is exactly how one gets here.
    #[error("unknown vessel kind '{0}' in the database")]
    UnknownVesselKind(String),

    #[error("unknown ingredient category '{0}' in the database")]
    UnknownCategory(String),

    #[error("unknown yeast form '{0}' in the database")]
    UnknownYeastForm(String),

    #[error("ingredient {ingredient_id} does not exist")]
    NoSuchIngredient { ingredient_id: i64 },

    // Phase 3b. Same story as the three above: `0003_recipe.sql` leaves both columns
    // open, so the enum is the only thing standing between a typo and an unreadable row.
    #[error("unknown beverage type '{0}' in the database")]
    UnknownBeverageType(String),

    #[error("unknown recipe stage '{0}' in the database")]
    UnknownStage(String),

    #[error("recipe version {recipe_version_id} does not exist")]
    NoSuchRecipeVersion { recipe_version_id: i64 },

    #[error("recipe item {recipe_item_id} does not exist")]
    NoSuchRecipeItem { recipe_item_id: i64 },

    /// Phase 3b's whole point. Not a failure — the rule working. The UI branches on
    /// this code to offer "fork to the next version" instead of an error dialog.
    #[error("recipe version {recipe_version_id} is in use by a batch; fork it to edit")]
    RecipeVersionLocked { recipe_version_id: i64 },

    // Phase 3c. `0004_batch.sql` leaves `status` open like every vocabulary before it.
    #[error("unknown batch status '{0}' in the database")]
    UnknownBatchStatus(String),

    #[error("batch {batch_id} does not exist")]
    NoSuchBatch { batch_id: i64 },

    #[error("ingredient lot {lot_id} does not exist")]
    NoSuchLot { lot_id: i64 },

    /// The status machine, refusing in the data layer rather than in the UI: a
    /// greyed-out button is not a rule, it is a suggestion the next caller ignores.
    #[error("batch {batch_id} cannot go from {from} to {to}")]
    IllegalBatchTransition {
        batch_id: i64,
        from: crate::db::batch::BatchStatus,
        to: crate::db::batch::BatchStatus,
    },

    /// Pitch is t=0 for the overlay, the finish detector and lag time alike. A batch
    /// that ferments without one can never be compared to anything.
    #[error("batch {batch_id} has no pitch time; record the pitch before fermenting")]
    BatchNotPitched { batch_id: i64 },

    #[error("batch {batch_id} is archived")]
    BatchArchived { batch_id: i64 },

    #[error("this is a {batch} batch, but the recipe version is {recipe}")]
    BeverageTypeMismatch {
        batch: crate::db::recipe::BeverageType,
        recipe: crate::db::recipe::BeverageType,
    },

    // Phase 3d. `0005_timeline.sql` leaves `kind` and `channel` open like every
    // vocabulary before them.
    #[error("unknown event kind '{0}' in the database")]
    UnknownEventKind(String),

    #[error("unknown annotation channel '{0}' in the database")]
    UnknownAnnotationChannel(String),

    #[error("batch event {event_id} does not exist")]
    NoSuchBatchEvent { event_id: i64 },

    #[error("stream binding {binding_id} does not exist")]
    NoSuchBatchStream { binding_id: i64 },

    /// Pitch is two facts written as one: the event on the timeline and
    /// `batch.pitched_at`, which 3c's status machine reads. `record_pitch` writes
    /// both; a bare `add_event` would write only the first, and the batch would carry
    /// a pitch it could not ferment from.
    #[error("record the pitch through `record_pitch`, not as a plain event")]
    UsePitchRecorder { batch_id: i64 },

    /// A fermenting batch with no pitch time is exactly the state
    /// [`Error::BatchNotPitched`] exists to prevent, so the pitch event stops being
    /// deletable the moment the batch leaves `planned`.
    #[error("batch {batch_id} has already left planned; its pitch cannot be removed")]
    PitchEventLocked { batch_id: i64 },

    /// One stream, one batch, at any instant. Overlapping bindings would put two
    /// batches' names on one curve, and neither Phase 4's chart nor Phase 5's overlay
    /// has any way to choose between them afterwards. Windows are half-open, so a
    /// fermenter turnaround — 18 starting exactly when 17 ends — is not an overlap.
    #[error("stream {stream_id} is already bound to batch {batch_id} over that window")]
    StreamAlreadyBound { stream_id: i64, batch_id: i64 },

    // Phase 3e. `0006_manual.sql` leaves `entry_unit` and `instrument` open like every
    // vocabulary before them.
    #[error("unknown instrument '{0}' in the database")]
    UnknownInstrument(String),

    #[error("unknown entry unit '{0}' in the database")]
    UnknownEntryUnit(String),

    #[error("measurement {measurement_id} does not exist")]
    NoSuchMeasurement { measurement_id: i64 },

    /// `manual_measurement_point_idx` makes this unreachable rather than merely
    /// unreached. It is an error and not an overwrite because the second entry is
    /// nearly always a different reading typed at a rounded time, and quietly keeping
    /// one of the two is how a bench log stops matching the bench. `Timestamp` has no
    /// `Display`, hence the expression form.
    #[error("this batch already has a {metric} measurement at {}", ts.as_secs())]
    MeasurementExists {
        metric: crate::db::Metric,
        ts: crate::units::Timestamp,
    },

    /// Brix expresses a gravity and nothing else; °C expresses a temperature and
    /// nothing else. Accepting the pair would mean guessing which half of it was the
    /// typo.
    #[error("{unit} is not a unit of {metric}")]
    MeasurementUnitMismatch {
        metric: crate::db::Metric,
        unit: crate::db::measurement::EntryUnit,
    },

    /// The instrument decides which correction runs, so a hydrometer claiming to have
    /// read a pH — or a wort correction factor attached to a hydrometer — is a form
    /// that would otherwise be answered with a confidently wrong number.
    #[error("a {instrument} does not measure {metric}")]
    MeasurementInstrumentMismatch {
        metric: crate::db::Metric,
        instrument: crate::db::measurement::Instrument,
    },

    /// Enforced, not advised. A sensor sending 47 has a next sample; a human typing 47
    /// into a pH box does not.
    #[error("{value} is not a plausible {metric}")]
    MeasurementImplausible {
        metric: crate::db::Metric,
        value: f64,
    },

    /// Terrill's cubic needs the original gravity to subtract the alcohol term, and
    /// there is no defensible default for it.
    #[error(
        "batch {batch_id} has no original gravity to correct a post-pitch refractometer reading against"
    )]
    NoOriginalGravity { batch_id: i64 },

    /// Phase 5b. One batch is not a comparison, and past eight the lines cannot be
    /// told apart. Counted after duplicates are dropped.
    #[error("compare 2 to {max} batches, not {n}", max = crate::db::compare::MAX_BATCHES)]
    CompareCount { n: usize },

    /// Phase 5c. A stored filter with a missing or newer `"v"`. Refused rather than
    /// read in part: a skipped criterion widens the result, and a wider result looks
    /// right. An unknown field under the right version is [`Error::InvalidFilter`].
    #[error(
        "saved filter is version {found}; this build reads version {}",
        crate::db::query::FILTER_VERSION
    )]
    FilterVersion { found: String },

    #[error("invalid filter: {reason}")]
    InvalidFilter { reason: String },

    #[error("saved query {id} does not exist")]
    NoSuchSavedQuery { id: i64 },

    #[error("a saved query needs a name")]
    SavedQueryNameEmpty,

    #[error("a saved query named '{name}' already exists")]
    SavedQueryNameTaken { name: String },

    #[error("source {source_id} has no sync_state row")]
    MissingSyncState { source_id: i64 },

    /// Phase 2g. `rusqlite` is blocking, so every database step of a poll runs on
    /// `spawn_blocking`. A panic in there must surface as a failed poll that backs the
    /// source off, not as a task that silently stops existing.
    #[error("a background database task failed: {detail}")]
    BackgroundTask { detail: String },

    /// Phase 2g. Resync and Rebuild address a running task, and a source that was
    /// added while the scheduler was down has none. Distinct from a source that does
    /// not exist: the fix is to restart the app, not to check the id.
    #[error("source {source_id} is not being polled")]
    SourceNotScheduled { source_id: i64 },

    #[error("cannot convert {unit} from {from} to {to}")]
    DimensionMismatch {
        unit: &'static str,
        from: Dimension,
        to: Dimension,
    },

    #[error("{field} value is not finite: {value}")]
    NotFinite { field: &'static str, value: f64 },

    #[error("{field} must be greater than zero: {value}")]
    NotPositive { field: &'static str, value: f64 },

    #[error("unknown unit '{0}'")]
    UnknownUnit(String),

    /// Phase 3e. Every formula in [`crate::calc`] is a fit over a stated range, and
    /// outside it the polynomial keeps returning a number that looks like an answer.
    /// Refusing is the only way the caller finds out; `what` names the input so the
    /// message says which of a formula's three arguments was the wrong one.
    #[error("{what} is outside this formula's range: {value}")]
    CalcOutOfRange { what: &'static str, value: f64 },

    #[error("cannot combine {left} and {right}: this app does not convert currencies")]
    CurrencyMismatch { left: Currency, right: Currency },

    #[error("'{0}' is not a three-letter currency code")]
    InvalidCurrency(String),

    #[error("could not read '{0}' as an amount")]
    InvalidMoney(String),

    #[error("amount is too large to represent in minor units")]
    MoneyOverflow,
}

impl Error {
    /// The host did not answer: refused, unroutable, or too slow. Not the host
    /// answering badly, which is every other transport error.
    pub fn is_unreachable(&self) -> bool {
        match self {
            Error::Timeout { .. } => true,
            Error::Http { source, .. } => source.is_connect() || source.is_timeout(),
            _ => false,
        }
    }

    /// A stable identifier the frontend can branch on. Messages are for humans and
    /// may change; these must not.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Sqlite(_) => "db.sqlite",
            Error::Pool(_) => "db.pool",
            Error::Io(_) => "io",
            // `config.io` stays the read code so nothing that already branches on it
            // has to change; a failed write gets its own, because "your settings did
            // not save" and "your settings did not load" need different UI.
            Error::ConfigRead { .. } => "config.io",
            Error::ConfigWrite { .. } => "config.write",
            Error::ConfigParse { .. } => "config.parse",
            Error::ConfigSerialize(_) => "config.serialize",
            Error::ConfigInvalid { .. } => "config.invalid",
            Error::JournalMode { .. } => "db.journal_mode",
            Error::SchemaAhead { .. } => "db.schema_ahead",
            Error::MigrationForeignKeys { .. } => "db.migration_foreign_keys",
            Error::NotAFermentrackLog { .. } => "fermentrack.not_a_log",
            Error::GraphLogRefused { .. } => "fermentrack.graph_log",
            Error::GravityLogUnsupported { .. } => "fermentrack.gravity_log",
            Error::Csv(_) => "fermentrack.csv",
            Error::LogMissing { .. } => "fermentrack.log_missing",
            Error::HttpStatus { .. } => "fermentrack.http_status",
            Error::NotCsv { .. } => "fermentrack.not_csv",
            Error::Http { .. } => "fermentrack.http",
            Error::Timeout { .. } => "fermentrack.timeout",
            Error::RangeMismatch { .. } => "fermentrack.range",
            Error::InvalidUrl { .. } => "fermentrack.url",
            Error::NotFermentrack { .. } => "fermentrack.not_fermentrack",
            Error::NotABrewdJournal { .. } => "brewd.not_a_journal",
            Error::IspindelUnitUnset { .. } => "brewd.ispindel_unit_unset",
            Error::NotBrewd { .. } => "brewd.not_brewd",
            Error::BrewdUnconfigured => "brewd.unconfigured",
            Error::UnknownMetric(_) => "db.unknown_metric",
            Error::UnknownSourceKind(_) => "db.unknown_source_kind",
            Error::UnknownVesselKind(_) => "db.unknown_vessel_kind",
            Error::UnknownCategory(_) => "db.unknown_category",
            Error::UnknownYeastForm(_) => "db.unknown_yeast_form",
            Error::NoSuchIngredient { .. } => "db.no_such_ingredient",
            Error::UnknownBeverageType(_) => "db.unknown_beverage_type",
            Error::UnknownStage(_) => "db.unknown_stage",
            Error::NoSuchRecipeVersion { .. } => "db.no_such_recipe_version",
            Error::NoSuchRecipeItem { .. } => "db.no_such_recipe_item",
            Error::RecipeVersionLocked { .. } => "db.recipe_version_locked",
            Error::UnknownBatchStatus(_) => "db.unknown_batch_status",
            Error::NoSuchBatch { .. } => "db.no_such_batch",
            Error::NoSuchLot { .. } => "db.no_such_lot",
            Error::IllegalBatchTransition { .. } => "db.illegal_batch_transition",
            Error::BatchNotPitched { .. } => "db.batch_not_pitched",
            Error::BatchArchived { .. } => "db.batch_archived",
            Error::BeverageTypeMismatch { .. } => "db.beverage_type_mismatch",
            Error::UnknownEventKind(_) => "db.unknown_event_kind",
            Error::UnknownAnnotationChannel(_) => "db.unknown_annotation_channel",
            Error::NoSuchBatchEvent { .. } => "db.no_such_batch_event",
            Error::NoSuchBatchStream { .. } => "db.no_such_batch_stream",
            Error::UsePitchRecorder { .. } => "db.use_pitch_recorder",
            Error::PitchEventLocked { .. } => "db.pitch_event_locked",
            Error::StreamAlreadyBound { .. } => "db.stream_already_bound",
            Error::UnknownInstrument(_) => "db.unknown_instrument",
            Error::UnknownEntryUnit(_) => "db.unknown_entry_unit",
            Error::NoSuchMeasurement { .. } => "db.no_such_measurement",
            Error::MeasurementExists { .. } => "db.measurement_exists",
            Error::MeasurementUnitMismatch { .. } => "db.measurement_unit_mismatch",
            Error::MeasurementInstrumentMismatch { .. } => "db.measurement_instrument_mismatch",
            Error::MeasurementImplausible { .. } => "db.measurement_implausible",
            Error::NoOriginalGravity { .. } => "db.no_original_gravity",
            Error::CompareCount { .. } => "compare.batch_count",
            Error::ExportWrite { .. } => "export.write",
            Error::FilterVersion { .. } => "query.filter_version",
            Error::InvalidFilter { .. } => "query.invalid_filter",
            Error::NoSuchSavedQuery { .. } => "db.no_such_saved_query",
            Error::SavedQueryNameEmpty => "query.name_empty",
            Error::SavedQueryNameTaken { .. } => "query.name_taken",
            Error::MissingSyncState { .. } => "sync.missing_state",
            Error::SourceNotScheduled { .. } => "sync.not_scheduled",
            Error::BackgroundTask { .. } => "sync.task",
            Error::DimensionMismatch { .. } => "units.dimension_mismatch",
            Error::NotFinite { .. } => "units.not_finite",
            Error::NotPositive { .. } => "units.not_positive",
            Error::UnknownUnit(_) => "units.unknown_unit",
            Error::CalcOutOfRange { .. } => "calc.out_of_range",
            Error::CurrencyMismatch { .. } => "money.currency_mismatch",
            Error::InvalidCurrency(_) => "money.invalid_currency",
            Error::InvalidMoney(_) => "money.invalid",
            Error::MoneyOverflow => "money.overflow",
        }
    }
}
