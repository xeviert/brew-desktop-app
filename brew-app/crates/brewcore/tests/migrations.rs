//! Schema tests. No Tauri, no network — plain `cargo test`.

use brewcore::db::{Db, migrations};
use rusqlite::Connection;

fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("brewery.db");
    (dir, path)
}

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn empty_database_migrates_to_head() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).expect("open");

    assert_eq!(db.schema_version().unwrap(), migrations::head());

    let tables = db.read(|conn| Ok(table_names(conn))).unwrap();
    for expected in [
        "source",
        "stream",
        "reading",
        "sync_state",
        "vessel",
        "yeast_strain",
        "ingredient",
        "ingredient_lot",
        "water_profile",
        "recipe",
        "recipe_version",
        "recipe_item",
    ] {
        assert!(tables.contains(&expected.to_string()), "missing {expected}");
    }
}

/// Every intermediate version must reach head. With one migration this is trivial;
/// the loop exists so migration 0002 is covered the day it lands.
#[test]
fn every_version_migrates_to_head() {
    for &(version, _) in migrations::MIGRATIONS {
        let (_dir, path) = scratch();
        let mut conn = Connection::open(&path).unwrap();
        migrations::apply_upto(&mut conn, version).unwrap();
        assert_eq!(migrations::current_version(&conn).unwrap(), version);
        drop(conn);

        let db = Db::open(&path).expect("open at intermediate version");
        assert_eq!(db.schema_version().unwrap(), migrations::head());
    }
}

#[test]
fn migrating_twice_is_a_no_op() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();
    let first = db.status().unwrap();
    drop(db);

    let db = Db::open(&path).unwrap();
    let second = db.status().unwrap();

    assert_eq!(first.schema_version, second.schema_version);
    assert_eq!(first.row_counts, second.row_counts);
}

/// A file written by a newer build must be refused, not silently downgraded.
#[test]
fn a_newer_schema_is_refused() {
    let (_dir, path) = scratch();
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", migrations::head() + 1)
        .unwrap();
    drop(conn);

    let err = Db::open(&path).expect_err("should refuse a future schema");
    assert_eq!(err.code(), "db.schema_ahead");
}

/// The pragmas that matter are per-connection, so the pool customiser is the thing
/// actually under test here — not the writer.
#[test]
fn pooled_readers_get_the_pragmas() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();

    let (journal, foreign_keys): (String, i32) = db
        .read(|conn| {
            Ok((
                conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
                conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?,
            ))
        })
        .unwrap();

    assert_eq!(journal.to_lowercase(), "wal");
    assert_eq!(
        foreign_keys, 1,
        "foreign_keys defaults to OFF on every connection"
    );
}

#[test]
fn foreign_keys_are_enforced_on_writes() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();

    let err = db
        .write(|conn| {
            conn.execute(
                "INSERT INTO stream (source_id, external_id, metric) VALUES (404, 'x', 'temp_c')",
                [],
            )?;
            Ok(())
        })
        .expect_err("dangling source_id must be rejected");

    assert!(
        err.to_string().contains("FOREIGN KEY"),
        "unexpected error: {err}"
    );
}

#[test]
fn source_kind_is_a_closed_set() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();

    db.write(|conn| {
        conn.execute(
            "INSERT INTO source (kind, name) VALUES ('manual', 'Bench readings')",
            [],
        )?;
        Ok(())
    })
    .expect("'manual' is valid");

    db.write(|conn| {
        conn.execute(
            "INSERT INTO source (kind, name) VALUES ('beer', 'nope')",
            [],
        )?;
        Ok(())
    })
    .expect_err("unknown kind must be rejected");
}

/// `reading` is WITHOUT ROWID with a composite PK; Phase 2's ingest leans on the
/// resulting upsert being a cheap no-op.
#[test]
fn duplicate_readings_are_ignored_not_duplicated() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();

    let count = db
        .write(|conn| {
            conn.execute(
                "INSERT INTO source (id, kind, name) VALUES (1, 'manual', 'Bench')",
                [],
            )?;
            conn.execute(
                "INSERT INTO stream (id, source_id, external_id, metric) \
                 VALUES (1, 1, 'bench', 'temp_c')",
                [],
            )?;
            for _ in 0..2 {
                conn.execute(
                    "INSERT INTO reading (stream_id, ts, value) VALUES (1, 1700000000, 18.5) \
                     ON CONFLICT(stream_id, ts) DO NOTHING",
                    [],
                )?;
            }
            Ok(conn.query_row("SELECT COUNT(*) FROM reading", [], |r| r.get::<_, i64>(0))?)
        })
        .unwrap();

    assert_eq!(count, 1);
}
