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

    #[error("could not enable WAL on {path}: journal_mode is '{mode}'")]
    JournalMode { path: PathBuf, mode: String },

    #[error(
        "database is at schema version {found}, but this build only knows up to \
         {head}; a newer version of the app wrote this file"
    )]
    SchemaAhead { found: i32, head: i32 },

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
            Error::JournalMode { .. } => "db.journal_mode",
            Error::SchemaAhead { .. } => "db.schema_ahead",
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
