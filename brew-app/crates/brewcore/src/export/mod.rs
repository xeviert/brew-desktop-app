//! Phase 5e. CSV export: the same query a screen runs, then a writer.
//!
//! **Never a scrape of the rendered table.** Each entry point below calls the function
//! behind its screen — [`run_query`], [`strain_rollup`], [`compare_batches`],
//! [`batch_series`] — so a file can't disagree with what was on screen, only be
//! more complete: series come back unbucketed (`max_points` unbounded).
//!
//! **Canonical units, named in the header** (`og_sg`, `ferm_temp_c_median`). Display
//! units are for eyes; a file gets re-imported, and a column whose unit depends on a
//! setting at export time can't be. Times are both `*_ts` (epoch seconds) and
//! `*_utc` (ISO 8601).
//!
//! **Missing is empty plus a reason.** A metric that couldn't be computed is an empty
//! cell and its `*_missing` column says why — never 0, which would poison every
//! average taken over the file, as 5a's `Computed` exists to prevent.

mod tables;

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use rusqlite::Connection;

pub use tables::{comparison_table, metrics_table, readings_table, rollup_table};

use crate::{
    Error, Result,
    calc::stability::StabilityParams,
    config::ExportDialect,
    db::{
        Metric,
        batch::get_batch,
        compare::{RelRange, YMode, compare_batches},
        query::{BatchFilter, run_query},
        rollup::strain_rollup,
        series::{batch_series, binding_span},
    },
    units::Timestamp,
};

/// Which of `#/query`'s two views to export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryTable {
    Batches,
    Strains,
}

/// One cell. Numbers are kept apart from text so a dialect can localise the decimal
/// point without touching a note that happens to contain a `.`.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    /// Already formatted with a `.` decimal point.
    Num(String),
}

impl Cell {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// Non-finite is empty: `NaN` in a CSV reads back as a string, or not at all.
    /// `Display` for `f64` is the shortest string that round-trips, and never uses an
    /// exponent, which Excel would reinterpret.
    pub fn num(v: f64) -> Self {
        if v.is_finite() {
            Self::Num(v.to_string())
        } else {
            Self::Empty
        }
    }

    pub fn int(v: impl Into<i64>) -> Self {
        Self::Num(v.into().to_string())
    }

    pub fn count(v: usize) -> Self {
        Self::Num(v.to_string())
    }

    pub fn flag(v: bool) -> Self {
        Self::Text(if v { "true" } else { "false" }.to_owned())
    }

    pub fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> Cell) -> Self {
        v.map_or(Self::Empty, f)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub header: Vec<&'static str>,
    pub rows: Vec<Vec<Cell>>,
}

/// Epoch seconds and ISO 8601 UTC, in that order.
pub(crate) fn time_cells(t: Option<Timestamp>) -> [Cell; 2] {
    match t {
        Some(t) => [Cell::int(t.as_secs()), Cell::opt(iso_utc(t), Cell::Text)],
        None => [Cell::Empty, Cell::Empty],
    }
}

