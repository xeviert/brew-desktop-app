//! Phase Vd. What a `brewd` journal's bytes mean, as a [`Dialect`].
//!
//! The format is [`crate::journal::format`]'s: `time,<columns…>`, integer epoch seconds,
//! raw payloads, an empty cell for a field that did not arrive. What this module adds is
//! the one thing `brewd` deliberately does not know — which [`Metric`] a column is, and
//! whether an iSpindle's `gravity` is SG or °P.
//!
//! The column table is fixed, in the same spirit as 2b's: a column is either one we
//! know, one we ignore on purpose (`interval`), or reported in `unknown_columns`. It is
//! keyed on the names in `crates/brewd/deploy/brewd.example.toml`, which is therefore
//! part of the contract.
//!
//! The gravity unit is a type parameter because [`Dialect::parse`] has no `self`, and
//! it is resolved in `parse` rather than at commit because a reading is stored in SG or
//! not at all. [`ingest`] picks the parameter from the live config on every poll.

use std::marker::PhantomData;

use serde::Serialize;

use crate::{
    Error, Result,
    calc::gravity::plato_to_sg,
    config::IspindelUnit,
    db::{Db, Metric, tier1::AnnotationChannel},
    journal::{
        self, CommitReport, Dialect, FetchOutcome, ParseReport, ParsedChunk, RestartReason,
        SyncState, SyncStep, TIME_COLUMN, preview, split_first_record,
    },
    units::{Celsius, Sg, Timestamp},
};

/// Every column a journal may carry. `None` is a column we know and skip on purpose.
///
/// `temperature` is the iSpindle's own thermometer and is assumed to be °C, the
/// firmware default; the payload does not say.
const COLUMNS: [(&str, Option<Metric>); 9] = [
    ("tilt", Some(Metric::TiltDeg)),
    ("gravity", Some(Metric::Sg)),
    ("temperature", Some(Metric::GravityTempC)),
    ("battery", Some(Metric::BatteryV)),
    ("rssi", Some(Metric::RssiDbm)),
    ("interval", None),
    ("vessel_temp", Some(Metric::TempC)),
    ("chamber_temp", Some(Metric::FridgeTempC)),
    ("room_temp", Some(Metric::RoomTempC)),
];

/// The metric a journal column carries, by the same table the parser uses. Phase 6's
/// alerts read journals on the Pi and need to find a device's battery and signal.
pub fn column_metric(column: &str) -> Option<Metric> {
    COLUMNS
        .iter()
        .find(|(name, _)| *name == column)
        .and_then(|(_, metric)| *metric)
}

/// The iSpindle gravity unit a [`JournalCsv`] parses with.
pub trait GravityUnit {
    const UNIT: Option<IspindelUnit>;
}

/// Markers for [`GravityUnit`].
pub mod unit {
    use super::{GravityUnit, IspindelUnit};

    pub struct Sg;
    pub struct Plato;
    /// Nothing recorded yet. A journal with a `gravity` column refuses to parse.
    pub struct Unset;

    impl GravityUnit for Sg {
        const UNIT: Option<IspindelUnit> = Some(IspindelUnit::Sg);
    }
    impl GravityUnit for Plato {
        const UNIT: Option<IspindelUnit> = Some(IspindelUnit::Plato);
    }
    impl GravityUnit for Unset {
        const UNIT: Option<IspindelUnit> = None;
    }
}

/// A `brewd` journal, with gravity read as `U`.
pub struct JournalCsv<U>(PhantomData<U>);

/// One journal record: a receipt time and whatever values the burst carried.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalRow {
    pub ts: Timestamp,
    /// Present values only, already in storage units.
    pub values: Vec<(&'static str, Metric, f64)>,
}

/// One header column this app will store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MappedColumn {
    pub column: String,
    pub metric: Metric,
}

/// What a header says about the journal it heads. The Sources pane shows it before
/// the user adds the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalHeader {
    /// Terminator stripped, as `sync_state.header` stores it.
    pub header: String,
    pub mapped: Vec<MappedColumn>,
    pub unknown_columns: Vec<String>,
    /// A `gravity` column is present, so `gravity.ispindel_unit` must be set before
    /// this journal syncs.
    pub needs_ispindel_unit: bool,
}

