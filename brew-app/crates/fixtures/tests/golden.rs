//! Byte-level regression guard on the generator.
//!
//! The committed files under `tests/fixtures/` are what
//! `cargo xtask gen-fixture --scenario all --rows 5 --seed 1` produced. If a change
//! to quoting, float formatting, line endings or the curve moves a single byte, this
//! fails and the diff is reviewable. Regenerating is deliberate: run that command
//! again, look at what moved, commit it.
//!
//! Worth saying out loud, since these files are the parser's only reality until a
//! Fermentrack exists: a golden proves the generator has not *changed*, not that it
//! is *right*. Rightness comes from the source excerpts cited in `src/lib.rs`.

use std::path::{Path, PathBuf};

use brewfixture::{JournalScenario, JournalSpec, Kind, LogSpec, Scenario};

const ROWS: usize = 5;
const SEED: u64 = 1;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn spec(scenario: Scenario) -> LogSpec {
    LogSpec {
        rows: ROWS,
        seed: SEED,
        ..scenario.spec()
    }
}

#[test]
fn the_committed_fixtures_are_byte_for_byte_what_the_generator_emits() {
    for scenario in Scenario::ALL {
        let spec = spec(scenario);
        for kind in [Kind::Full, Kind::Graph] {
            let path = fixtures_dir().join(spec.file_name(kind));
            let committed = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{}: {e}\nrun: cargo xtask gen-fixture --scenario all --rows {ROWS} --seed {SEED} --out crates/fixtures/tests/fixtures", path.display()));

            assert_eq!(
                String::from_utf8(committed).expect("fixtures are UTF-8"),
                spec.csv(kind),
                "{} is stale",
                path.display()
            );
        }
    }
}

/// Git is not configured to touch these, but a `core.autocrlf` somewhere would turn
/// every `\r\n` into `\r\r\n` and make the offset tests agree with a corrupt file.
#[test]
fn the_committed_fixtures_are_still_crlf_on_disk() {
    for scenario in Scenario::ALL {
        let spec = spec(scenario);
        let bytes = std::fs::read(fixtures_dir().join(spec.file_name(Kind::Full))).unwrap();

        assert!(bytes.ends_with(b"\r\n"));
        assert_eq!(
            bytes.windows(2).filter(|w| w == b"\r\n").count(),
            ROWS + 1,
            "expected a header and {ROWS} rows"
        );
        assert!(
            !bytes.windows(3).any(|w| w == b"\r\r\n"),
            "line endings were rewritten somewhere between here and the generator"
        );
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "a BOM crept in");
    }
}

/// The shape a Phase 2b test will lean on: a header, then one row per log point,
/// with the same cell count on every line.
#[test]
fn every_row_has_as_many_cells_as_the_header() {
    for scenario in Scenario::ALL {
        let spec = spec(scenario);
        for kind in [Kind::Full, Kind::Graph] {
            let csv = std::fs::read_to_string(fixtures_dir().join(spec.file_name(kind))).unwrap();
            let expected = spec.header(kind).len();

            for (i, line) in csv.split_terminator("\r\n").enumerate() {
                // Counting cells needs the quoting honoured, so this counts commas
                // outside quotes rather than calling `split(',')`.
                let mut cells = 1;
                let mut in_quotes = false;
                for c in line.chars() {
                    match c {
                        '"' => in_quotes = !in_quotes,
                        ',' if !in_quotes => cells += 1,
                        _ => {}
                    }
                }
                assert_eq!(cells, expected, "line {i} of {}", spec.file_name(kind));
            }
        }
    }
}

/// Phase Vd. The same guard over the `brewd` journal fixtures, which live in their own
/// directory and are LF rather than CRLF.
///
/// Regenerate with `cargo xtask gen-fixture --format journal --rows 5 --seed 1 --out
/// crates/fixtures/tests/fixtures/journal`.
#[test]
fn the_committed_journals_are_byte_for_byte_what_the_generator_emits() {
    for scenario in JournalScenario::ALL {
        let spec = JournalSpec {
            rows: ROWS,
            seed: SEED,
            ..scenario.spec()
        };
        let path = fixtures_dir().join("journal").join(spec.file_name());
        let committed = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{}: {e}\nrun: cargo xtask gen-fixture --format journal --rows {ROWS} --seed {SEED} --out crates/fixtures/tests/fixtures/journal",
                path.display()
            )
        });

        assert!(!committed.contains(&b'\r'), "{} is not LF", path.display());
        assert_eq!(
            String::from_utf8(committed).expect("fixtures are UTF-8"),
            spec.csv(),
            "{} is stale",
            path.display()
        );
    }
}
