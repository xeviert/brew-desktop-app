//! Dev helpers. `gen-fixture` emits byte-accurate Fermentrack `_full.csv` files and
//! `brewd` journals so ingest can be built and tested without either running. `build-pi` cross-compiles
//! `brewd` for the Pi Zero 2 W. `demo-db` writes one long ferment to look at the chart
//! against.

use std::path::{Path, PathBuf};

use brewcore::{
    db::{
        Db, Metric, SourceKind,
        batch::{NewBatch, create_batch},
        measurement::{EntryUnit, Instrument, NewMeasurement, record_measurement},
        recipe::BeverageType,
        tier1::{create_source, create_stream, insert_readings},
        timeline::{EventKind, NewBatchEvent, add_event, bind_stream, record_pitch},
    },
    units::Timestamp,
};
use brewfixture::{
    JournalScenario, Kind, LogSpec, Scenario,
    curve::{Rng, activity_at, gravity_at},
};

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("db-status") => db_status(args.next().map(PathBuf::from)),
        Some("gen-fixture") => gen_fixture(args.collect()),
        Some("build-pi") => build_pi(),
        Some("demo-db") => demo_db(args.next().map(PathBuf::from)),
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
         build-pi           cross-compile brewd for the Pi (needs `cross` and Docker)\n  \
         demo-db [path]     write a 60-day ferment to chart (default target/demo.db)\n\n\
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

const HOUR: i64 = 3_600;
const DAY: i64 = 24 * HOUR;

