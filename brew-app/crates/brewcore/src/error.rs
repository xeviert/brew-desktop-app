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

    // Phase 2b. File-level problems only: a single bad row is warned about and
    // skipped (see `fermentrack::ParseReport`), because one malformed line must not
    // cost a week of backfill. A bad *file* is fatal, because half-ingesting one
    // writes wrong numbers that look right.
    #[error("this does not look like a Fermentrack log: {detail}")]
    NotAFermentrackLog { detail: String },

    #[error(
        "{path_hint} is a _graph.csv; use _full.csv instead. The graph file writes          0 for a missing reading, which would be stored as a real 0.0 °C"
    )]
    GraphLogRefused { path_hint: String },

    // A gravity sensor's own log, not a beer log. `source.kind` has carried
    // 'fermentrack_gravity_log' since 0001, but nothing reads one yet, and saying so
    // is the difference between a user fixing the URL and a user filing a bug.
    #[error("{path_hint} is a gravity-sensor log; this app only ingests a beer _full.csv so far")]
    GravityLogUnsupported { path_hint: String },

    #[error("could not read the log as CSV: {0}")]
    Csv(#[from] csv::Error),

    #[error("unknown metric '{0}' in the database")]
    UnknownMetric(String),

    #[error("cannot convert {unit} from {from} to {to}")]
    DimensionMismatch {
        unit: &'static str,
        from: Dimension,
        to: Dimension,
    },

    #[error("{field} value is not finite: {value}")]
    NotFinite { field: &'static str, value: f64 },

    #[error("unknown unit '{0}'")]
    UnknownUnit(String),

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
            Error::NotAFermentrackLog { .. } => "fermentrack.not_a_log",
            Error::GraphLogRefused { .. } => "fermentrack.graph_log",
            Error::GravityLogUnsupported { .. } => "fermentrack.gravity_log",
            Error::Csv(_) => "fermentrack.csv",
            Error::UnknownMetric(_) => "db.unknown_metric",
            Error::DimensionMismatch { .. } => "units.dimension_mismatch",
            Error::NotFinite { .. } => "units.not_finite",
            Error::UnknownUnit(_) => "units.unknown_unit",
            Error::CurrencyMismatch { .. } => "money.currency_mismatch",
            Error::InvalidCurrency(_) => "money.invalid_currency",
            Error::InvalidMoney(_) => "money.invalid",
            Error::MoneyOverflow => "money.overflow",
        }
    }
}