/// `None` only outside chrono's range, some quarter-million years out.
fn iso_utc(t: Timestamp) -> Option<String> {
    chrono::DateTime::from_timestamp(t.as_secs(), 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// UTF-8's byte order mark. Excel reads a CSV without one as the ANSI code page, so
/// "Levure Côte des Blancs" arrives as "CÃ´te".
const BOM: &[u8] = b"\xEF\xBB\xBF";

pub fn write<W: Write>(table: &Table, dialect: ExportDialect, mut out: W) -> std::io::Result<()> {
    let (delimiter, decimal_comma) = match dialect {
        ExportDialect::Rfc4180 => (b',', false),
        ExportDialect::ExcelFr => {
            out.write_all(BOM)?;
            (b';', true)
        }
    };
    let mut w = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .terminator(csv::Terminator::CRLF)
        .from_writer(out);
    w.write_record(&table.header)?;
    for row in &table.rows {
        debug_assert_eq!(row.len(), table.header.len(), "row and header disagree");
        w.write_record(row.iter().map(|cell| match cell {
            Cell::Empty => String::new(),
            Cell::Text(s) => s.clone(),
            Cell::Num(s) if decimal_comma => s.replace('.', ","),
            Cell::Num(s) => s.clone(),
        }))?;
    }
    w.flush()
}

pub fn write_file(path: &Path, table: &Table, dialect: ExportDialect) -> Result<()> {
    let err = |source| Error::ExportWrite {
        path: path.to_owned(),
        source,
    };
    let file = File::create(path).map_err(err)?;
    let mut out = BufWriter::new(file);
    write(table, dialect, &mut out).map_err(err)?;
    // `BufWriter`'s drop swallows the last flush's error; a full disk shows up here.
    out.into_inner()
        .map_err(|e| err(e.into_error()))?
        .sync_all()
        .map_err(err)
}

/// 5c's result, as the batches table or 5d's rollup.
pub fn query(
    conn: &Connection,
    filter: &BatchFilter,
    params: &StabilityParams,
    table: QueryTable,
) -> Result<Table> {
    let result = run_query(conn, filter, params)?;
    Ok(match table {
        QueryTable::Batches => metrics_table(&result),
        QueryTable::Strains => rollup_table(&strain_rollup(conn, &result.rows)?),
    })
}

/// 5b's overlay over the same span, every point.
pub fn comparison(
    conn: &Connection,
    batch_ids: &[i64],
    y: YMode,
    with_temp: bool,
    span: Option<RelRange>,
) -> Result<Table> {
    let comparison = compare_batches(conn, batch_ids, y, with_temp, span, usize::MAX)?;
    Ok(comparison_table(&comparison))
}

/// Every reading bound to the batch, every metric, over the whole record — not the
/// chart's zoom or its `[pitch, now)` default, which would drop pre-pitch readings.
pub fn readings(conn: &Connection, batch_id: i64) -> Result<Table> {
    let batch = get_batch(conn, batch_id)?;
    let span = binding_span(conn, batch_id, Timestamp::now())?;
    let series = batch_series(conn, batch_id, &Metric::ALL, span, usize::MAX)?;
    Ok(readings_table(&series, batch.pitched_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Table {
        Table {
            header: vec!["code", "og_sg", "note"],
            rows: vec![
                vec![
                    Cell::text("SELTZER-001"),
                    Cell::num(1.0452),
                    Cell::text("a;b,\"c\"\nd"),
                ],
                vec![Cell::text("SELTZER-002"), Cell::num(0.000001), Cell::Empty],
                vec![Cell::text("Côte des Blancs"), Cell::num(-3.5), Cell::text("1.5")],
            ],
        }
    }

    fn written(dialect: ExportDialect) -> Vec<u8> {
        let mut out = Vec::new();
        write(&table(), dialect, &mut out).unwrap();
        out
    }

    fn read_back(bytes: &[u8], delimiter: u8) -> Vec<Vec<String>> {
        csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(false)
            .from_reader(bytes)
            .records()
            .map(|r| r.unwrap().iter().map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn rfc4180_round_trips() {
        let bytes = written(ExportDialect::Rfc4180);
        assert!(!bytes.starts_with(BOM));
        assert!(bytes.ends_with(b"\r\n"));
        assert_eq!(
            read_back(&bytes, b','),
            [
                vec!["code", "og_sg", "note"],
                vec!["SELTZER-001", "1.0452", "a;b,\"c\"\nd"],
                vec!["SELTZER-002", "0.000001", ""],
                vec!["Côte des Blancs", "-3.5", "1.5"],
            ]
        );
    }

    #[test]
    fn excel_fr_round_trips_with_decimal_commas_and_text_untouched() {
        let bytes = written(ExportDialect::ExcelFr);
        let body = bytes.strip_prefix(BOM).expect("a BOM first");
        assert!(body.starts_with(b"code;og_sg;note\r\n"));
        assert_eq!(
            read_back(body, b';'),
            [
                vec!["code", "og_sg", "note"],
                vec!["SELTZER-001", "1,0452", "a;b,\"c\"\nd"],
                vec!["SELTZER-002", "0,000001", ""],
                vec!["Côte des Blancs", "-3,5", "1.5"],
            ]
        );
    }

    #[test]
    fn numbers_never_use_an_exponent_and_non_finite_is_empty() {
        assert_eq!(Cell::num(1e-7), Cell::Num("0.0000001".into()));
        assert_eq!(Cell::num(1e21), Cell::Num("1000000000000000000000".into()));
        assert_eq!(Cell::num(f64::NAN), Cell::Empty);
        assert_eq!(Cell::num(f64::INFINITY), Cell::Empty);
    }

    #[test]
    fn times_are_epoch_and_iso_utc() {
        assert_eq!(
            time_cells(Some(Timestamp::from_secs(1_700_000_000))),
            [
                Cell::Num("1700000000".into()),
                Cell::text("2023-11-14T22:13:20Z")
            ]
        );
        assert_eq!(time_cells(None), [Cell::Empty, Cell::Empty]);
    }

    #[test]
    fn readings_are_the_whole_record_unbucketed() {
        use crate::db::{
            SourceKind,
            batch::{NewBatch, create_batch, set_pitched_at},
            recipe::BeverageType,
            tier1::{create_source, create_stream, insert_readings},
            timeline::bind_stream,
        };
        const T0: Timestamp = Timestamp::from_secs(1_700_000_000);

        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        let batch = NewBatch {
            code: "SELTZER-001".into(),
            recipe_version_id: None,
            beverage_type: BeverageType::Seltzer,
            vessel_id: None,
            volume_l: None,
            og: None,
            notes: None,
        };
        let id = create_batch(&conn, &batch, T0).unwrap();
        set_pitched_at(&conn, id, T0).unwrap();

        // Bound an hour before the pitch, and past the 1000 points a chart asks for.
        let from = T0.saturating_add_secs(-3_600);
        let source = create_source(&conn, SourceKind::BrewdJournal, "probe", None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream = create_stream(&tx, source, "probe", Metric::TempC).unwrap();
        let rows: Vec<_> = (0..3_000)
            .map(|i| (stream, from.saturating_add_secs(i * 60), 18.0))
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        bind_stream(&conn, id, stream, from, None).unwrap();

        let t = readings(&conn, id).unwrap();
        assert_eq!(t.rows.len(), 3_000);
        assert_eq!(t.rows[0][3], Cell::text("temp_c"));
        assert_eq!(t.rows[0][4], Cell::Num("-3600".into()));
    }

    #[test]
    fn a_file_is_written_and_a_bad_path_names_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out.csv");
        write_file(&path, &table(), ExportDialect::ExcelFr).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            written(ExportDialect::ExcelFr)
        );

        let bad = tmp.path().join("missing").join("out.csv");
        let err = write_file(&bad, &table(), ExportDialect::Rfc4180).unwrap_err();
        assert_eq!(err.code(), "export.write");
        assert!(err.to_string().contains("missing"), "{err}");
    }
}