/// A header, resolved against [`COLUMNS`]. Index `i` is cell `i + 1`.
struct Layout {
    cells: Vec<Option<(&'static str, Metric)>>,
    unknown: Vec<String>,
}

impl Layout {
    fn from_header(header: &str) -> Result<Self> {
        let not_a_journal = |detail: String| Error::NotABrewdJournal { detail };
        let record = record(header.as_bytes())
            .ok_or_else(|| not_a_journal(format!("unreadable header: {}", preview(header))))?;

        let mut names = record.iter();
        match names.next() {
            Some(first) if first == TIME_COLUMN => {}
            _ => {
                return Err(not_a_journal(format!(
                    "the first column is not `{TIME_COLUMN}`: {}",
                    preview(header)
                )));
            }
        }

        let mut layout = Layout {
            cells: Vec::new(),
            unknown: Vec::new(),
        };
        for name in names {
            let known = COLUMNS.iter().find(|(column, _)| *column == name);
            let cell = match known {
                Some((column, Some(metric))) => Some((*column, *metric)),
                Some((_, None)) => None,
                None => {
                    layout.unknown.push(name.to_owned());
                    None
                }
            };
            // `fan_out` keys streams by metric per source, so two columns for one
            // metric would merge into a single series with nothing to say so.
            if let Some((column, metric)) = cell
                && let Some((other, _)) = layout.cells.iter().flatten().find(|(_, m)| *m == metric)
            {
                return Err(not_a_journal(format!(
                    "`{other}` and `{column}` would both be stored as {}",
                    metric.as_str()
                )));
            }
            layout.cells.push(cell);
        }
        Ok(layout)
    }

    fn has_gravity(&self) -> bool {
        self.cells.iter().flatten().any(|(_, m)| *m == Metric::Sg)
    }
}

/// One line as cells. `None` only for a reader error, which a single `\n`-terminated
/// line gives only when it is not UTF-8.
fn record(line: &[u8]) -> Option<csv::StringRecord> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(line);
    let mut record = csv::StringRecord::new();
    match reader.read_record(&mut record) {
        Ok(_) => Some(record),
        Err(_) => None,
    }
}

/// A cell's value in storage units, or `Err(())` for one that is present but unusable.
fn value<U: GravityUnit>(metric: Metric, raw: &str) -> std::result::Result<f64, ()> {
    let value: f64 = raw.trim().parse().map_err(|_| ())?;
    // `f64::from_str` accepts `nan` and `inf`, and ESPHome publishes `nan` for a probe
    // that failed to read.
    if !value.is_finite() {
        return Err(());
    }
    match metric {
        Metric::Sg => {
            let sg = match U::UNIT {
                Some(IspindelUnit::Plato) => plato_to_sg(value).map_err(|_| ())?,
                _ => Sg::new(value).map_err(|_| ())?,
            };
            sg.is_plausible().then_some(sg.get()).ok_or(())
        }
        Metric::TempC | Metric::FridgeTempC | Metric::RoomTempC | Metric::GravityTempC => {
            Celsius::PLAUSIBLE
                .contains(&value)
                .then_some(value)
                .ok_or(())
        }
        _ => Ok(value),
    }
}

impl<U: GravityUnit> Dialect for JournalCsv<U> {
    type Row = JournalRow;

    fn parse(name: &str, header: &str, body: &[u8]) -> Result<ParsedChunk<JournalRow>> {
        let layout = Layout::from_header(header)?;
        if U::UNIT.is_none() && layout.has_gravity() {
            return Err(Error::IspindelUnitUnset {
                journal: name.to_owned(),
            });
        }

        let mut chunk = ParsedChunk {
            rows: Vec::new(),
            report: ParseReport {
                unknown_columns: layout.unknown.clone(),
                ..ParseReport::default()
            },
        };
        let (mut rest, mut at) = (body, 0);
        while let Some((line, tail)) = split_first_record(rest) {
            match parse_row::<U>(line, &layout, &mut chunk.report) {
                Some(row) => {
                    chunk.rows.push(row);
                    chunk.report.rows_parsed += 1;
                }
                None => chunk.report.rows_skipped += 1,
            }
            at += line.len();
            chunk.report.bytes_consumed = at;
            rest = tail;
        }
        Ok(chunk)
    }

    fn ts(row: &JournalRow) -> Timestamp {
        row.ts
    }

    fn readings(row: &JournalRow) -> impl Iterator<Item = (&str, Metric, f64)> {
        row.values
            .iter()
            .map(|&(column, metric, value)| (column, metric, value))
    }

    fn annotations(_: &JournalRow) -> impl Iterator<Item = (AnnotationChannel, &str)> {
        std::iter::empty()
    }

    fn not_a_log(detail: String) -> Error {
        Error::NotABrewdJournal { detail }
    }

    /// A journal row is a value stamped at receipt and never rewritten, so a file that
    /// shrank or rotated invalidates nothing already stored. Only Rebuild deletes.
    fn deletes_on_restart(reason: &RestartReason) -> bool {
        matches!(reason, RestartReason::Requested)
    }
}