/// One seltzer, pitched 60 days ago so the default range is the whole ferment: an
/// ESP32 at 30 s, an iSpindle at 15 min, a few hydrometer and pH readings, the usual
/// events, and three days with the Pi off. 4d's "stays interactive" is judged on this.
///
/// Refuses an existing file rather than replacing it: the path is whatever was typed,
/// and it could be the real database.
fn demo_db(path: Option<PathBuf>) {
    let path = path.unwrap_or_else(|| PathBuf::from("target/demo.db"));
    if path.exists() {
        eprintln!("{} exists; delete it first", path.display());
        std::process::exit(1);
    }
    let db = Db::open(&path).unwrap_or_else(|err| {
        eprintln!("[{}] {err}", err.code());
        std::process::exit(1);
    });

    let now = Timestamp::now().as_secs();
    let pitch = now - now % HOUR - 60 * DAY;
    let at = |offset: i64| Timestamp::from_secs(pitch + offset);
    let hours = |offset: i64| offset as f64 / HOUR as f64;
    let pi_off = 30 * DAY..33 * DAY;
    let cold_crash = 21 * DAY;
    let mut rng = Rng::new(17);

    let written = db.write(|conn| {
        let tx = conn.transaction()?;
        let batch_id = create_batch(
            &tx,
            &NewBatch {
                code: "SELTZER-001".into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: Some("demo: cargo xtask demo-db".into()),
            },
            at(-DAY),
        )?;
        record_pitch(&tx, batch_id, at(0), Some("EC-1118, rehydrated"))?;

        let esp32 = create_source(&tx, SourceKind::BrewdJournal, "esp32-fv1", None)?;
        let ispindel = create_source(&tx, SourceKind::BrewdJournal, "ispindel-1", None)?;
        let streams = [
            (esp32, "vessel_temp", Metric::TempC),
            (esp32, "setpoint", Metric::SetpointC),
            (esp32, "room_temp", Metric::RoomTempC),
            (ispindel, "gravity", Metric::Sg),
            (ispindel, "temperature", Metric::GravityTempC),
            (ispindel, "battery", Metric::BatteryV),
        ]
        .map(|(source, name, metric)| {
            let id = create_stream(&tx, source, name, metric)?;
            bind_stream(&tx, batch_id, id, at(0), None)?;
            Ok::<_, brewcore::Error>(id)
        });
        let [temp, setpoint, room, sg, sg_temp, battery] = streams;
        let (temp, setpoint, room) = (temp?, setpoint?, room?);
        let (sg, sg_temp, battery) = (sg?, sg_temp?, battery?);

        let setpoint_at = |t: i64| if t < cold_crash { 18.0 } else { 2.0 };
        // Runs warm while the yeast works, and lags the setpoint down after the crash.
        let beer_at = |t: i64| {
            if t < cold_crash {
                18.0 + 1.5 * activity_at(hours(t))
            } else {
                2.0 + 16.0 * (-hours(t - cold_crash) / 8.0).exp()
            }
        };
        let mut rows = Vec::new();
        for t in (0..60 * DAY).step_by(30).filter(|t| !pi_off.contains(t)) {
            let day_cycle = (hours(t) / 24.0 * std::f64::consts::TAU).sin();
            rows.push((temp, at(t), beer_at(t) + rng.noise(0.06)));
            rows.push((setpoint, at(t), setpoint_at(t)));
            rows.push((room, at(t), 21.0 + 2.0 * day_cycle + rng.noise(0.1)));
        }
        for t in (0..60 * DAY).step_by(900).filter(|t| !pi_off.contains(t)) {
            // A knock once a week: the spike 4a's min/max must keep and 4b's median
            // must drop.
            let bump = if t % (7 * DAY) == 3 * DAY { 0.012 } else { 0.0 };
            rows.push((sg, at(t), gravity_at(hours(t)) + rng.noise(0.002) + bump));
            rows.push((sg_temp, at(t), beer_at(t) + rng.noise(0.2)));
            rows.push((battery, at(t), 4.1 - 0.3 * hours(t) / (60.0 * 24.0)));
        }
        insert_readings(&tx, &rows)?;

        for (day, value, unit, instrument) in [
            (
                0,
                gravity_at(0.0) + 0.001,
                EntryUnit::Sg,
                Instrument::Hydrometer,
            ),
            (
                3,
                gravity_at(72.0) + 0.001,
                EntryUnit::Sg,
                Instrument::Hydrometer,
            ),
            (
                7,
                gravity_at(168.0) + 0.001,
                EntryUnit::Sg,
                Instrument::Hydrometer,
            ),
            (
                14,
                gravity_at(336.0) + 0.001,
                EntryUnit::Sg,
                Instrument::Hydrometer,
            ),
            (0, 4.6, EntryUnit::Ph, Instrument::PhMeter),
            (2, 3.4, EntryUnit::Ph, Instrument::PhMeter),
            (4, 3.1, EntryUnit::Ph, Instrument::PhMeter),
            (7, 3.2, EntryUnit::Ph, Instrument::PhMeter),
            (14, 3.2, EntryUnit::Ph, Instrument::PhMeter),
        ] {
            record_measurement(
                &tx,
                batch_id,
                at(day * DAY + 10 * HOUR),
                &NewMeasurement {
                    metric: unit.metric(),
                    entry_value: value,
                    entry_unit: unit,
                    instrument: Some(instrument),
                    sample_temp_c: None,
                    calibration_temp_c: None,
                    wort_correction: None,
                },
            )?;
        }

        for (offset, kind, note) in [
            (DAY, EventKind::NutrientAddition, "Fermaid-O, 0.5 g/L"),
            (2 * DAY, EventKind::NutrientAddition, "Fermaid-O, 0.5 g/L"),
            (10 * DAY, EventKind::FlavoringAddition, "lime zest, 2 g/L"),
            (cold_crash, EventKind::ColdCrash, "setpoint 2 °C"),
            (pi_off.start, EventKind::Note, "Pi off for an SD card swap"),
        ] {
            add_event(
                &tx,
                batch_id,
                &NewBatchEvent {
                    ts: at(offset),
                    kind,
                    payload: None,
                    note: Some(note.into()),
                },
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    });

    match written {
        Ok(rows) => {
            println!("{rows} readings  {}", path.display());
            println!(
                "run the app on it with {}={}",
                brewcore::db::DB_PATH_ENV,
                path.display()
            );
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
