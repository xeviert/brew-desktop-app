//! The parser against the committed fixtures, with the generator as the oracle.
//!
//! This is the test 2a existed to make possible. `brewfixture` has no `brewcore`
//! dependency and hand-rolls its own date formatting and CSV quoting, so
//! `LogSpec::generate()` is an independent statement of what each log point *is*.
//! Parsing the bytes on disk and comparing against it is evidence; comparing the
//! parser against itself would not be.
//!
//! The bytes come from disk rather than from `spec.csv(..)` for the same reason
//! `crates/fixtures/tests/golden.rs` reads from disk: a stray `core.autocrlf` that
//! rewrote every `\r\n` into `\r\r\n` would otherwise pass unnoticed.
//!
//! Replacing the fixtures with a real capture once Fermentrack is up (BUILDOUT 2a)
//! does not change these assertions — only the bytes they read.

use std::path::{Path, PathBuf};

use brewcore::{
    db::Metric,
    fermentrack::{ControllerState, parse_full_log},
    units::{Celsius, Sg, TempScale},
};
use brewfixture::{Kind, LogSpec, Row, Scenario};

/// What `cargo xtask gen-fixture --scenario all --rows 5 --seed 1` produced.
const ROWS: usize = 5;
const SEED: u64 = 1;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/tests/fixtures")
}

fn spec(scenario: Scenario) -> LogSpec {
    LogSpec {
        rows: ROWS,
        seed: SEED,
        ..scenario.spec()
    }
}

fn bytes(spec: &LogSpec, kind: Kind) -> Vec<u8> {
    let path = fixtures_dir().join(spec.file_name(kind));
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nrun: cargo xtask gen-fixture --scenario all --rows {ROWS} --seed {SEED} \
             --out crates/fixtures/tests/fixtures",
            path.display()
        )
    })
}

fn scale_of(row: &Row) -> TempScale {
    match row.temp_format {
        'C' => TempScale::C,
        'F' => TempScale::F,
        other => panic!("the generator emitted temp_format {other:?}"),
    }
}

/// The oracle's raw value put through the same conversion the parser uses, so the two
/// are bit-identical and can be compared exactly. `py_float` prints the shortest
/// representation that round-trips, so parsing it back gives the same `f64`.
fn expected_celsius(row: &Row, raw: Option<f64>) -> Option<Celsius> {
    raw.map(|value| {
        scale_of(row)
            .to_celsius(value)
            .expect("the generator emits finite values")
    })
}

#[test]
fn every_scenario_parses_to_exactly_what_the_generator_generated() {
    for scenario in Scenario::ALL {
        let spec = spec(scenario);
        let name = spec.file_name(Kind::Full);
        let chunk = parse_full_log(&name, &bytes(&spec, Kind::Full))
            .unwrap_or_else(|e| panic!("{name}: [{}] {e}", e.code()));

        let expected = spec.generate();
        assert_eq!(chunk.rows.len(), expected.len(), "{name}: row count");

        for (i, (parsed, oracle)) in chunk.rows.iter().zip(&expected).enumerate() {
            let at = format!("{name} row {i}");
            assert_eq!(parsed.ts.as_secs(), oracle.ts, "{at}: log_time");
            assert_eq!(parsed.temp_format, scale_of(oracle), "{at}: temp_format");

            assert_eq!(
                parsed.beer_temp,
                expected_celsius(oracle, oracle.beer_temp),
                "{at}: beer_temp"
            );
            assert_eq!(
                parsed.beer_set,
                expected_celsius(oracle, oracle.beer_set),
                "{at}: beer_set"
            );
            assert_eq!(
                parsed.fridge_temp,
                expected_celsius(oracle, oracle.fridge_temp),
                "{at}: fridge_temp"
            );
            assert_eq!(
                parsed.fridge_set,
                expected_celsius(oracle, oracle.fridge_set),
                "{at}: fridge_set"
            );
            assert_eq!(
                parsed.room_temp,
                expected_celsius(oracle, oracle.room_temp),
                "{at}: room_temp"
            );
            assert_eq!(
                parsed.grav_temp,
                expected_celsius(oracle, oracle.grav_temp),
                "{at}: grav_temp"
            );

            // Gravity is scale-free, so it is compared without conversion.
            assert_eq!(parsed.gravity.map(Sg::get), oracle.gravity, "{at}: gravity");
            assert_eq!(
                parsed.state,
                ControllerState::from_code(oracle.state as i64),
                "{at}: state"
            );
            assert_eq!(parsed.beer_ann, oracle.beer_ann, "{at}: beer_ann");
            assert_eq!(parsed.fridge_ann, oracle.fridge_ann, "{at}: fridge_ann");
            assert_eq!(
                parsed.associated_beer_id,
                Some(spec.log_id as i64),
                "{at}: associated_beer_id"
            );
        }
    }
}