/// A row, or `None` to skip it: the wrong cell count, or no usable `time`.
fn parse_row<U: GravityUnit>(
    line: &[u8],
    layout: &Layout,
    report: &mut ParseReport,
) -> Option<JournalRow> {
    let record = record(line)?;
    if record.len() != layout.cells.len() + 1 {
        tracing::warn!(
            cells = record.len(),
            expected = layout.cells.len() + 1,
            "skipping a journal row with the wrong cell count"
        );
        return None;
    }
    let ts = Timestamp::from_secs(record.get(0)?.trim().parse().ok()?);

    let mut values = Vec::new();
    for (cell, raw) in layout.cells.iter().zip(record.iter().skip(1)) {
        let Some((column, metric)) = *cell else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        match value::<U>(metric, raw) {
            Ok(value) => values.push((column, metric, value)),
            Err(()) => {
                tracing::warn!(column, cell = raw, "dropping an unusable journal value");
                report.values_dropped += 1;
            }
        }
    }
    Some(JournalRow { ts, values })
}

/// Classify a journal from its first bytes. The probe the Sources pane runs before
/// adding one; like Fermentrack's, it never parses a row, so a prefix that ends
/// mid-record cannot be mistaken for data.
pub fn inspect_header(bytes: &[u8]) -> Result<JournalHeader> {
    let not_a_journal = |detail: &str| Error::NotABrewdJournal {
        detail: detail.to_owned(),
    };
    let (line, _) = split_first_record(bytes).ok_or_else(|| {
        not_a_journal(if bytes.is_empty() {
            "the file is empty"
        } else {
            "the first line never ends"
        })
    })?;
    let header = std::str::from_utf8(line)
        .map_err(|_| not_a_journal("it is not UTF-8"))?
        .trim_end_matches(['\r', '\n']);

    let layout = Layout::from_header(header)?;
    Ok(JournalHeader {
        header: header.to_owned(),
        mapped: layout
            .cells
            .iter()
            .flatten()
            .map(|(column, metric)| MappedColumn {
                column: (*column).to_owned(),
                metric: *metric,
            })
            .collect(),
        needs_ispindel_unit: layout.has_gravity(),
        unknown_columns: layout.unknown,
    })
}

/// [`journal::ingest()`] with gravity read in `unit`.
pub fn ingest(
    unit: Option<IspindelUnit>,
    state: &SyncState,
    outcome: FetchOutcome,
) -> Result<SyncStep<JournalRow>> {
    match unit {
        Some(IspindelUnit::Sg) => journal::ingest::<JournalCsv<unit::Sg>>(state, outcome),
        Some(IspindelUnit::Plato) => journal::ingest::<JournalCsv<unit::Plato>>(state, outcome),
        None => journal::ingest::<JournalCsv<unit::Unset>>(state, outcome),
    }
}

