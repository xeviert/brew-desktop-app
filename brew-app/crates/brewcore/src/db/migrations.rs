use rusqlite::Connection;

use crate::{Error, Result};

/// Applied in order, each inside a transaction that also bumps `user_version`.
///
/// **Append-only.** Once a migration is committed it is never edited: the database
/// it already ran against will not re-run it. Fix a mistake with a new migration.
/// No migration may depend on application code, because by the time it runs against
/// an old database the code will have moved on.
pub const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("../../migrations/0001_tier1.sql")),
    (2, include_str!("../../migrations/0002_reference.sql")),
    (3, include_str!("../../migrations/0003_recipe.sql")),
    (4, include_str!("../../migrations/0004_batch.sql")),
    (5, include_str!("../../migrations/0005_timeline.sql")),
    (6, include_str!("../../migrations/0006_manual.sql")),
];

/// The schema version this build knows about.
pub fn head() -> i32 {
    MIGRATIONS.last().map_or(0, |&(version, _)| version)
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
            .all(|(i, &(version, _))| version == i as i32 + 1),
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

    for &(version, sql) in MIGRATIONS
        .iter()
        .filter(|&&(version, _)| version > current && version <= target)
    {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        // `PRAGMA user_version` takes no bound parameter, so the value is inlined
        // into the statement — `pragma_update` handles that, and rejects anything
        // that is not a literal. `version` comes from the table above regardless.
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }

    current_version(conn)
}

/// Bring the database to [`head`].
pub fn migrate(conn: &mut Connection) -> Result<i32> {
    apply_upto(conn, head())
}
