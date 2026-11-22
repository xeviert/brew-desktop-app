//! Dev helpers. `gen-fixture` emits byte-accurate Fermentrack `_full.csv` files and
//! `brewd` journals so ingest can be built and tested without either running. `build-pi` cross-compiles
//! `brewd` for the Pi Zero 2 W. `demo-db` writes one long ferment to look at the chart
//! against, with a few more batches beside it for 5b's comparison overlay, 5c's
//! queries and 5d's strain rollup.

use std::path::{Path, PathBuf};

use brewcore::{
    db::{
        Db, Metric, SourceKind,
        batch::{
            BatchStatus, NewBatch, NewBatchIngredient, add_batch_ingredient, archive_batch,
            create_batch, set_batch_measurements, transition_batch,
        },
        cleaning::{CleaningKind, NewCleaning, add_cleaning},
        inventory::set_remaining,
        measurement::{EntryUnit, Instrument, NewMeasurement, record_measurement},
        packaging::{NewPackaging, PackageFormat, package_batch},
        recipe::{
            BeverageType, NewRecipeItem, NewRecipeVersion, Stage, add_recipe_item, create_recipe,
            create_recipe_version,
        },
        reference::{
            Category, NewIngredient, NewLot, NewYeastStrain, VesselKind, YAN_ATTR,
            create_ingredient, create_lot, create_vessel, create_yeast_strain,
        },
        tier1::{create_source, create_stream, insert_readings},
        timeline::{EventKind, NewBatchEvent, add_event, bind_stream, record_pitch},
    },
    units::{Currency, Litres, Money, Quantity, Sg, Timestamp, Unit},
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
/// The iSpindle's battery runs down and it goes quiet for the last 12 h, so 4f's home
/// screen has a silent sensor and a "battery likely" guess to show.
///
/// Beside it, for 5b: two beers and a cider pitched at other times and gravities (one
/// beer archived, its streams unbound when it finished), and a planned batch with no
/// pitch. The seltzer has no OG, so it is also the overlay's raw fallback.
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
        // A strain, its ingredient, and one lot; returns the ingredient and the lot.
        let yeast_lot = |name: &str, lab: &str, band: (f64, f64)| -> brewcore::Result<(i64, i64)> {
            let strain = create_yeast_strain(
                &tx,
                &NewYeastStrain {
                    name: name.into(),
                    lab: Some(lab.into()),
                    attenuation_min: Some(band.0),
                    attenuation_max: Some(band.1),
                    ..Default::default()
                },
            )?;
            let ingredient = create_ingredient(
                &tx,
                &NewIngredient {
                    name: name.into(),
                    category: Category::Yeast,
                    default_unit: Unit::G,
                    yeast_strain_id: Some(strain),
                    attrs: None,
                    notes: None,
                },
            )?;
            let lot = create_lot(
                &tx,
                &NewLot {
                    ingredient_id: ingredient,
                    supplier: None,
                    lot_code: None,
                    entry: Quantity::new(500.0, Unit::G)?,
                    cost: None,
                    purchased_at: None,
                    expires_at: None,
                    notes: None,
                },
            )?;
            Ok((ingredient, lot))
        };
        let pitch_yeast = |batch_id: i64, lot: i64| {
            add_batch_ingredient(
                &tx,
                batch_id,
                &NewBatchIngredient {
                    ingredient_lot_id: lot,
                    entry: Quantity::new(5.0, Unit::G)?,
                    stage: Stage::Pitch,
                    added_at: None,
                    notes: None,
                },
            )
        };
        // A seltzer ferments past a band written for grape must, so EC-1118's row is
        // flagged; US-05's sits inside its datasheet.
        let (ec1118_ingredient, ec1118) = yeast_lot("EC-1118", "Lallemand", (0.90, 1.00))?;
        let (_, us05) = yeast_lot("US-05", "Fermentis", (0.78, 0.82))?;

        // U2: the seltzers are brewed off v1 of a recipe, so v1 is locked and the
        // recipe screen has something to fork. The nutrient lines match SELTZER-001's
        // Fermaid-O events below.
        let plain = |name: &str, category: Category, unit: Unit, attrs: Option<String>| {
            create_ingredient(
                &tx,
                &NewIngredient {
                    name: name.into(),
                    category,
                    default_unit: unit,
                    yeast_strain_id: None,
                    attrs,
                    notes: None,
                },
            )
        };
        let sucrose = plain("Sucrose", Category::Sugar, Unit::Kg, None)?;
        // TOSNA's figure, the lower of the two `calc::yan` offers.
        let fermaid = plain(
            "Fermaid O",
            Category::Nutrient,
            Unit::G,
            Some(format!("{{\"{YAN_ATTR}\": 40.0}}")),
        )?;
        // U5: priced lots, so SELTZER-001 has a cost and a plan-vs-actual to read.
        let priced = |ingredient: i64, qty: Quantity, minor: i64, code: &str| {
            create_lot(
                &tx,
                &NewLot {
                    ingredient_id: ingredient,
                    supplier: Some("Metro".into()),
                    lot_code: Some(code.into()),
                    entry: qty,
                    cost: Some(Money::new(minor, Currency::EUR)),
                    purchased_at: Some(at(-7 * DAY)),
                    expires_at: None,
                    notes: None,
                },
            )
        };
        let sucrose_lot = priced(sucrose, Quantity::new(5.0, Unit::Kg)?, 1250, "SUC-2608")?;
        let fermaid_lot = priced(fermaid, Quantity::new(250.0, Unit::G)?, 1890, "FO-0412")?;
        // 7c: a second bag for SELTZER-007, and the first one counted short.
        let sucrose_bulk = priced(sucrose, Quantity::new(10.0, Unit::Kg)?, 2200, "SUC-2610")?;
        // 7b: FV1 holds SELTZER-007 until it is packaged; CIDER-005 is planned into it next.
        let fv1 = create_vessel(
            &tx,
            "FV1",
            VesselKind::Fermenter,
            Some(Litres::new(30.0)?),
            None,
        )?;
        create_vessel(
            &tx,
            "FV2",
            VesselKind::Fermenter,
            Some(Litres::new(30.0)?),
            None,
        )?;
        let recipe = create_recipe(&tx, "Citron-Verveine", BeverageType::Seltzer, None)?;
        let v1 = create_recipe_version(
            &tx,
            recipe,
            &NewRecipeVersion {
                target_volume_l: Some(Litres::new(20.0)?),
                target_og: Some(Sg::new(1.038)?),
                target_fg: Some(Sg::new(0.998)?),
                ..Default::default()
            },
        )?;
        for (ingredient, qty, stage, offset_hours) in [
            (sucrose, Quantity::new(4.0, Unit::Kg)?, Stage::Prep, None),
            (
                ec1118_ingredient,
                Quantity::new(5.0, Unit::G)?,
                Stage::Pitch,
                None,
            ),
            (
                fermaid,
                Quantity::new(10.0, Unit::G)?,
                Stage::Fermentation,
                Some(24.0),
            ),
            (
                fermaid,
                Quantity::new(10.0, Unit::G)?,
                Stage::Fermentation,
                Some(48.0),
            ),
        ] {
            add_recipe_item(
                &tx,
                v1,
                &NewRecipeItem {
                    ingredient_id: ingredient,
                    entry: qty,
                    stage,
                    offset_hours,
                    notes: None,
                },
            )?;
        }

        let batch_id = create_batch(
            &tx,
            &NewBatch {
                code: "SELTZER-001".into(),
                recipe_version_id: Some(v1),
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: Some("demo: cargo xtask demo-db".into()),
            },
            at(-DAY),
        )?;
        record_pitch(&tx, batch_id, at(0), Some("EC-1118, rehydrated"))?;
        pitch_yeast(batch_id, ec1118)?;
        // 4.2 kg against the plan's 4, and the first Fermaid O only: one Both with a
        // delta, one Missing left to record from the screen.
        for (lot, qty, stage, added_at) in [
            (
                sucrose_lot,
                Quantity::new(4.2, Unit::Kg)?,
                Stage::Prep,
                None,
            ),
            (
                fermaid_lot,
                Quantity::new(10.0, Unit::G)?,
                Stage::Fermentation,
                Some(at(DAY)),
            ),
        ] {
            add_batch_ingredient(
                &tx,
                batch_id,
                &NewBatchIngredient {
                    ingredient_lot_id: lot,
                    entry: qty,
                    stage,
                    added_at,
                    notes: None,
                },
            )?;
        }

        let esp32 = create_source(&tx, SourceKind::BrewdJournal, "esp32-fv1", None)?;
        let ispindel = create_source(&tx, SourceKind::BrewdJournal, "ispindel-1", None)?;
        let streams = [
            (esp32, "vessel_temp", Metric::TempC),
            (esp32, "setpoint", Metric::SetpointC),
            (esp32, "room_temp", Metric::RoomTempC),
            (ispindel, "gravity", Metric::Sg),
            (ispindel, "temperature", Metric::GravityTempC),
            (ispindel, "battery", Metric::BatteryV),
            (ispindel, "rssi", Metric::RssiDbm),
        ]
        .map(|(source, name, metric)| {
            let id = create_stream(&tx, source, name, metric)?;
            bind_stream(&tx, batch_id, id, at(0), None)?;
            Ok::<_, brewcore::Error>(id)
        });
        let [temp, setpoint, room, sg, sg_temp, battery, rssi] = streams;
        let (temp, setpoint, room) = (temp?, setpoint?, room?);
        let (sg, sg_temp, battery, rssi) = (sg?, sg_temp?, battery?, rssi?);

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
        for t in (0..60 * DAY - 12 * HOUR)
            .step_by(900)
            .filter(|t| !pi_off.contains(t))
        {
            // A knock once a week: the spike 4a's min/max must keep and 4b's median
            // must drop.
            let bump = if t % (7 * DAY) == 3 * DAY { 0.012 } else { 0.0 };
            rows.push((sg, at(t), gravity_at(hours(t)) + rng.noise(0.002) + bump));
            rows.push((sg_temp, at(t), beer_at(t) + rng.noise(0.2)));
            rows.push((battery, at(t), 4.1 - 0.45 * hours(t) / (60.0 * 24.0)));
            rows.push((rssi, at(t), -65.0 + rng.noise(3.0)));
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
        let mut written = rows.len();

        let base = now - now % HOUR;
        // The two seltzers answer 5c's "EC-1118 seltzers, OG 1.035–1.040, at 18 °C";
        // SELTZER-001 has no OG, so it is the one the query can't judge. CIDER-004 took
        // both strains: 5d's mixed row.
        for (n, (code, kind, og, fg, tau_h, beer_c, ago, days, archived, lots)) in [
            (
                "ALE-002",
                BeverageType::Beer,
                1.050,
                1.011,
                30.0,
                19.0,
                120 * DAY,
                14 * DAY,
                true,
                vec![us05],
            ),
            (
                "ALE-003",
                BeverageType::Beer,
                1.062,
                1.012,
                44.0,
                20.0,
                9 * DAY,
                9 * DAY,
                false,
                vec![us05],
            ),
            (
                "CIDER-004",
                BeverageType::Cider,
                1.055,
                0.998,
                70.0,
                16.0,
                30 * DAY,
                20 * DAY,
                false,
                vec![ec1118, us05],
            ),
            (
                "SELTZER-006",
                BeverageType::Seltzer,
                1.037,
                0.996,
                40.0,
                17.6,
                90 * DAY,
                14 * DAY,
                true,
                vec![ec1118],
            ),
            (
                "SELTZER-007",
                BeverageType::Seltzer,
                1.038,
                0.997,
                36.0,
                17.7,
                20 * DAY,
                14 * DAY,
                false,
                vec![ec1118],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let pitch = base - ago;
            let at = |offset: i64| Timestamp::from_secs(pitch + offset);
            let batch_id = create_batch(
                &tx,
                &NewBatch {
                    code: code.into(),
                    recipe_version_id: (kind == BeverageType::Seltzer).then_some(v1),
                    beverage_type: kind,
                    vessel_id: (code == "SELTZER-007").then_some(fv1),
                    volume_l: None,
                    og: Some(Sg::new(og)?),
                    notes: Some("demo: cargo xtask demo-db".into()),
                },
                at(-DAY),
            )?;
            record_pitch(&tx, batch_id, at(0), None)?;
            for lot in lots {
                pitch_yeast(batch_id, lot)?;
            }
            set_batch_measurements(&tx, batch_id, None, None, Some(Sg::new(fg)?))?;
            let source = create_source(&tx, SourceKind::BrewdJournal, &format!("tilt-{n}"), None)?;
            let sg = create_stream(&tx, source, "gravity", Metric::Sg)?;
            let temp = create_stream(&tx, source, "temperature", Metric::TempC)?;
            let until = archived.then(|| at(days));
            bind_stream(&tx, batch_id, sg, at(0), until)?;
            bind_stream(&tx, batch_id, temp, at(0), until)?;

            let mut rows = Vec::new();
            for t in (0..days).step_by(900) {
                let h = hours(t);
                let gravity = if h <= 12.0 {
                    og
                } else {
                    fg + (og - fg) * (-(h - 12.0) / tau_h).exp()
                };
                rows.push((sg, at(t), gravity + rng.noise(0.0015)));
                rows.push((temp, at(t), beer_c + 1.5 * activity_at(h) + rng.noise(0.1)));
            }
            insert_readings(&tx, &rows)?;
            written += rows.len();
            if archived {
                archive_batch(&tx, batch_id, at(days + DAY))?;
            }
            // 7a–7c: SELTZER-007 sanitised into, fed from the second bag, conditioned and
            // packaged two days after its readings end — a keg and a case, 2 L short of
            // the recipe's 20 L — then FV1 rinsed and cleaned but not sanitised, so
            // CIDER-005's pitch warns.
            if code == "SELTZER-007" {
                let sanitised = NewCleaning {
                    kind: CleaningKind::CleanAndSanitize,
                    chemical: Some("PBW, then Star San 1.5 mL/L".into()),
                    notes: None,
                };
                add_cleaning(&tx, fv1, at(-3 * HOUR), &sanitised)?;
                add_batch_ingredient(
                    &tx,
                    batch_id,
                    &NewBatchIngredient {
                        ingredient_lot_id: sucrose_bulk,
                        entry: Quantity::new(4.0, Unit::Kg)?,
                        stage: Stage::Prep,
                        added_at: None,
                        notes: None,
                    },
                )?;
                transition_batch(&tx, batch_id, BatchStatus::Conditioning, at(10 * DAY))?;
                let line =
                    |format, size: f64, count, volume: f64| -> brewcore::Result<NewPackaging> {
                        Ok(NewPackaging {
                            format,
                            container_size_l: Some(Litres::new(size)?),
                            container_count: Some(count),
                            volume_l: Some(Litres::new(volume)?),
                            notes: None,
                        })
                    };
                package_batch(
                    &tx,
                    batch_id,
                    at(days + 2 * DAY),
                    &[
                        line(PackageFormat::Keg, 19.0, 1, 12.0)?,
                        line(PackageFormat::Bottle, 0.5, 12, 6.0)?,
                    ],
                )?;
                let rinsed = NewCleaning {
                    kind: CleaningKind::Clean,
                    chemical: Some("PBW".into()),
                    notes: Some("sanitise before the next pitch".into()),
                };
                add_cleaning(&tx, fv1, at(days + 2 * DAY + 2 * HOUR), &rinsed)?;
            }
        }
        // The first bag weighed: 0.7 kg against the 0.8 kg the books say.
        set_remaining(
            &tx,
            sucrose_lot,
            Timestamp::from_secs(base),
            Quantity::new(0.7, Unit::Kg)?,
            None,
        )?;

        create_batch(
            &tx,
            &NewBatch {
                code: "CIDER-005".into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Cider,
                vessel_id: Some(fv1),
                volume_l: None,
                og: None,
                notes: Some("demo: not pitched yet".into()),
            },
            Timestamp::from_secs(base),
        )?;

        tx.commit()?;
        Ok(written)
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