/// Nothing in a well-formed log should be skipped or dropped. A report that is not
/// pristine here means the parser is throwing away data the generator says is good.
#[test]
fn a_clean_log_costs_nothing_in_the_report() {
    for scenario in Scenario::ALL {
        let spec = spec(scenario);
        let name = spec.file_name(Kind::Full);
        let chunk = parse_full_log(&name, &bytes(&spec, Kind::Full)).unwrap();

        assert_eq!(chunk.report.rows_parsed, ROWS, "{name}");
        assert_eq!(chunk.report.rows_skipped, 0, "{name}");
        assert_eq!(chunk.report.values_dropped, 0, "{name}");
        assert!(chunk.report.unknown_columns.is_empty(), "{name}");
    }
}

/// The trap the bare-zero rule exists for. `ugly` has no room probe, so Fermentrack
/// writes the integer `0` on every row — which must not become a year of 0 °C.
#[test]
fn the_absent_room_probe_is_none_on_every_row_and_never_zero() {
    let spec = spec(Scenario::Ugly);
    let chunk = parse_full_log("ugly", &bytes(&spec, Kind::Full)).unwrap();

    assert!(!chunk.rows.is_empty());
    for (i, row) in chunk.rows.iter().enumerate() {
        assert_eq!(row.room_temp, None, "row {i}");
        assert!(
            !row.readings()
                .any(|(metric, _)| metric == Metric::RoomTempC),
            "row {i} put an absent sensor in the fan-out"
        );
    }
}

/// `gravity_enabled` changes the column set, and 2b reads the header rather than
/// guessing. `nominal` has no gravity columns at all; `gravity` and `ugly` do.
#[test]
fn gravity_is_read_only_when_the_columns_are_there() {
    for (scenario, present) in [
        (Scenario::Nominal, false),
        (Scenario::Gravity, true),
        (Scenario::Ugly, true),
        (Scenario::LegacyV1, false),
    ] {
        let spec = spec(scenario);
        let chunk = parse_full_log(scenario.name(), &bytes(&spec, Kind::Full)).unwrap();

        for (i, row) in chunk.rows.iter().enumerate() {
            assert_eq!(
                row.gravity.is_some(),
                present,
                "{} row {i}",
                scenario.name()
            );
            if let Some(sg) = row.gravity {
                // A seltzer wash between OG and FG. Outside this band the value came
                // from the wrong column.
                assert!(sg.is_plausible(), "{} row {i}: {sg}", scenario.name());
                assert!(sg.get() <= 1.045 && sg.get() >= 1.000);
            }
        }
    }
}

/// The annotations the generator plants: one with the delimiter, one with the
/// quotechar. Losing either means the CSV dialect is not being honoured, and the cells
/// after it will have shifted along by one.
#[test]
fn quoted_annotations_arrive_verbatim() {
    let spec = spec(Scenario::Ugly);
    let chunk = parse_full_log("ugly", &bytes(&spec, Kind::Full)).unwrap();

    let beer: Vec<&str> = chunk
        .rows
        .iter()
        .filter_map(|r| r.beer_ann.as_deref())
        .collect();
    let fridge: Vec<&str> = chunk
        .rows
        .iter()
        .filter_map(|r| r.fridge_ann.as_deref())
        .collect();

    assert_eq!(
        beer,
        [
            "Temp profile started, target 18.0C",
            "Beer temp set to 18.0 in web interface",
        ]
    );
    assert_eq!(fridge, ["Fridge renamed to \"Chest 2\""]);
}

