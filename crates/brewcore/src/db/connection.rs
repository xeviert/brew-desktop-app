use std::{path::Path, time::Duration};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

use crate::{Error, Result};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Apply the pragmas every connection needs, returning the journal mode in force.
///
/// Two things make this easy to get wrong:
///
/// * `PRAGMA journal_mode` *returns a row*, so it cannot go through
///   `pragma_update` — that uses `execute` and fails with
///   `ExecuteReturnedResults`. It has to be queried.
/// * Of these four, only `journal_mode` persists in the file. `synchronous`,
///   `foreign_keys` and `busy_timeout` are per-connection and reset to their
///   defaults on every open — and `foreign_keys` defaults to OFF, so skipping this
///   on one connection means silently unenforced foreign keys on that connection.
///
/// Neither `journal_mode` nor `foreign_keys` can be changed inside a transaction
/// (the latter silently does nothing), so this runs immediately on open.
fn apply_pragmas(conn: &Connection) -> rusqlite::Result<String> {
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(mode)
}

/// Open the single writer connection and verify WAL actually took.
///
/// WAL is what lets a chart query run while the sync task inserts. If the file
/// cannot be put into WAL — a network share, most commonly — failing loudly here
/// beats discovering it as `SQLITE_BUSY` under load in Phase 2.
pub(super) fn open_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    let mode = apply_pragmas(&conn)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::JournalMode {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(conn)
}

/// Reader pool. `journal_mode` is already WAL in the file by the time this runs,
/// so the customiser exists for the three per-connection pragmas.
pub(super) fn build_pool(path: &Path) -> Result<r2d2::Pool<SqliteConnectionManager>> {
    let manager = SqliteConnectionManager::file(path).with_flags(
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    );
    Ok(r2d2::Pool::builder()
        .connection_customizer(Box::new(Pragmas))
        .build(manager)?)
}

#[derive(Debug)]
struct Pragmas;

impl r2d2::CustomizeConnection<Connection, rusqlite::Error> for Pragmas {
    fn on_acquire(&self, conn: &mut Connection) -> rusqlite::Result<()> {
        apply_pragmas(conn).map(|_| ())
    }
}
