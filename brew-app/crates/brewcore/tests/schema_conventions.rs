//! The conventions in `units` are only real if the schema keeps obeying them.
//!
//! This reads a migrated database back through `sqlite_master` / `PRAGMA
//! table_info` and fails on names that break Phase 1c. Today it passes trivially —
//! `0001_tier1.sql` has no money or mass columns. It exists to fail on migration
//! `0002`, the day someone types `cost REAL`.
//!
//! A blocklist, not an allowlist: `og`, `fg` and `ph` are legitimately
//! dimensionless, and an allowlist would be a maintenance tax that gets suppressed
//! the first time it is wrong.

use brewcore::db::Db;
use rusqlite::Connection;

struct Column {
    table: String,
    name: String,
    decl_type: String,
}

fn columns(conn: &Connection) -> Vec<Column> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    let mut out = Vec::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok(Column {
                    table: table.clone(),
                    name: row.get::<_, String>(1)?.to_lowercase(),
                    decl_type: row.get::<_, String>(2)?.to_uppercase(),
                })
            })
            .unwrap();
        for row in rows {
            out.push(row.unwrap());
        }
    }
    out
}

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn check(names: &[String], cols: &[Column]) -> Vec<String> {
    let mut bad = Vec::new();

    // Fermentrack's vocabulary stays quarantined in the `fermentrack` module: this
    // app is multi-beverage, and a `beer_id` column is how that stops being true.
    for name in names {
        if name.to_lowercase().contains("beer") {
            bad.push(format!("object `{name}` is named after beer"));
        }
    }

    let money_tables: Vec<&str> = cols
        .iter()
        .filter(|c| c.name.ends_with("_minor"))
        .map(|c| c.table.as_str())
        .collect();

    for col in cols {
        let Column {
            table,
            name,
            decl_type,
        } = col;
        let at = format!("{table}.{name}");

        if name.contains("beer") {
            bad.push(format!("{at} is named after beer"));
        }

        // Non-canonical units in a column name mean the value was stored in them.
        for suffix in [
            "_f",
            "_fahrenheit",
            "_plato",
            "_brix",
            "_ml",
            "_kg",
            "_oz",
            "_lb",
            "_gal",
        ] {
            if name.ends_with(suffix) {
                bad.push(format!("{at} ends in `{suffix}`; store the canonical unit"));
            }
        }

        // Money is minor units plus an ISO code. Match on name *segments* rather
        // than the whole name, so `unit_cost` and `shipping_cost_per_l` are caught
        // and not just a bare `cost`. `_minor` is the way out, which is why
        // `unit_cost_minor` passes and gets checked by the rules below instead.
        let money_word = name.split('_').any(|w| {
            matches!(
                w,
                "price" | "prices" | "cost" | "costs" | "amount" | "fee" | "fees"
            )
        });
        if (money_word && !name.ends_with("_minor")) || name.ends_with("_cents") {
            bad.push(format!("{at} should be `*_minor INTEGER` + a `currency`"));
        }

        if name.ends_with("_minor") {
            if !decl_type.contains("INT") {
                bad.push(format!("{at} is `{decl_type}`; minor units are INTEGER"));
            }
            if !cols
                .iter()
                .any(|c| c.table == *table && c.name == "currency")
            {
                bad.push(format!("{at} has no `currency` column on `{table}`"));
            }
        }

        if name == "currency" && !money_tables.contains(&table.as_str()) {
            bad.push(format!("{at} has no `*_minor` column to describe"));
        }

        // Epoch seconds. Text timestamps sort correctly only by convention and cost
        // a parse on every comparison.
        let is_time = name == "ts" || name.ends_with("_ts") || name.ends_with("_at");
        if is_time && !decl_type.contains("INT") {
            bad.push(format!("{at} is `{decl_type}`; timestamps are INTEGER"));
        }

        // Canonical °C / litres / grams.
        let is_canonical_float =
            name.ends_with("_c") || name.ends_with("_l") || name.ends_with("_g");
        if is_canonical_float && !decl_type.contains("REAL") {
            bad.push(format!("{at} is `{decl_type}`; canonical units are REAL"));
        }
    }

    bad
}

#[test]
fn schema_follows_the_phase_1c_conventions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path().join("brewery.db")).expect("open");
    let (names, cols) = db
        .read(|conn| Ok((table_names(conn), columns(conn))))
        .unwrap();

    let bad = check(&names, &cols);
    assert!(
        bad.is_empty(),
        "schema convention violations:\n  {}",
        bad.join("\n  ")
    );
}

/// A linter that cannot fail is decoration. This builds the schema nobody should
/// write and asserts each rule fires.
#[test]
fn the_rules_actually_catch_violations() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE beer_log (id INTEGER PRIMARY KEY);
         CREATE TABLE offender (
           id          INTEGER PRIMARY KEY,
           beer_id     INTEGER,
           temp_f      REAL,
           qty_kg      REAL,
           cost        REAL,
           unit_cost   REAL,
           shipping_fee REAL,
           total_cents INTEGER,
           price_minor REAL,
           logged_at   TEXT,
           volume_l    TEXT
         );
         CREATE TABLE lonely_currency (currency TEXT);
         CREATE TABLE innocent (
           unit_cost_minor INTEGER,
           currency        TEXT,
           costume_note    TEXT,
           total_volume_l  REAL
         );",
    )
    .unwrap();

    let bad = check(&table_names(&conn), &columns(&conn));
    let fired = |needle: &str| bad.iter().any(|b| b.contains(needle));

    assert!(fired("`beer_log` is named after beer"));
    assert!(fired("offender.beer_id is named after beer"));
    assert!(fired("offender.temp_f ends in `_f`"));
    assert!(fired("offender.qty_kg ends in `_kg`"));
    assert!(fired("offender.cost should be"));
    assert!(fired("offender.unit_cost should be"));
    assert!(fired("offender.shipping_fee should be"));
    assert!(fired("offender.total_cents should be"));
    assert!(fired("offender.price_minor is `REAL`"));
    assert!(fired("offender.price_minor has no `currency` column"));
    assert!(fired("offender.logged_at is `TEXT`"));
    assert!(fired("offender.volume_l is `TEXT`"));
    assert!(fired("lonely_currency.currency has no `*_minor`"));

    // A blocklist that fires on the correct schema is worse than none — the next
    // person suppresses it. `costume_note` is the substring-match trap.
    let innocent: Vec<&String> = bad.iter().filter(|b| b.contains("innocent.")).collect();
    assert!(innocent.is_empty(), "false positives: {innocent:?}");
}
