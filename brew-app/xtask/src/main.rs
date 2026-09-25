//! Dev helpers. `gen-fixture` emits byte-accurate Fermentrack `_full.csv` files and
//! `brewd` journals so ingest can be built and tested without either running. `build-pi` cross-compiles
//! `brewd` for the Pi Zero 2 W.

use std::path::{Path, PathBuf};

use brewfixture::{JournalScenario, Kind, LogSpec, Scenario};

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("db-status") => db_status(args.next().map(PathBuf::from)),
        Some("gen-fixture") => gen_fixture(args.collect()),
        Some("build-pi") => build_pi(),
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
        "usage: cargo xtask <task>\n\n  \
         db-status [path]   open a database and print its status\n  \
         gen-fixture [...]  write Fermentrack log fixtures\n  \
         build-pi           cross-compile brewd for the Pi (needs `cross` and Docker)\n\n\
         gen-fixture options:\n  \
         --out DIR          default target/fixtures\n  \
         --scenario NAME    nominal | gravity | ugly | v1 | all (default all)\n  \
         --rows N           rows per file (default 120)\n  \
         --seed N           PRNG seed; the same seed gives the same bytes\n  \
         --interval SECS    logging cadence (default: the scenario's)"
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

/// The Pi Zero 2 W has 512 MB of RAM and cannot link `brewd`, so it is never built
/// on the device. `cross` rather than a bare `--target`: rusqlite's bundled SQLite
/// reaches `brewd` through `brewcore`, and that is C needing an aarch64 toolchain.
fn build_pi() {
    const TARGET: &str = "aarch64-unknown-linux-gnu";
    let status = std::process::Command::new("cross")
        .args(["build", "--package", "brewd", "--release", "--target", TARGET])
        .status()
        .unwrap_or_else(|err| {
            eprintln!(
                "could not run `cross` ({err}). Install it with `cargo install cross`; it needs Docker."
            );
            std::process::exit(1);
        });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("target/{TARGET}/release/brewd");
}

/// Write one `_full.csv` and one `_graph.csv` per scenario, under the filenames
/// Fermentrack would have used, so a fixture can be dropped straight into a `/data/`
/// directory.
fn gen_fixture(args: Vec<String>) {
    let mut out = PathBuf::from("target/fixtures");
    let mut journal = false;
    let mut scenario = String::from("all");
    let mut rows = None;
    let mut seed = None;
    let mut interval = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--format" => {
                journal = match value().as_str() {
                    "fermentrack" => false,
                    "journal" => true,
                    other => fail(&format!("unknown format: {other}")),
                }
            }
            "--out" => out = PathBuf::from(value()),
            "--scenario" => scenario = value(),
            "--rows" => rows = Some(parse(&value(), "--rows")),
            "--seed" => seed = Some(parse(&value(), "--seed")),
            "--interval" => interval = Some(parse(&value(), "--interval")),
            other => fail(&format!("unknown option: {other}")),
        }
    }

    if let Err(err) = std::fs::create_dir_all(&out) {
        fail(&format!("could not create {}: {err}", out.display()));
    }

    if journal {
        let scenarios = if scenario == "all" {
            JournalScenario::ALL.to_vec()
        } else {
            vec![
                JournalScenario::parse(&scenario)
                    .unwrap_or_else(|| fail(&format!("unknown scenario: {scenario}"))),
            ]
        };
        for scenario in scenarios {
            let mut spec = scenario.spec();
            spec.rows = rows.unwrap_or(spec.rows);
            spec.seed = seed.unwrap_or(spec.seed);
            spec.interval_secs = interval.unwrap_or(spec.interval_secs);
            write(&out.join(spec.file_name()), &spec.csv());
        }
        return;
    }

    let scenarios = if scenario == "all" {
        Scenario::ALL.to_vec()
    } else {
        vec![
            Scenario::parse(&scenario)
                .unwrap_or_else(|| fail(&format!("unknown scenario: {scenario}"))),
        ]
    };
    for scenario in scenarios {
        let mut spec = scenario.spec();
        if let Some(rows) = rows {
            spec.rows = rows;
        }
        if let Some(seed) = seed {
            spec.seed = seed;
        }
        if let Some(interval) = interval {
            spec.interval_secs = interval;
        }

        for kind in [Kind::Full, Kind::Graph] {
            write_log(&out, &spec, kind);
        }
    }
}

fn write_log(dir: &Path, spec: &LogSpec, kind: Kind) {
    write(&dir.join(spec.file_name(kind)), &spec.csv(kind));
}

fn write(path: &Path, body: &str) {
    // `write` in binary mode, never a text-mode writer: Windows would turn every
    // `\r\n` this crate emitted into `\r\r\n`, and the bytes are the whole point.
    if let Err(err) = std::fs::write(path, body.as_bytes()) {
        fail(&format!("could not write {}: {err}", path.display()));
    }
    println!("{:>8} bytes  {}", body.len(), path.display());
}

fn parse<T: std::str::FromStr>(value: &str, what: &str) -> T {
    value
        .parse()
        .unwrap_or_else(|_| fail(&format!("{what}: '{value}' is not a number")))
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    usage();
    std::process::exit(2);
}
