//! Core error type. The Tauri layer converts this into a serialisable
//! `{code, message, detail}` at its boundary; nothing here knows that exists.

use std::path::PathBuf;

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
        }
    }
}
