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
        "batch",
        "batch_ingredient",
        "batch_event",
        "batch_stream",
        "annotation",
        "manual_measurement",
        "vessel_stream",
        "saved_query",
    ] {
        assert!(tables.contains(&expected.to_string()), "missing {expected}");
    }
}

/// Every intermediate version must reach head. With one migration this is trivial;
/// the loop exists so migration 0002 is covered the day it lands.
#[test]
fn every_version_migrates_to_head() {
    for version in migrations::MIGRATIONS.iter().map(|m| m.version) {
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
fn every_connection_gets_the_pragmas() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();

    // Three of the four are per-connection and reset on every open, so the writer
    // and a pooled reader are checked separately.
    let on_writer = db.write(|conn| pragmas(conn)).unwrap();
    let on_reader = db.read(pragmas).unwrap();

    for (who, (journal, synchronous, foreign_keys, busy_timeout_ms)) in
        [("writer", on_writer), ("reader", on_reader)]
    {
        assert_eq!(journal.to_lowercase(), "wal", "{who}");
        assert_eq!(synchronous, 1, "{who}: synchronous=NORMAL");
        assert_eq!(
            foreign_keys, 1,
            "{who}: foreign_keys defaults to OFF on every connection"
        );
        assert_eq!(busy_timeout_ms, 5000, "{who}");
    }
}

fn pragmas(conn: &Connection) -> brewcore::Result<(String, i32, i32, i32)> {
    Ok((
        conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
        conn.query_row("PRAGMA synchronous", [], |r| r.get(0))?,
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?,
        conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?,
    ))
}

/// Phase 2g: a sync never blocks a UI query. The writer is held mid-transaction
/// with an uncommitted row while a read runs, which fails if reads are ever routed
/// through the writer's lock. WAL itself is pinned by the pragma test above.
#[test]
fn a_read_is_not_blocked_by_an_open_write_transaction() {
    use std::{sync::mpsc, thread, time::Duration};

    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();
    fn count_sources(conn: &Connection) -> brewcore::Result<i64> {
        Ok(conn.query_row("SELECT COUNT(*) FROM source", [], |r| r.get(0))?)
    }

    let (in_txn, wait_in_txn) = mpsc::channel();
    let (release, wait_release) = mpsc::channel::<()>();
    let writer = {
        let db = db.clone();
        thread::spawn(move || {
            db.write(|conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO source (kind, name) VALUES ('manual', 'held open')",
                    [],
                )?;
                in_txn.send(()).unwrap();
                wait_release.recv().unwrap();
                tx.commit()?;
                Ok(())
            })
        })
    };

    wait_in_txn.recv().unwrap();
    // On its own thread with a deadline, so a read stuck behind the writer fails the
    // test instead of hanging it. Well under the 5 s busy_timeout.
    let (read_done, wait_read) = mpsc::channel();
    let reader = {
        let db = db.clone();
        thread::spawn(move || read_done.send(db.read(count_sources)).unwrap())
    };
    let during = wait_read.recv_timeout(Duration::from_secs(1));
    release.send(()).unwrap();
    writer.join().unwrap().unwrap();
    reader.join().unwrap();

    let during = during.expect("the read waited on the open write");
    assert_eq!(
        during.unwrap(),
        0,
        "a reader sees the last commit, not the open write"
    );
    assert_eq!(db.read(count_sources).unwrap(), 1);
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

/// 0001 closed `source.kind` with a `CHECK`; 0007 removed it. The vocabulary is
/// `SourceKind`'s to enforce now, and the database takes any text.
#[test]
fn source_kind_is_no_longer_checked_by_the_schema() {
    let (_dir, path) = scratch();
    let db = Db::open(&path).unwrap();

    db.write(|conn| {
        conn.execute(
            "INSERT INTO source (kind, name) VALUES ('brewd_journal', 'ESP32 #1')",
            [],
        )?;
        conn.execute(
            "INSERT INTO source (kind, name) VALUES ('not_a_kind', 'nope')",
            [],
        )?;
        Ok(())
    })
    .expect("the schema no longer refuses a kind");

    assert_eq!(brewcore::db::SourceKind::parse("not_a_kind"), None);
}

/// The rebuild path, against a file with rows on both sides of every foreign key
/// that points at `source`. The rows must survive, the keys must still be enforced
/// afterwards, and the connection must get its `foreign_keys` setting back.
#[test]
fn rebuilding_source_keeps_every_row_that_references_it() {
    let (_dir, path) = scratch();
    let mut conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrations::apply_upto(&mut conn, 6).unwrap();

    conn.execute_batch(
        "INSERT INTO source (id, kind, name, config)            VALUES (1, 'fermentrack_beer_log', 'Fermenter 1', '{\"base_url\":\"http://x\"}'),                   (2, 'manual', 'Bench', NULL);          INSERT INTO stream (id, source_id, external_id, metric)            VALUES (10, 1, 'beer_temp', 'temp_c'), (20, 2, 'manual/1/sg', 'sg');          INSERT INTO reading (stream_id, ts, value) VALUES (10, 100, 18.5), (20, 100, 1.040);          INSERT INTO sync_state (source_id, remote_path, byte_offset, header, synced_at)            VALUES (1, 'data/log_full.csv', 4096, 'log_time,beer_temp', 100);          INSERT INTO annotation (source_id, ts, channel, text)            VALUES (1, 100, 'vessel', 'dry hop');",
    )
    .unwrap();

    migrations::migrate(&mut conn).unwrap();
    assert_eq!(
        migrations::current_version(&conn).unwrap(),
        migrations::head()
    );

    let enforced: bool = conn
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert!(enforced, "the runner must restore foreign_keys");

    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count("source"), 2);
    assert_eq!(count("stream"), 2);
    assert_eq!(count("reading"), 2);
    assert_eq!(count("sync_state"), 1);
    assert_eq!(count("annotation"), 1);

    let (kind, config): (String, Option<String>) = conn
        .query_row("SELECT kind, config FROM source WHERE id = 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "fermentrack_beer_log");
    assert_eq!(config.as_deref(), Some("{\"base_url\":\"http://x\"}"));

    let dangling: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(dangling, 0);

    // The references now bind to the rebuilt table, and are still enforced.
    let err = conn
        .execute(
            "INSERT INTO stream (source_id, external_id, metric) VALUES (404, 'x', 'temp_c')",
            [],
        )
        .expect_err("dangling source_id must still be rejected");
    assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
}

/// A rebuild that would commit a dangling reference is refused and rolled back,
/// leaving the file at the version before it.
#[test]
fn a_rebuild_that_orphans_rows_is_rolled_back() {
    let (_dir, path) = scratch();
    let mut conn = Connection::open(&path).unwrap();
    migrations::apply_upto(&mut conn, 6).unwrap();

    // An orphan that predates the migration, written with keys off as a hand-edited
    // or pre-pragma file could have. The rebuild must not bless it. (Explicitly off:
    // the bundled SQLite defaults it on.)
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    conn.execute(
        "INSERT INTO stream (source_id, external_id, metric) VALUES (404, 'x', 'temp_c')",
        [],
    )
    .unwrap();

    let err = migrations::migrate(&mut conn).expect_err("orphan must fail the check");
    assert_eq!(err.code(), "db.migration_foreign_keys");
    assert_eq!(migrations::current_version(&conn).unwrap(), 6);

    let kept_check: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'source'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(kept_check.contains("CHECK"), "rolled back to 0001's table");
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