/// A multi-hour outage is a gap in the timestamps, not a gap in the rows. Phase 2e's
/// duplicate check relies on the timestamps being exactly what the file said.
#[test]
fn the_outage_gap_is_preserved_exactly() {
    let spec = spec(Scenario::Ugly);
    let chunk = parse_full_log("ugly", &bytes(&spec, Kind::Full)).unwrap();
    let oracle = spec.generate();

    let deltas = |rows: &[i64]| -> Vec<i64> { rows.windows(2).map(|w| w[1] - w[0]).collect() };
    let parsed: Vec<i64> = chunk.rows.iter().map(|r| r.ts.as_secs()).collect();
    let expected: Vec<i64> = oracle.iter().map(|r| r.ts).collect();

    assert_eq!(parsed, expected);
    // Row 3 lands 4 hours plus one interval after row 2, and the cadence resumes.
    assert_eq!(deltas(&parsed)[2], 4 * 3600 + spec.interval_secs);
    assert_eq!(deltas(&parsed)[3], spec.interval_secs);
}

/// `temp_format` really does flip mid-file, and both sides must land on the same
/// physical temperature. The setpoint is the check that carries: it is a constant
/// 18 °C, written as `18.0` before the switch and `64.4` after.
#[test]
fn the_mid_file_temp_format_switch_converts_both_halves_to_the_same_setpoint() {
    let spec = spec(Scenario::Ugly);
    let chunk = parse_full_log("ugly", &bytes(&spec, Kind::Full)).unwrap();

    let scales: Vec<TempScale> = chunk.rows.iter().map(|r| r.temp_format).collect();
    assert!(scales.contains(&TempScale::C), "no Celsius rows");
    assert!(scales.contains(&TempScale::F), "no Fahrenheit rows");

    for (i, row) in chunk.rows.iter().enumerate() {
        let set = row.beer_set.expect("the setpoint is always logged").get();
        assert!(
            (set - 18.0).abs() < 1e-9,
            "row {i} ({:?}) gave a setpoint of {set}",
            row.temp_format
        );
    }
}

/// `model_version` only gates `state` in `_graph.csv`. A `_full.csv` from v1 parses
/// identically in shape to one from v2 — which is why the parser never looks at the
/// version.
#[test]
fn a_version_one_full_log_parses_the_same_as_a_version_two_one() {
    let v1 = spec(Scenario::LegacyV1);
    let v2 = spec(Scenario::Nominal);
    assert_eq!(v1.model_version, 1);
    assert_eq!(v2.model_version, 2);

    let one = parse_full_log("v1", &bytes(&v1, Kind::Full)).unwrap();
    let two = parse_full_log("v2", &bytes(&v2, Kind::Full)).unwrap();

    assert_eq!(one.rows.len(), two.rows.len());
    for (a, b) in one.rows.iter().zip(&two.rows) {
        assert!(a.state.is_some() && b.state.is_some());
        assert_eq!(
            a.readings().map(|(m, _)| m).collect::<Vec<_>>(),
            b.readings().map(|(m, _)| m).collect::<Vec<_>>()
        );
    }
}

/// Never `_graph.csv`: it substitutes `0` for a missing reading in a column where we
/// cannot tell that apart, so every average computed from it would be quietly wrong.
#[test]
fn every_graph_log_is_refused_whatever_its_shape() {
    for scenario in Scenario::ALL {
        let spec = spec(scenario);
        let name = spec.file_name(Kind::Graph);
        let err = parse_full_log(&name, &bytes(&spec, Kind::Graph))
            .expect_err("a _graph.csv must be refused");

        assert_eq!(err.code(), "fermentrack.graph_log", "{name}: {err}");
        assert!(err.to_string().contains(&name), "{err}");
    }
}

/// The fan-out width per scenario, which is what Phase 2e will insert per row.
#[test]
fn the_fan_out_width_follows_the_columns_present() {
    for (scenario, metrics) in [
        // beer temp + setpoint, fridge temp + setpoint, room temp, state
        (Scenario::Nominal, 6),
        (Scenario::LegacyV1, 6),
        // plus gravity and its own thermometer
        (Scenario::Gravity, 8),
        // gravity columns present, but no room probe
        (Scenario::Ugly, 7),
    ] {
        let spec = spec(scenario);
        let chunk = parse_full_log(scenario.name(), &bytes(&spec, Kind::Full)).unwrap();

        for (i, row) in chunk.rows.iter().enumerate() {
            assert_eq!(
                row.readings().count(),
                metrics,
                "{} row {i}: {:?}",
                scenario.name(),
                row.readings().collect::<Vec<_>>()
            );
        }
    }
}
