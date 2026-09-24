//! The database handle. Every read and write goes through here.

pub mod batch;
mod connection;
mod enums;
mod metric;
pub mod migrations;
mod paths;
pub mod recipe;
pub mod reference;
pub mod tier1;
pub mod timeline;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

use crate::Result;

pub use metric::Metric;
pub use paths::{DB_PATH_ENV, resolve_db_path};
pub use tier1::{SourceKind, SourceRow};

/// One SQLite file, one writer, a pool of readers.
///
/// SQLite allows exactly one writer at a time. Serialising writes through a single
/// owned connection turns that constraint into a queue instead of a race for the
/// lock; `busy_timeout` is only the backstop for the WAL checkpointer. Readers come
/// from a pool and stay live during a write — that is what WAL buys, and Phase 2
/// depends on it (a chart query must not block on a sync).
///
/// `rusqlite` is synchronous, so callers on an async runtime should wrap
/// [`Db::write`] and [`Db::read`] in `tokio::task::spawn_blocking`. This type
/// deliberately knows nothing about tokio.
#[derive(Clone)]
pub struct Db {
    writer: Arc<Mutex<Connection>>,
    pool: r2d2::Pool<SqliteConnectionManager>,
    path: PathBuf,
}

impl Db {
    /// Open (creating if needed), configure, and migrate to head.
    ///
    /// Migrations run before the reader pool exists, so no reader can observe a
    /// half-migrated schema.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut writer = connection::open_writer(&path)?;
        let version = migrations::migrate(&mut writer)?;
        tracing::info!(path = %path.display(), schema_version = version, "database ready");

        let pool = connection::build_pool(&path)?;
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            pool,
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` against the writer. Blocks until the writer is free.
    pub fn write<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        // A panic inside a previous `write` poisons the mutex, but rusqlite rolls
        // any open transaction back on drop, so the connection is still sound.
        // Refusing every subsequent write would turn one bug into a dead app.
        let mut conn = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut conn)
    }

    /// Run `f` against a pooled reader.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.pool.get()?;
        f(&conn)
    }

    pub fn schema_version(&self) -> Result<i32> {
        self.read(migrations::current_version)
    }

    /// What the status footer needs: where the file is, how big, what version, and
    /// how much is in it.
    pub fn status(&self) -> Result<DbStatus> {
        let schema_version = self.schema_version()?;
        let row_counts = self.read(row_counts)?;
        // The `-wal` and `-shm` sidecars are not counted: they are transient, and a
        // size that jumps around with checkpointing is noise in the UI.
        let size_bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);

        Ok(DbStatus {
            path: self.path.display().to_string(),
            size_bytes,
            schema_version,
            row_counts,
        })
    }
}

/// Only the path: a full dump would be the pool's internals, which is noise.
impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("path", &self.path).finish()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DbStatus {
    pub path: String,
    pub size_bytes: u64,
    pub schema_version: i32,
    pub row_counts: BTreeMap<String, i64>,
}

/// Counted from `sqlite_master` rather than a hardcoded list, so later migrations
/// show up here without anyone remembering to update this function.
fn row_counts(conn: &Connection) -> Result<BTreeMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    tables
        .into_iter()
        .map(|table| {
            // `table` came from sqlite_master, not from a user; quoted anyway
            // because a table name cannot be a bound parameter.
            let count: i64 =
                conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                    row.get(0)
                })?;
            Ok((table, count))
        })
        .collect()
}