/// [`journal::commit()`] for a journal. The unit is fixed arbitrarily: a commit reads
/// rows already converted, and nothing it calls depends on the parameter.
pub fn commit(
    db: &Db,
    state: &mut SyncState,
    step: &SyncStep<JournalRow>,
    at: Timestamp,
) -> Result<CommitReport> {
    journal::commit::<JournalCsv<unit::Unset>>(db, state, step, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESP32: &str = "time,vessel_temp,chamber_temp";
    const ISPINDEL: &str = "time,tilt,gravity,temperature,battery,rssi,interval";

    fn parse<U: GravityUnit>(header: &str, body: &str) -> Result<ParsedChunk<JournalRow>> {
        JournalCsv::<U>::parse("esp32-1.csv", header, body.as_bytes())
    }

    fn metrics(row: &JournalRow) -> Vec<(Metric, f64)> {
        row.values.iter().map(|&(_, m, v)| (m, v)).collect()
    }

    #[test]
    fn every_known_column_maps_to_its_metric() {
        let chunk =
            parse::<unit::Sg>(ISPINDEL, "1790000000,40.1,1.0420,20.5,4.08,-67,900\n").unwrap();
        assert_eq!(chunk.report.rows_parsed, 1);
        assert_eq!(chunk.report.unknown_columns, Vec::<String>::new());
        assert_eq!(chunk.rows[0].ts, Timestamp::from_secs(1_790_000_000));
        assert_eq!(
            metrics(&chunk.rows[0]),
            [
                (Metric::TiltDeg, 40.1),
                (Metric::Sg, 1.042),
                (Metric::GravityTempC, 20.5),
                (Metric::BatteryV, 4.08),
                (Metric::RssiDbm, -67.0),
            ]
        );

        let chunk = parse::<unit::Unset>(
            "time,vessel_temp,chamber_temp,room_temp",
            "1,18.5,17.0,21\n",
        )
        .unwrap();
        assert_eq!(
            metrics(&chunk.rows[0]),
            [
                (Metric::TempC, 18.5),
                (Metric::FridgeTempC, 17.0),
                (Metric::RoomTempC, 21.0)
            ]
        );
    }

    #[test]
    fn plato_is_stored_as_sg() {
        let chunk = parse::<unit::Plato>("time,gravity", "1,12.0\n").unwrap();
        let (_, sg) = metrics(&chunk.rows[0])[0];
        assert!((sg - 1.0484).abs() < 0.0005, "{sg}");

        // 1.048 read as °P is not a gravity anyone brews at, but it *is* a valid
        // number: only the plausibility band on the SG side can catch it, and the
        // SG side is not wrong. That is why the unit has no default.
        let chunk = parse::<unit::Sg>("time,gravity", "1,12.0\n").unwrap();
        assert_eq!(chunk.report.values_dropped, 1);
    }

    #[test]
    fn a_gravity_column_with_no_unit_refuses_the_chunk() {
        let err = parse::<unit::Unset>("time,gravity", "1,1.040\n").unwrap_err();
        assert_eq!(err.code(), "brewd.ispindel_unit_unset");
        // A journal with no gravity column never asks.
        assert!(parse::<unit::Unset>(ESP32, "1,18.0,17.0\n").is_ok());
    }

    #[test]
    fn absent_cells_cost_nothing_and_unusable_ones_are_counted() {
        let chunk =
            parse::<unit::Unset>(ESP32, "1,18.0,\n2,nan,17.0\n3,\"1,5\"\"\",17.1\n").unwrap();
        assert_eq!(chunk.report.rows_parsed, 3);
        assert_eq!(chunk.report.values_dropped, 2);
        assert_eq!(metrics(&chunk.rows[0]), [(Metric::TempC, 18.0)]);
        assert_eq!(metrics(&chunk.rows[1]), [(Metric::FridgeTempC, 17.0)]);
        assert_eq!(metrics(&chunk.rows[2]), [(Metric::FridgeTempC, 17.1)]);
    }

    #[test]
    fn a_row_without_a_usable_time_or_shape_is_skipped() {
        let chunk =
            parse::<unit::Unset>(ESP32, "x,18.0,17.0\n1.5,18.0,17.0\n2,18.0\n3,18.0,17.0\n")
                .unwrap();
        assert_eq!(chunk.report.rows_skipped, 3);
        assert_eq!(chunk.report.rows_parsed, 1);
        assert_eq!(chunk.report.bytes_consumed, 45);
    }

    #[test]
    fn interval_is_ignored_and_strangers_are_reported() {
        let chunk = parse::<unit::Unset>("time,interval,humidity", "1,900,55\n").unwrap();
        assert_eq!(chunk.report.unknown_columns, ["humidity"]);
        assert!(chunk.rows[0].values.is_empty());
        assert_eq!(chunk.report.values_dropped, 0);
    }

    #[test]
    fn a_header_must_start_with_time_and_name_each_metric_once() {
        let err = parse::<unit::Unset>("log_time,beer_temp", "").unwrap_err();
        assert_eq!(err.code(), "brewd.not_a_journal");

        // There is no second column for the same metric in the table today; a copy of
        // one column is the shape the check exists for.
        let err = parse::<unit::Unset>("time,vessel_temp,vessel_temp", "").unwrap_err();
        assert_eq!(err.code(), "brewd.not_a_journal");
        assert!(err.to_string().contains("temp_c"), "{err}");
    }

    #[test]
    fn a_header_probe_names_what_will_be_stored() {
        let header = inspect_header(b"time,gravity,interval,humidity\n1,1.0").unwrap();
        assert_eq!(header.header, "time,gravity,interval,humidity");
        assert_eq!(
            header.mapped,
            [MappedColumn {
                column: "gravity".to_owned(),
                metric: Metric::Sg
            }]
        );
        assert_eq!(header.unknown_columns, ["humidity"]);
        assert!(header.needs_ispindel_unit);

        for (bytes, code) in [
            (&b""[..], "brewd.not_a_journal"),
            (b"time,vessel_te", "brewd.not_a_journal"),
            (b"<html>\n", "brewd.not_a_journal"),
        ] {
            assert_eq!(inspect_header(bytes).unwrap_err().code(), code);
        }
    }

    #[test]
    fn only_a_requested_rebuild_deletes() {
        type J = JournalCsv<unit::Unset>;
        assert!(J::deletes_on_restart(&RestartReason::Requested));
        for reason in [
            RestartReason::Truncated { total: None },
            RestartReason::NotOnARecordBoundary,
            RestartReason::NoStoredHeader,
            RestartReason::Rotated {
                rotated_to: "esp32-1.1.csv".to_owned(),
            },
        ] {
            assert!(!J::deletes_on_restart(&reason), "{reason}");
        }
    }
}
