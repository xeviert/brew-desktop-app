use rusqlite::Connection;

use crate::{Error, Result};

/// One schema step.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i32,
    pub sql: &'static str,
    /// The migration rebuilds a table other tables reference — SQLite's
    /// create-copy-drop-rename, the only way to change a `CHECK`.
    ///
    /// Declared, never inferred from the SQL. With `foreign_keys=ON` the `DROP TABLE`
    /// of a referenced table fails or cascades, and the pragma cannot be turned off
    /// from inside the transaction every migration runs in — it silently does nothing
    /// there. So a rebuild runs with foreign keys off from *before* `BEGIN`, and
    /// `PRAGMA foreign_key_check` must come back empty before it commits.
    pub rebuilds_tables: bool,
}

const fn plain(version: i32, sql: &'static str) -> Migration {
    Migration {
        version,
        sql,
        rebuilds_tables: false,
    }
}

const fn rebuild(version: i32, sql: &'static str) -> Migration {
    Migration {
        version,
        sql,
        rebuilds_tables: true,
    }
}

/// Applied in order, each inside a transaction that also bumps `user_version`.
///
/// **Append-only.** Once a migration is committed it is never edited: the database
/// it already ran against will not re-run it. Fix a mistake with a new migration.
/// No migration may depend on application code, because by the time it runs against
/// an old database the code will have moved on.
pub const MIGRATIONS: &[Migration] = &[
    plain(1, include_str!("../../migrations/0001_tier1.sql")),
    plain(2, include_str!("../../migrations/0002_reference.sql")),
    plain(3, include_str!("../../migrations/0003_recipe.sql")),
    plain(4, include_str!("../../migrations/0004_batch.sql")),
    plain(5, include_str!("../../migrations/0005_timeline.sql")),
    plain(6, include_str!("../../migrations/0006_manual.sql")),
    rebuild(7, include_str!("../../migrations/0007_source_kind.sql")),
    plain(8, include_str!("../../migrations/0008_vessel_stream.sql")),
    plain(9, include_str!("../../migrations/0009_saved_query.sql")),
];

/// The schema version this build knows about.
pub fn head() -> i32 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

/// The version currently recorded in the file. `0` for a fresh database.
pub fn current_version(conn: &Connection) -> Result<i32> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Apply every migration up to and including `target`, returning the resulting
/// version. A no-op if the file is already there.
///
/// Each migration commits with its own `user_version` bump, so a process that dies
/// mid-run leaves the file at N or N+1 — never between.
pub fn apply_upto(conn: &mut Connection, target: i32) -> Result<i32> {
    debug_assert!(
        MIGRATIONS
            .iter()
            .enumerate()
            .all(|(i, m)| m.version == i as i32 + 1),
        "MIGRATIONS must be contiguous and sorted from 1"
    );

    let head = head();
    let current = current_version(conn)?;
    if current > head {
        return Err(Error::SchemaAhead {
            found: current,
            head,
        });
    }

    for migration in MIGRATIONS
        .iter()
        .filter(|m| m.version > current && m.version <= target)
    {
        if migration.rebuilds_tables {
            apply_rebuild(conn, migration)?;
        } else {
            apply_in_transaction(conn, migration)?;
        }
    }

    current_version(conn)
}

/// Bring the database to [`head`].
pub fn migrate(conn: &mut Connection) -> Result<i32> {
    apply_upto(conn, head())
}

fn apply_in_transaction(conn: &mut Connection, migration: &Migration) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(migration.sql)?;
    // `PRAGMA user_version` takes no bound parameter, so the value is inlined
    // into the statement — `pragma_update` handles that, and rejects anything
    // that is not a literal. `version` comes from the table above regardless.
    tx.pragma_update(None, "user_version", migration.version)?;
    tx.commit()?;
    Ok(())
}

/// See [`Migration::rebuilds_tables`]. The connection's own `foreign_keys` setting is
/// put back on every path, error included: this runs on the app's one writer
/// connection, and leaving it OFF would unenforce every foreign key until restart.
fn apply_rebuild(conn: &mut Connection, migration: &Migration) -> Result<()> {
    let enforced: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = rebuild_with_keys_off(conn, migration);
    conn.pragma_update(None, "foreign_keys", enforced)?;
    result
}

fn rebuild_with_keys_off(conn: &mut Connection, migration: &Migration) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(migration.sql)?;

    let violations = {
        let mut stmt = tx.prepare("PRAGMA foreign_key_check")?;
        let mut rows = stmt.query([])?;
        let mut count = 0;
        while rows.next()?.is_some() {
            count += 1;
        }
        count
    };
    if violations > 0 {
        // Dropping `tx` rolls it back.
        return Err(Error::MigrationForeignKeys {
            version: migration.version,
            violations,
        });
    }

    tx.pragma_update(None, "user_version", migration.version)?;
    tx.commit()?;
    Ok(())
}
