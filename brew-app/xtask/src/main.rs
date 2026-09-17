//! Dev helpers. Phase 2 adds `gen-fixture`, which emits byte-accurate Fermentrack
//! `_full.csv` files so ingest can be built and tested without a Fermentrack.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("db-status") => db_status(args.next().map(PathBuf::from)),
        Some(other) => {
            eprintln!("unknown task: {other}");
            usage();
            std::process::exit(2);
        }
        None => usage(),
    }
}

fn usage() {
    eprintln!(
        "usage: cargo xtask <task>\n\n  db-status [path]   open a database and print its status"
    );
}

/// Opening a database outside the app is the quickest way to confirm migrations
/// run and the pragmas take.
fn db_status(path: Option<PathBuf>) {
    let path = path
        .unwrap_or_else(|| brewcore::db::resolve_db_path(&std::env::current_dir().expect("cwd")));

    match brewcore::db::Db::open(&path).and_then(|db| db.status()) {
        Ok(status) => {
            println!("path           {}", status.path);
            println!("schema version {}", status.schema_version);
            println!("size           {} bytes", status.size_bytes);
            for (table, count) in &status.row_counts {
                println!("  {table:<12} {count}");
            }
        }
        Err(err) => {
            eprintln!("[{}] {err}", err.code());
            std::process::exit(1);
        }
    }
}
