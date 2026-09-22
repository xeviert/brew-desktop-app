//! Reading Fermentrack's `_full.csv`.
//!
//! Bytes in, typed rows out. No network, no database: this module is the one place
//! that knows what Fermentrack's log bytes mean, and `docs/FERMENTRACK-API.md` §1 is
//! where every rule below was read from the source.
//!
//! Three rules carry the weight, and each is a bug that would be silent:
//!
//! 1. **A bare `0` is a missing reading.** `BeerLogPoint.data_point` writes
//!    `beerTemp = self.beer_temp or 0`, and Python's `str()` renders that integer as
//!    `0` while a genuine reading is `str(float)` and always carries a `.`. So `0` is
//!    `None` and `0.0` is a real zero. Get this wrong and an absent room probe
//!    becomes a year of 0 °C readings dragging down every average.
//! 2. **`temp_format` is per row.** The `Beer` caches its own format and it can drift
//!    from the device's, so a file really does switch `C` to `F` mid-stream. Convert
//!    with the row's own cell, never the file's first one.
//! 3. **Map by header name, never by position.** The gravity columns are appended
//!    after `associated_beer_id` in `_full.csv` but sit *before* `state` in
//!    `_graph.csv`, so positional indexing eventually reads gravity out of the `state`
//!    column and puts 4.0 in the SG series.
//!
//! `model_version` is deliberately never consulted: the check in
//! `Beer.column_headers` is `if which == 'base_csv' and self.model_version > 1`, so it
//! only decides whether `_graph.csv` carries `state`. Column *presence* is the only
//! question this module asks.
//!
//! # Leniency
//!
//! File-level problems are fatal; row-level problems are warned about and skipped.
//! One malformed line must not cost a week of backfill, and a wrong file must not be
//! half-ingested — half-ingesting writes wrong numbers that look right. Everything
//! skipped or dropped is counted in [`ParseReport`], which Phase 2g surfaces in the
//! status footer.

use crate::{
    Error, Result,
    db::Metric,
    fermentrack::ControllerState,
    units::{Celsius, Sg, TempScale, Timestamp},
};

/// `self.log_time.astimezone(utc).strftime('%Y/%m/%d %H:%M:%SZ')`. The trailing `Z` is
/// a literal to `chrono`, not an offset specifier — the log is always UTC.
const LOG_TIME_FORMAT: &str = "%Y/%m/%d %H:%M:%SZ";

/// Every column this module understands. Anything else in a header is recorded and
/// ignored.
const KNOWN_COLUMNS: [&str; 13] = [
    "log_time",
    "beer_temp",
    "beer_set",
    "beer_ann",
    "fridge_temp",
    "fridge_set",
    "fridge_ann",
    "room_temp",
    "state",
    "temp_format",
    "associated_beer_id",
    "gravity",
    "grav_temp",
];

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Where each known column sits in this particular file.
///
/// `log_time` and `temp_format` are not optional: their absence is the only two
/// file-level errors, so once a `Columns` exists both are known to be present.
#[derive(Debug, Clone)]
struct Columns {
    log_time: usize,
    temp_format: usize,
    beer_temp: Option<usize>,
    beer_set: Option<usize>,
    beer_ann: Option<usize>,
    fridge_temp: Option<usize>,
    fridge_set: Option<usize>,
    fridge_ann: Option<usize>,
    room_temp: Option<usize>,
    state: Option<usize>,
    associated_beer_id: Option<usize>,
    gravity: Option<usize>,
    grav_temp: Option<usize>,
    /// Cell count the header declared. A row with a different count is skipped.
    width: usize,
    unknown: Vec<String>,
}

impl Columns {
    fn from_header(name: &str, header: &csv::StringRecord) -> Result<Self> {
        let find = |wanted: &str| header.iter().position(|h| h.trim() == wanted);

        let Some(log_time) = find("log_time") else {
            // No `log_time` at all: an HTML error page, the wrong file, or a body that
            // is not CSV. The preview is what makes a misdirected URL obvious.
            return Err(Error::NotAFermentrackLog {
                detail: preview(header),
            });
        };

        // Classification is by column, not by filename suffix, so a file served under
        // a renamed path is still judged correctly. `Beer.column_headers` emits
        // `beer_temp` and `beer_set` in both log files and in every `model_version`,
        // so their absence means this is not a beer log at all — most likely a
        // `GravityLog`'s own file, which has `log_time` and `gravity` but none of the
        // controller columns and is a different parser's job.
        if find("beer_temp").is_none() || find("beer_set").is_none() {
            return Err(if find("gravity").is_some() {
                Error::GravityLogUnsupported {
                    path_hint: name.to_owned(),
                }
            } else {
                Error::NotAFermentrackLog {
                    detail: preview(header),
                }
            });
        }

        // Among beer logs, `temp_format` is the discriminator: `_graph.csv` has never
        // carried it in any `model_version`, and `_full.csv` always does.
        let Some(temp_format) = find("temp_format") else {
            return Err(Error::GraphLogRefused {
                path_hint: name.to_owned(),
            });
        };

        // Recorded, never fatal: a newer Fermentrack adding a column must not stop the
        // app ingesting the ones we do understand.
        let unknown: Vec<String> = header
            .iter()
            .map(str::trim)
            .filter(|h| !KNOWN_COLUMNS.contains(h))
            .map(str::to_owned)
            .collect();

        Ok(Self {
            log_time,
            temp_format,
            beer_temp: find("beer_temp"),
            beer_set: find("beer_set"),
            beer_ann: find("beer_ann"),
            fridge_temp: find("fridge_temp"),
            fridge_set: find("fridge_set"),
            fridge_ann: find("fridge_ann"),
            room_temp: find("room_temp"),
            state: find("state"),
            associated_beer_id: find("associated_beer_id"),
            gravity: find("gravity"),
            grav_temp: find("grav_temp"),
            width: header.len(),
            unknown,
        })
    }
}

/// The header as one line, truncated on a character boundary.
fn preview(header: &csv::StringRecord) -> String {
    super::preview(&header.iter().collect::<Vec<_>>().join(","))
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// One numeric cell from a sensor column.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cell {
    /// Fermentrack never had this value. Written as the bare integer `0`, or (for the
    /// annotation columns) as nothing at all.
    Absent,
    Real(f64),
}

/// Parse a sensor cell under Fermentrack's `x or 0` convention.
///
/// `Err(())` means the bytes are not a number at all — the caller warns and drops the
/// value rather than the row, because one unreadable cell does not invalidate the
/// timestamp next to it.
///
/// Only for `beer_temp`, `beer_set`, `fridge_temp`, `fridge_set`, `room_temp`,
/// `gravity` and `grav_temp`. `state`, `temp_format` and `associated_beer_id` are
/// genuine integers and must not come through here — a real `state` of `0` is IDLE,
/// not a missing reading.
fn py_number(cell: &str) -> Result<Cell, ()> {
    let text = cell.trim();
    // Empty should not happen outside the annotation columns, but a malformed file
    // must not panic its way through the ingest path.
    if text.is_empty() || text == "0" {
        return Ok(Cell::Absent);
    }

    let value: f64 = text.parse().map_err(|_| ())?;
    // `"nan"` and `"inf"` parse successfully. A non-finite value reaching SQLite is
    // stored as NULL, so it would vanish rather than fail loudly.
    if !value.is_finite() {
        return Err(());
    }
    if !text.contains(['.', 'e', 'E']) {
        // Fermentrack cannot emit this: it writes `str(float)`. Treating it as absent
        // would silently drop a reading we can plainly read, and erroring would reject
        // a whole file over one cell, so take it and say so.
        tracing::debug!(
            cell = text,
            "integer in a float column; Fermentrack writes str(float)"
        );
    }
    Ok(Cell::Real(value))
}

/// `2025/09/18 00:00:00Z`, and nothing else. Exactly this many characters.
const LOG_TIME_WIDTH: usize = 20;

/// `%Y/%m/%d %H:%M:%SZ` to epoch seconds. `chrono` rejects trailing garbage, so a
/// `log_time` cell with anything after the `Z` is a parse failure rather than a
/// silently truncated timestamp.
///
/// The width check is what makes 2d's record-boundary proof work. `%Y` happily accepts
/// an unpadded year, so a resume landing three bytes into a row reads
/// `5/09/18 00:00:00Z` as the year 5 — a row that parses, with a timestamp forty
/// thousand years wrong, out of bytes that were never a record. `strftime` always emits
/// four zero-padded digits, so anything other than 20 characters did not come from
/// Fermentrack.
fn parse_log_time(cell: &str) -> Option<Timestamp> {
    let cell = cell.trim();
    if cell.len() != LOG_TIME_WIDTH {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(cell, LOG_TIME_FORMAT)
        .ok()
        .map(|dt| Timestamp::from_secs(dt.and_utc().timestamp()))
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One log point, converted to canonical units.
///
/// A `None` means "no reading": the sensor was absent, the cell was unreadable, or the
/// value was outside the plausible band. Which of those it was is in the log and in
/// [`ParseReport`]; the row itself does not carry the distinction, because nothing
/// downstream would treat them differently.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRow {
    pub ts: Timestamp,
    /// The scale this row's raw values arrived in. Kept for diagnostics — every
    /// temperature below is already °C.
    pub temp_format: TempScale,
    pub beer_temp: Option<Celsius>,
    pub beer_set: Option<Celsius>,
    pub fridge_temp: Option<Celsius>,
    pub fridge_set: Option<Celsius>,
    pub room_temp: Option<Celsius>,
    pub gravity: Option<Sg>,
    pub grav_temp: Option<Celsius>,
    pub state: Option<ControllerState>,
    /// Phase 3d imports these as suggested `batch_event`s. Not a reading.
    pub beer_ann: Option<String>,
    pub fridge_ann: Option<String>,
    pub associated_beer_id: Option<i64>,
}

impl LogRow {
    /// Fan out to the metrics this row carries — up to eight, skipping every absent
    /// one. Phase 2e's write path is a loop over this, which is what keeps the
    /// column-to-[`Metric`] mapping in exactly one place.
    ///
    /// The column name rides along because it is what 2e stores in
    /// `stream.external_id`, and pairing it with the `Metric` here is what stops that
    /// mapping existing twice.
    pub fn readings(&self) -> impl Iterator<Item = (&'static str, Metric, f64)> {
        [
            ("beer_temp", Metric::TempC, self.beer_temp.map(Celsius::get)),
            (
                "beer_set",
                Metric::SetpointC,
                self.beer_set.map(Celsius::get),
            ),
            (
                "fridge_temp",
                Metric::FridgeTempC,
                self.fridge_temp.map(Celsius::get),
            ),
            (
                "fridge_set",
                Metric::FridgeSetpointC,
                self.fridge_set.map(Celsius::get),
            ),
            (
                "room_temp",
                Metric::RoomTempC,
                self.room_temp.map(Celsius::get),
            ),
            ("gravity", Metric::Sg, self.gravity.map(Sg::get)),
            (
                "grav_temp",
                Metric::GravityTempC,
                self.grav_temp.map(Celsius::get),
            ),
            // Stored as its integer. The enum is for labelling, not for storage.
            ("state", Metric::State, self.state.map(|s| s.code() as f64)),
        ]
        .into_iter()
        .filter_map(|(column, metric, value)| value.map(|value| (column, metric, value)))
    }
}

/// What a chunk cost to parse. Phase 2g renders this; a sync that is quietly dropping
/// half its values should be visible without reading the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ParseReport {
    pub rows_parsed: usize,
    /// Rows with the wrong cell count, or an unusable `log_time` / `temp_format`.
    pub rows_skipped: usize,
    /// Cells that were present but unreadable or implausible. Not counted for a
    /// genuinely absent sensor — that is data, not damage.
    pub values_dropped: usize,
    pub unknown_columns: Vec<String>,
    /// Bytes of the input taken up by records that were *read* — parsed or
    /// deliberately skipped. Relative to the buffer that was passed in, so
    /// [`parse_full_log`]'s includes the header line and [`parse_with_header`]'s does
    /// not. Phase 2d advances `byte_offset` by exactly this and never by the length of
    /// what it fetched.
    pub bytes_consumed: usize,
    /// A `csv::Error` ended the read before the end of the input. The bytes from
    /// `bytes_consumed` onwards were never looked at, so 2d must not claim them:
    /// advancing past a fault it did not parse is a permanent hole in `reading`.
    pub stopped_early: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedChunk {
    pub rows: Vec<LogRow>,
    pub report: ParseReport,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a whole `_full.csv`, header included. `name` is the remote path or filename,
/// used only in error messages.
pub fn parse_full_log(name: &str, bytes: &[u8]) -> Result<ParsedChunk> {
    let mut reader = reader(bytes, true);
    let header = reader.headers()?.clone();
    let columns = Columns::from_header(name, &header)?;
    Ok(read_rows(bytes, &mut reader, &columns))
}

/// Parse a continuation chunk against a header held elsewhere.
///
/// This is the shape Phase 2d resumes with: `sync_state.header` holds the header line,
/// and the body is whatever the `Range` request returned. `body` must begin on a
/// record boundary — see [`split_complete_lines`].
///
/// 2d reaches it on a first fetch too, by lifting the header off with
/// [`split_first_record`], so that one parser covers both and `bytes_consumed` needs no
/// per-caller adjustment.
pub fn parse_with_header(name: &str, header: &str, body: &[u8]) -> Result<ParsedChunk> {
    let header = header_record(header)?;
    let columns = Columns::from_header(name, &header)?;
    Ok(read_rows(body, &mut reader(body, false), &columns))
}

/// What a header line says about the file it heads.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HeaderInfo {
    /// The header line with its terminator stripped, exactly as `sync_state.header`
    /// stores it — so a validated probe can seed a sync without refetching.
    pub header: String,
    /// A `gravity` column is present, so this beer has a tilt or iSpindel attached.
    pub gravity_enabled: bool,
    pub unknown_columns: Vec<String>,
}

/// Classify a log from its header alone. Phase 2f's validation probe.
///
/// Deliberately not `parse_full_log` on a 2 KB prefix: the `csv` crate treats EOF as a
/// record terminator, so the half-row a prefix ends on would parse as a complete
/// reading, and validation would report a row count that is off by one and a `last_ts`
/// from a line Fermentrack never finished writing. Nothing here parses a row at all.
///
/// The three refusals — [`Error::NotAFermentrackLog`], [`Error::GraphLogRefused`],
/// [`Error::GravityLogUnsupported`] — come from [`Columns::from_header`] unchanged, so
/// the settings pane branches on the same `code()` the sync loop does. `name` is used
/// only in those messages, and should be the filename the user pasted.
pub fn inspect_header(name: &str, bytes: &[u8]) -> Result<HeaderInfo> {
    let not_a_log = |detail: &str| Error::NotAFermentrackLog {
        detail: detail.to_owned(),
    };

    // No terminator means we never saw a whole header: an empty file, or one whose
    // first line is longer than the prefix we asked for. Either way there is nothing to
    // classify, and guessing from a partial column list would refuse good logs.
    let (line, _) = split_first_record(bytes).ok_or_else(|| {
        not_a_log(if bytes.is_empty() {
            "the file is empty"
        } else {
            "the first line never ends"
        })
    })?;
    let line = std::str::from_utf8(line)
        .map_err(|_| not_a_log("it is not UTF-8"))?
        .trim_end_matches(['\r', '\n']);

    let record = header_record(line)?;
    let columns = Columns::from_header(name, &record)?;

    Ok(HeaderInfo {
        header: line.to_owned(),
        gravity_enabled: columns.gravity.is_some(),
        unknown_columns: columns.unknown,
    })
}

fn reader(bytes: &[u8], has_headers: bool) -> csv::Reader<&[u8]> {
    csv::ReaderBuilder::new()
        .has_headers(has_headers)
        // Flexible so a short row is *our* skip-with-a-warning rather than the csv
        // crate's error, which would give us no say in whether iteration continues.
        // `Columns::width` is what enforces the shape.
        .flexible(true)
        .from_reader(bytes)
}

fn header_record(header: &str) -> Result<csv::StringRecord> {
    let mut reader = reader(header.trim_end_matches(['\r', '\n']).as_bytes(), false);
    let mut record = csv::StringRecord::new();
    reader.read_record(&mut record)?;
    Ok(record)
}

/// Where a record really ends.
///
/// `csv_core` reports a record complete as soon as it sees the `\r` of a `\r\n`, leaving
/// the `\n` for the next read, so the reader's position stops one byte short of the
/// boundary. An offset saved there would have 2d resume mid-terminator.
fn consumed_through(input: &[u8], mut at: usize) -> usize {
    while matches!(input.get(at), Some(b'\r' | b'\n')) {
        at += 1;
    }
    at
}

fn read_rows(input: &[u8], reader: &mut csv::Reader<&[u8]>, columns: &Columns) -> ParsedChunk {
    let mut rows = Vec::new();
    let mut report = ParseReport {
        unknown_columns: columns.unknown.clone(),
        ..ParseReport::default()
    };
    let mut record = csv::StringRecord::new();

    loop {
        match reader.read_record(&mut record) {
            Ok(false) => break,
            Ok(true) => {
                // The reader's position advances by the bytes the record occupied, so
                // after a successful read it sits just past that record's terminator.
                // That — not the length of the chunk — is what 2d may claim.
                report.bytes_consumed = consumed_through(input, reader.position().byte() as usize);
                if let Some(row) = row_from_record(columns, &record, &mut report) {
                    report.rows_parsed += 1;
                    rows.push(row);
                }
            }
            Err(err) => {
                // A quoting or encoding fault mid-chunk. Stop rather than guess: the
                // remaining bytes are no longer trustworthy. `bytes_consumed` stays at
                // the last record that read cleanly, so 2d commits what parsed, stops
                // there, and surfaces the error — a source that wedges visibly beats
                // one that skips rows nobody will ever notice missing.
                tracing::warn!(%err, "stopped reading the log chunk early");
                report.rows_skipped += 1;
                report.stopped_early = true;
                break;
            }
        }
    }

    ParsedChunk { rows, report }
}

fn row_from_record(
    columns: &Columns,
    record: &csv::StringRecord,
    report: &mut ParseReport,
) -> Option<LogRow> {
    if record.len() != columns.width {
        tracing::warn!(
            expected = columns.width,
            found = record.len(),
            "skipping a row whose cell count does not match the header"
        );
        report.rows_skipped += 1;
        return None;
    }

    // Without a timestamp the row cannot be placed on a timeline, so there is nothing
    // useful to keep.
    let raw_time = record.get(columns.log_time).unwrap_or_default();
    let Some(ts) = parse_log_time(raw_time) else {
        tracing::warn!(
            log_time = raw_time,
            "skipping a row with an unusable log_time"
        );
        report.rows_skipped += 1;
        return None;
    };

    // Rule 2: this row's own cell. A file that switches C to F mid-stream is normal.
    let raw_format = record.get(columns.temp_format).unwrap_or_default();
    let Ok(temp_format) = raw_format.parse::<TempScale>() else {
        tracing::warn!(
            temp_format = raw_format,
            "skipping a row with an unreadable temp_format"
        );
        report.rows_skipped += 1;
        return None;
    };

    let mut temp = |index, what| temp_cell(record, index, temp_format, what, report);
    let beer_temp = temp(columns.beer_temp, "beer_temp");
    let beer_set = temp(columns.beer_set, "beer_set");
    let fridge_temp = temp(columns.fridge_temp, "fridge_temp");
    let fridge_set = temp(columns.fridge_set, "fridge_set");
    let room_temp = temp(columns.room_temp, "room_temp");
    let grav_temp = temp(columns.grav_temp, "grav_temp");

    Some(LogRow {
        ts,
        temp_format,
        beer_temp,
        beer_set,
        fridge_temp,
        fridge_set,
        room_temp,
        grav_temp,
        gravity: sg_cell(record, columns.gravity, report),
        state: state_cell(record, columns.state, report),
        beer_ann: annotation(record, columns.beer_ann),
        fridge_ann: annotation(record, columns.fridge_ann),
        associated_beer_id: columns
            .associated_beer_id
            .and_then(|i| record.get(i))
            .and_then(|cell| cell.trim().parse().ok()),
    })
}

/// A sensor cell to °C, converted with the row's scale and sanity-checked.
///
/// An absent column and an absent reading both give `None` and cost nothing; only a
/// cell we could not use counts against `values_dropped`.
fn temp_cell(
    record: &csv::StringRecord,
    index: Option<usize>,
    scale: TempScale,
    what: &'static str,
    report: &mut ParseReport,
) -> Option<Celsius> {
    let raw = record.get(index?)?;
    match py_number(raw) {
        Ok(Cell::Absent) => None,
        Ok(Cell::Real(value)) => match scale.to_celsius(value) {
            Ok(celsius) if celsius.is_plausible() => Some(celsius),
            Ok(celsius) => {
                tracing::warn!(column = what, %celsius, "dropping an implausible temperature");
                report.values_dropped += 1;
                None
            }
            Err(err) => {
                tracing::warn!(column = what, %err, "dropping an unusable temperature");
                report.values_dropped += 1;
                None
            }
        },
        Err(()) => {
            tracing::warn!(
                column = what,
                cell = raw,
                "dropping an unreadable temperature"
            );
            report.values_dropped += 1;
            None
        }
    }
}

/// The plausibility check here earns its keep: `Sg::PLAUSIBLE` is what catches gravity
/// having been read out of the `state` column, which is exactly what positional
/// indexing across the two log files produces.
fn sg_cell(
    record: &csv::StringRecord,
    index: Option<usize>,
    report: &mut ParseReport,
) -> Option<Sg> {
    let raw = record.get(index?)?;
    match py_number(raw) {
        Ok(Cell::Absent) => None,
        Ok(Cell::Real(value)) => match Sg::new(value) {
            Ok(sg) if sg.is_plausible() => Some(sg),
            Ok(sg) => {
                tracing::warn!(%sg, "dropping an implausible gravity");
                report.values_dropped += 1;
                None
            }
            Err(err) => {
                tracing::warn!(%err, "dropping an unusable gravity");
                report.values_dropped += 1;
                None
            }
        },
        Err(()) => {
            tracing::warn!(cell = raw, "dropping an unreadable gravity");
            report.values_dropped += 1;
            None
        }
    }
}

/// `state` is a genuine integer, so a `0` here is IDLE and not a missing reading.
fn state_cell(
    record: &csv::StringRecord,
    index: Option<usize>,
    report: &mut ParseReport,
) -> Option<ControllerState> {
    let raw = record.get(index?)?.trim();
    if raw.is_empty() {
        return None;
    }
    let Ok(code) = raw.parse::<i64>() else {
        tracing::warn!(cell = raw, "dropping an unreadable state");
        report.values_dropped += 1;
        return None;
    };
    match ControllerState::from_code(code) {
        Some(state) => Some(state),
        None => {
            tracing::warn!(code, "dropping a state outside the documented 0..=9 range");
            report.values_dropped += 1;
            None
        }
    }
}

/// Free text. Only `beer_ann` and `fridge_ann` are ever blank in a real log, and blank
/// means "nothing happened", not "unknown".
fn annotation(record: &csv::StringRecord, index: Option<usize>) -> Option<String> {
    let text = record.get(index?)?;
    (!text.is_empty()).then(|| text.to_owned())
}

// ---------------------------------------------------------------------------
// Line boundaries
// ---------------------------------------------------------------------------

/// Every record boundary in `bytes`, as an offset just past a terminator.
///
/// A `\r\n` inside a quoted annotation is **not** a boundary: Python's writer quotes
/// any field containing `,` `"` `\r` or `\n`, so a multi-line annotation is reachable,
/// and a naive `rfind` for `\r\n` would cut a record in half.
fn boundaries(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    let mut in_quotes = false;
    bytes.iter().enumerate().filter_map(move |(i, byte)| {
        match byte {
            // An escaped `""` toggles twice and lands back inside the field, which is
            // the correct answer.
            b'"' => {
                in_quotes = !in_quotes;
                None
            }
            // Keyed on `\n`: `\r\n` ends with it, and a lone `\n` from a hand-edited
            // file still terminates a record as far as the csv reader is concerned.
            b'\n' if !in_quotes => Some(i + 1),
            _ => None,
        }
    })
}

/// Split a buffer into `(complete records, trailing partial record)`.
///
/// **Split before you parse.** The `csv` crate treats EOF as a record terminator, so a
/// trailing half-written row parses as a complete one and `bytes_consumed` claims bytes
/// Fermentrack never finished writing. Both failures are silent and neither is
/// detectable afterwards: a row cut at `,0,C,1` has the right cell count and a valid
/// `log_time`, so it commits as a real reading while the genuine row's tail arrives
/// next poll as a one-cell record and is skipped; and Rust's `f64` parser accepts
/// `18.`, so a value cut mid-number stores 18.0 where the file says 18.06.
///
/// Phase 2d keeps no remainder buffer — `byte_offset` only ever advances past a
/// complete record, so the tail is re-requested for free by the next
/// `Range: bytes=<offset>-`. This is what makes that safe.
///
/// `bytes` must start on a record boundary — otherwise the quote state is inverted
/// from the first byte and every boundary after it is wrong. That is 2d's invariant to
/// keep, and the reason a resume that cannot prove it refetches from zero.
pub fn split_complete_lines(bytes: &[u8]) -> (&[u8], &[u8]) {
    bytes.split_at(boundaries(bytes).last().unwrap_or(0))
}

/// Split the first complete record off a buffer: `(the record including its
/// terminator, the rest)`, or `None` when no record is complete.
///
/// Quote-aware for the same reason [`split_complete_lines`] is, and subject to the same
/// start-on-a-boundary invariant.
///
/// Phase 2d uses this twice. Once to lift the header line off a fetch that began at
/// byte 0, so that [`parse_with_header`] handles the first chunk and every continuation
/// alike and `bytes_consumed` means the same thing in both. Once to prove a resume
/// chunk really does begin on a record boundary, by re-parsing its first record alone.
pub fn split_first_record(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    boundaries(bytes).next().map(|at| bytes.split_at(at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fermentrack::DETAIL_CHARS;

    /// `_full.csv` without the gravity columns, exactly as `Beer.column_headers`
    /// builds it.
    const HEADER: &str = "log_time,beer_temp,beer_set,beer_ann,fridge_temp,fridge_set,\
                          fridge_ann,room_temp,state,temp_format,associated_beer_id";

    /// With `gravity_enabled`: appended after `associated_beer_id`, which is the
    /// opposite of where `_graph.csv` puts them.
    const HEADER_GRAVITY: &str = "log_time,beer_temp,beer_set,beer_ann,fridge_temp,\
                                  fridge_set,fridge_ann,room_temp,state,temp_format,\
                                  associated_beer_id,gravity,grav_temp";

    /// Fermentrack writes CRLF, including on the last row.
    fn log(header: &str, rows: &[&str]) -> Vec<u8> {
        let mut out = format!("{header}\r\n");
        for row in rows {
            out.push_str(row);
            out.push_str("\r\n");
        }
        out.into_bytes()
    }

    fn parse(header: &str, rows: &[&str]) -> ParsedChunk {
        parse_full_log("test_full.csv", &log(header, rows)).expect("should parse")
    }

    fn only_row(header: &str, row: &str) -> LogRow {
        let chunk = parse(header, &[row]);
        assert_eq!(chunk.rows.len(), 1, "{:?}", chunk.report);
        chunk.rows.into_iter().next().unwrap()
    }

    /// Conversions are a linear map, so a relative tolerance is the honest comparison:
    /// `(64.4 - 32) * 5 / 9` is `18.000000000000004`, not `18.0`.
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
    }

    // -- inspect_header (Phase 2f's validation probe) ------------------------

    /// The probe reads 2 KB of a log that may be megabytes, so it must decide from the
    /// header alone — and must hand back the exact header line `sync_state` stores, so
    /// that a validated URL can be synced without refetching it.
    #[test]
    fn a_header_alone_identifies_a_full_log() {
        let info = inspect_header("x_full.csv", &log(HEADER, &["nonsense"])).expect("a full log");
        assert_eq!(info.header, HEADER);
        assert!(!info.gravity_enabled);
        assert!(info.unknown_columns.is_empty());

        let info = inspect_header("x_full.csv", &log(HEADER_GRAVITY, &[])).expect("a full log");
        assert!(
            info.gravity_enabled,
            "a gravity column means a tilt is fitted"
        );
    }

    /// The whole reason the probe exists. These are three different mistakes — wrong
    /// file for the right beer, right file for the wrong kind of device, and not
    /// Fermentrack at all — and the settings pane branches on `code()` to say so.
    #[test]
    fn the_three_refusals_keep_their_own_codes() {
        let graph = "log_time,beer_temp,beer_set,fridge_temp,fridge_set,room_temp,state";
        let err = inspect_header("b_graph.csv", &log(graph, &[])).unwrap_err();
        assert_eq!(err.code(), "fermentrack.graph_log");
        assert!(err.to_string().contains("b_graph.csv"), "{err}");

        let gravity = "log_time,gravity,grav_temp";
        let err = inspect_header("Gravity Device 2 - L5 - Tilt_full.csv", &log(gravity, &[]))
            .unwrap_err();
        assert_eq!(err.code(), "fermentrack.gravity_log");

        let err = inspect_header("index.html", &log("a,b,c", &[])).unwrap_err();
        assert_eq!(err.code(), "fermentrack.not_a_log");
    }

    /// A 2 KB prefix ends mid-row. The `csv` crate treats EOF as a terminator, so
    /// `parse_full_log` on that buffer would report a row Fermentrack never finished
    /// writing; `inspect_header` must not look at rows at all.
    #[test]
    fn a_prefix_cut_mid_row_still_reads_its_header() {
        let mut bytes = log(
            HEADER,
            &["2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,0,0,C,17"],
        );
        bytes.truncate(HEADER.len() + 2 + 20);
        let info = inspect_header("x_full.csv", &bytes).expect("the header is complete");
        assert_eq!(info.header, HEADER);
    }

    /// A header with no terminator was cut by our own prefix cap, so we never saw the
    /// whole column list. Classifying it would refuse good logs on a slow link.
    #[test]
    fn a_header_that_never_ends_is_refused_rather_than_guessed_at() {
        let err = inspect_header("x_full.csv", HEADER.as_bytes()).unwrap_err();
        assert_eq!(err.code(), "fermentrack.not_a_log");

        let err = inspect_header("x_full.csv", b"").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    /// A newer Fermentrack column must be reported, never fatal — the probe is what
    /// tells the user their box is ahead of this build.
    #[test]
    fn an_unknown_column_is_reported_not_refused() {
        let info = inspect_header("x_full.csv", &log(&format!("{HEADER},ph"), &[])).unwrap();
        assert_eq!(info.unknown_columns, vec!["ph".to_owned()]);
    }

    // -- the bare-zero rule -------------------------------------------------

    /// The rule the whole parser turns on. `x or 0` writes the integer `0` for a
    /// sensor that was never there; a genuine reading is `str(float)`.
    #[test]
    fn a_bare_zero_is_a_missing_reading_and_zero_point_zero_is_a_real_one() {
        let absent = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,0,0,C,17",
        );
        assert_eq!(absent.room_temp, None, "a bare 0 must not become 0 °C");

        let real = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,0.0,0,C,17",
        );
        assert_eq!(real.room_temp.map(Celsius::get), Some(0.0));
    }

    /// An absent sensor is data, not damage: it must not inflate `values_dropped`.
    #[test]
    fn an_absent_sensor_costs_nothing_in_the_report() {
        let chunk = parse(
            HEADER,
            &["2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,0,0,C,17"],
        );
        assert_eq!(chunk.report.values_dropped, 0);
        assert_eq!(chunk.report.rows_parsed, 1);
        assert_eq!(chunk.report.rows_skipped, 0);
    }

    /// The same rule in the column where breaking it is least visible: a 0.0 SG is
    /// outside `Sg::PLAUSIBLE` and would be dropped, but a bare `0` read as a real
    /// reading would sit in the series as a plausible-looking hole in the curve.
    #[test]
    fn a_bare_zero_gravity_is_a_missing_reading() {
        let absent = only_row(
            HEADER_GRAVITY,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,21.0,0,C,17,0,18.3",
        );
        assert_eq!(absent.gravity, None);

        // An absent gravity sensor is data, not damage.
        let chunk = parse(
            HEADER_GRAVITY,
            &["2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,21.0,0,C,17,0,18.3"],
        );
        assert_eq!(chunk.report.values_dropped, 0);

        // `0.0` is a real reading — implausible as an SG, so dropped and counted.
        let real = parse(
            HEADER_GRAVITY,
            &["2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,21.0,0,C,17,0.0,18.3"],
        );
        assert_eq!(real.rows[0].gravity, None);
        assert_eq!(real.report.values_dropped, 1);
    }

    /// `state` is a genuine integer and is exempt: `0` is IDLE, not "missing".
    #[test]
    fn a_zero_state_is_idle_not_absent() {
        let row = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,21.0,0,C,17",
        );
        assert_eq!(row.state, Some(ControllerState::Idle));
        assert_eq!(row.associated_beer_id, Some(17));
    }

    #[test]
    fn py_number_follows_pythons_str() {
        assert_eq!(py_number("0"), Ok(Cell::Absent));
        assert_eq!(py_number(""), Ok(Cell::Absent));
        assert_eq!(py_number("0.0"), Ok(Cell::Real(0.0)));
        assert_eq!(py_number("18.06"), Ok(Cell::Real(18.06)));
        assert_eq!(py_number("-0.5"), Ok(Cell::Real(-0.5)));
        assert_eq!(py_number("1e-3"), Ok(Cell::Real(0.001)));
        // Readable, so taken, with a debug line. Fermentrack cannot emit it.
        assert_eq!(py_number("18"), Ok(Cell::Real(18.0)));
        assert_eq!(py_number("nan"), Err(()));
        assert_eq!(py_number("inf"), Err(()));
        assert_eq!(py_number("--.-"), Err(()));
    }

    // -- per-row temp_format ------------------------------------------------

    /// The `Beer` caches its own format and it drifts from the device's, so one file
    /// really does carry both. 18.0 °C and 64.4 °F are the same temperature.
    #[test]
    fn each_row_is_converted_with_its_own_temp_format() {
        let chunk = parse(
            HEADER,
            &[
                "2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17",
                "2025/09/18 00:00:30Z,64.4,64.4,,62.6,62.6,,69.8,0,F,17",
            ],
        );
        assert_eq!(chunk.rows.len(), 2);
        assert_eq!(chunk.rows[0].temp_format, TempScale::C);
        assert_eq!(chunk.rows[1].temp_format, TempScale::F);

        for row in &chunk.rows {
            let beer = row.beer_temp.unwrap().get();
            assert!(close(beer, 18.0), "{beer} should be 18 °C");
        }
        assert!(close(chunk.rows[1].room_temp.unwrap().get(), 21.0));
    }

    /// Gravity is scale-free: the same SG on a Fahrenheit row.
    #[test]
    fn a_temp_format_switch_does_not_touch_gravity() {
        let row = only_row(
            HEADER_GRAVITY,
            "2025/09/18 00:00:00Z,64.4,64.4,,62.6,62.6,,69.8,0,F,17,1.043,64.9",
        );
        assert_eq!(row.gravity.map(Sg::get), Some(1.043));
        assert!(close(row.grav_temp.unwrap().get(), 18.277_777_777_777_78));
    }

    #[test]
    fn a_row_with_an_unreadable_temp_format_is_skipped() {
        let chunk = parse(
            HEADER,
            &["2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,K,17"],
        );
        assert!(chunk.rows.is_empty());
        assert_eq!(chunk.report.rows_skipped, 1);
    }

    // -- quoting ------------------------------------------------------------

    /// `split(',')` eats this row. An annotation is free text and Python's writer
    /// quotes it minimally.
    #[test]
    fn an_annotation_containing_a_comma_survives_whole() {
        let row = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,\"Temp profile started, target 18.0C\",\
             17.0,17.0,,21.0,0,C,17",
        );
        assert_eq!(
            row.beer_ann.as_deref(),
            Some("Temp profile started, target 18.0C")
        );
        // The cells after it must not have shifted along by one.
        assert_eq!(row.state, Some(ControllerState::Idle));
        assert_eq!(row.associated_beer_id, Some(17));
    }

    #[test]
    fn a_doubled_quote_is_unescaped_to_one() {
        let row = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,\
             \"Fridge renamed to \"\"Chest 2\"\"\",21.0,0,C,17",
        );
        assert_eq!(
            row.fridge_ann.as_deref(),
            Some("Fridge renamed to \"Chest 2\"")
        );
        assert_eq!(row.associated_beer_id, Some(17));
    }

    /// Blank means "nothing happened", which is `None` rather than an empty string.
    #[test]
    fn a_blank_annotation_is_none_not_an_empty_string() {
        let row = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,21.0,0,C,17",
        );
        assert_eq!(row.beer_ann, None);
        assert_eq!(row.fridge_ann, None);
    }

    // -- header -------------------------------------------------------------

    /// A newer Fermentrack adding a column must not stop us ingesting the rest.
    #[test]
    fn an_unknown_column_is_recorded_and_ignored() {
        let header = format!("{HEADER},humidity");
        let chunk = parse(
            &header,
            &["2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,21.0,0,C,17,55.0"],
        );
        assert_eq!(chunk.report.unknown_columns, ["humidity"]);
        assert_eq!(chunk.rows.len(), 1);
        assert!(close(chunk.rows[0].beer_temp.unwrap().get(), 18.06));
    }

    /// Column presence, never `model_version`: the header is the same in v1 and v2,
    /// and the order the columns arrive in is not ours to assume either.
    #[test]
    fn columns_are_found_by_name_not_by_position() {
        let reordered = "associated_beer_id,temp_format,state,room_temp,fridge_ann,\
                         fridge_set,fridge_temp,beer_ann,beer_set,beer_temp,log_time";
        let row = only_row(
            reordered,
            "17,C,0,21.0,,17.0,17.0,,18.0,18.06,2025/09/18 00:00:00Z",
        );
        assert!(close(row.beer_temp.unwrap().get(), 18.06));
        assert!(close(row.beer_set.unwrap().get(), 18.0));
        assert_eq!(row.ts, Timestamp::from_secs(1_758_153_600));
    }

    /// No `log_time` at all: an HTML error page, or a URL pointing at the dashboard.
    #[test]
    fn a_body_without_log_time_is_not_a_fermentrack_log() {
        let err = parse_full_log("test_full.csv", b"<!DOCTYPE html><html><head>\r\n")
            .expect_err("HTML is not a log");
        assert_eq!(err.code(), "fermentrack.not_a_log");
        assert!(err.to_string().contains("DOCTYPE"), "{err}");
    }

    #[test]
    fn a_header_preview_is_truncated_rather_than_dumped() {
        let long = "x".repeat(DETAIL_CHARS * 3);
        let err = parse_full_log("test_full.csv", format!("{long}\r\n").as_bytes())
            .expect_err("no log_time");
        let message = err.to_string();
        assert!(message.contains('…'), "{message}");
        assert!(message.chars().count() < DETAIL_CHARS * 2, "{message}");
    }

    /// `temp_format` is the discriminator. Both `model_version`s of `_graph.csv` lack
    /// it, and taking one would store `0` for every missing reading as a real 0.0 °C.
    #[test]
    fn a_graph_log_is_refused_by_name() {
        for header in [
            // model_version 2
            "log_time,beer_temp,beer_set,fridge_temp,fridge_set,room_temp,state",
            // model_version 1 — no `state`
            "log_time,beer_temp,beer_set,fridge_temp,fridge_set,room_temp",
            // gravity_enabled: the gravity columns sit *before* `state` here
            "log_time,beer_temp,beer_set,fridge_temp,fridge_set,room_temp,gravity,\
             grav_temp,state",
        ] {
            let err = parse_full_log("B17_graph.csv", log(header, &[]).as_slice())
                .expect_err("a graph log must be refused");
            assert_eq!(err.code(), "fermentrack.graph_log", "{header}");
            assert!(err.to_string().contains("B17_graph.csv"), "{err}");
            assert!(err.to_string().contains("_full.csv"), "{err}");
        }
    }

    /// A `GravityLog`'s own `_full.csv`: it has `log_time` and `gravity` but none of
    /// the controller columns. Calling it a `_graph.csv` would send the user looking
    /// for a file that does not exist — 2f lets them paste this URL.
    #[test]
    fn a_gravity_sensor_log_is_named_as_one() {
        let header = "log_time,gravity,temp,temp_format,associated_beer_id";
        let err = parse_full_log("Gravity Device 1 - L3 - Tilt_full.csv", &log(header, &[]))
            .expect_err("a gravity log is not a beer log");
        assert_eq!(err.code(), "fermentrack.gravity_log");
        assert!(err.to_string().contains("Gravity Device 1"), "{err}");
    }

    /// `log_time` alone is not enough to call something a Fermentrack log. Without the
    /// controller columns and without `gravity` there is nothing to guess from, so say
    /// that rather than blaming the wrong file.
    #[test]
    fn a_csv_with_only_log_time_is_not_classified_as_either_log() {
        let err = parse_full_log("mystery.csv", &log("log_time,humidity,pressure", &[]))
            .expect_err("not a Fermentrack log");
        assert_eq!(err.code(), "fermentrack.not_a_log");
        assert!(err.to_string().contains("humidity"), "{err}");
    }

    // -- rows ---------------------------------------------------------------

    #[test]
    fn log_time_parses_as_utc_and_rejects_trailing_garbage() {
        assert_eq!(
            parse_log_time("2025/09/18 00:00:00Z"),
            Some(Timestamp::from_secs(1_758_153_600))
        );
        assert_eq!(
            parse_log_time("1970/01/01 00:00:00Z"),
            Some(Timestamp::UNIX_EPOCH)
        );
        // The leap day two of the three calendar rules get wrong.
        assert_eq!(
            parse_log_time("2024/02/29 00:00:00Z"),
            Some(Timestamp::from_secs(1_709_164_800))
        );
        assert_eq!(parse_log_time("2025/09/18 00:00:00Z junk"), None);
        assert_eq!(parse_log_time("2025-09-18 00:00:00Z"), None);
        assert_eq!(parse_log_time("2025/09/18 00:00:00"), None);
        assert_eq!(parse_log_time("2025/02/30 00:00:00Z"), None);
        assert_eq!(parse_log_time("2025/13/01 00:00:00Z"), None);
        assert_eq!(parse_log_time(""), None);
    }

    /// A timestamp is what places a row on a timeline, so there is nothing worth
    /// keeping without one — but the rows around it are still good.
    #[test]
    fn a_row_with_an_unusable_log_time_is_skipped_and_the_rest_survive() {
        let chunk = parse(
            HEADER,
            &[
                "2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17",
                "not a timestamp,18.1,18.0,,17.0,17.0,,21.0,0,C,17",
                "2025/09/18 00:01:00Z,18.2,18.0,,17.0,17.0,,21.0,0,C,17",
            ],
        );
        assert_eq!(chunk.rows.len(), 2);
        assert_eq!(chunk.report.rows_parsed, 2);
        assert_eq!(chunk.report.rows_skipped, 1);
        assert_eq!(chunk.rows[1].ts, Timestamp::from_secs(1_758_153_660));
    }

    #[test]
    fn a_row_with_the_wrong_cell_count_is_skipped() {
        let chunk = parse(
            HEADER,
            &[
                "2025/09/18 00:00:00Z,18.0,18.0,,17.0",
                "2025/09/18 00:00:30Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17",
                "2025/09/18 00:01:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17,extra",
            ],
        );
        assert_eq!(chunk.rows.len(), 1);
        assert_eq!(chunk.report.rows_skipped, 2);
    }

    #[test]
    fn a_state_outside_the_documented_range_is_dropped_not_fatal() {
        let row = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,11,C,17",
        );
        assert_eq!(row.state, None);
        // The rest of the row is intact.
        assert!(close(row.beer_temp.unwrap().get(), 18.0));
    }

    /// `Sg::PLAUSIBLE` catches BUILDOUT's specific warning: reading gravity out of the
    /// `state` column and putting 4.0 in the SG series.
    #[test]
    fn an_implausible_gravity_is_dropped_and_counted() {
        let chunk = parse(
            HEADER_GRAVITY,
            &["2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,4,C,17,4.0,18.3"],
        );
        assert_eq!(chunk.rows[0].gravity, None);
        assert_eq!(chunk.report.values_dropped, 1);
    }

    #[test]
    fn an_implausible_temperature_is_dropped_and_counted() {
        let chunk = parse(
            HEADER,
            &["2025/09/18 00:00:00Z,999.0,18.0,,17.0,17.0,,21.0,0,C,17"],
        );
        assert_eq!(chunk.rows[0].beer_temp, None);
        assert_eq!(chunk.report.values_dropped, 1);
        assert!(close(chunk.rows[0].beer_set.unwrap().get(), 18.0));
    }

    // -- fan-out ------------------------------------------------------------

    /// One CSV row becomes up to eight `reading` rows. Phase 2e loops over this.
    #[test]
    fn a_full_row_fans_out_to_eight_metrics() {
        let row = only_row(
            HEADER_GRAVITY,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,16.5,,21.0,4,C,17,1.043,18.3",
        );
        let readings: Vec<_> = row.readings().collect();
        assert_eq!(readings.len(), 8);

        let by_metric = |wanted: Metric| {
            readings
                .iter()
                .find(|(_, m, _)| *m == wanted)
                .map(|(_, _, v)| *v)
                .unwrap()
        };
        assert!(close(by_metric(Metric::TempC), 18.06));
        assert!(close(by_metric(Metric::SetpointC), 18.0));
        assert!(close(by_metric(Metric::FridgeTempC), 17.0));
        assert!(close(by_metric(Metric::FridgeSetpointC), 16.5));
        assert!(close(by_metric(Metric::RoomTempC), 21.0));
        assert!(close(by_metric(Metric::Sg), 1.043));
        assert!(close(by_metric(Metric::GravityTempC), 18.3));
        // The state's integer, not its label.
        assert_eq!(by_metric(Metric::State), 4.0);
    }

    /// Absent metrics are absent from the fan-out, so 2e never writes a placeholder.
    #[test]
    fn absent_metrics_do_not_appear_in_the_fan_out() {
        let row = only_row(
            HEADER,
            "2025/09/18 00:00:00Z,18.06,18.0,,17.0,17.0,,0,0,C,17",
        );
        let metrics: Vec<Metric> = row.readings().map(|(_, m, _)| m).collect();
        assert!(!metrics.contains(&Metric::RoomTempC));
        assert!(!metrics.contains(&Metric::Sg));
        assert!(!metrics.contains(&Metric::GravityTempC));
        assert_eq!(metrics.len(), 5);
    }

    // -- continuation chunks ------------------------------------------------

    /// Phase 2d's shape: the header lives in `sync_state`, the body is whatever the
    /// `Range` request returned.
    #[test]
    fn a_continuation_chunk_is_parsed_against_a_stored_header() {
        let body = "2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17\r\n\
                    2025/09/18 00:00:30Z,18.1,18.0,,17.0,17.0,,21.0,0,C,17\r\n";
        let chunk =
            parse_with_header("test_full.csv", HEADER, body.as_bytes()).expect("should parse");
        assert_eq!(chunk.rows.len(), 2);
        assert_eq!(chunk.rows[0].ts, Timestamp::from_secs(1_758_153_600));
    }

    /// The stored header keeps its line terminator; it must not become a field.
    #[test]
    fn a_stored_header_may_carry_its_crlf() {
        let chunk = parse_with_header(
            "test_full.csv",
            &format!("{HEADER}\r\n"),
            b"2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17\r\n",
        )
        .expect("should parse");
        assert_eq!(chunk.rows.len(), 1);
        assert!(chunk.report.unknown_columns.is_empty());
    }

    // -- stopping early -----------------------------------------------------

    /// What 2d's offset advance is computed from. A clean chunk consumes all of it.
    #[test]
    fn a_clean_chunk_consumes_every_byte_it_was_given() {
        let body = "2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17\r\n\
                    2025/09/18 00:00:30Z,18.1,18.0,,17.0,17.0,,21.0,0,C,17\r\n";
        let chunk = parse_with_header("test_full.csv", HEADER, body.as_bytes()).unwrap();
        assert!(!chunk.report.stopped_early);
        assert_eq!(chunk.report.bytes_consumed, body.len());

        // With the header included, the offset covers the header line too.
        let whole = log(
            HEADER,
            &["2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17"],
        );
        let chunk = parse_full_log("test_full.csv", &whole).unwrap();
        assert_eq!(chunk.report.bytes_consumed, whole.len());
    }

    /// A fault mid-chunk must leave `bytes_consumed` at the last record that read
    /// cleanly. If it counted the whole chunk, 2d would advance past bytes it never
    /// parsed and those rows would be gone for good.
    #[test]
    fn a_fault_mid_chunk_stops_early_without_claiming_the_rest() {
        let good = b"2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17\r\n";
        let mut body = good.to_vec();
        // Not UTF-8. An annotation is free text, so a chunk that was cut in the wrong
        // place really can put a truncated multi-byte sequence in one.
        body.extend_from_slice(
            b"2025/09/18 00:00:30Z,18.1,18.0,\xff\xfe,17.0,17.0,,21.0,0,C,17\r\n",
        );
        body.extend_from_slice(b"2025/09/18 00:01:00Z,18.2,18.0,,17.0,17.0,,21.0,0,C,17\r\n");

        let chunk = parse_with_header("test_full.csv", HEADER, &body).unwrap();

        assert_eq!(chunk.rows.len(), 1);
        assert!(chunk.report.stopped_early);
        assert_eq!(chunk.report.rows_skipped, 1);
        assert_eq!(chunk.report.bytes_consumed, good.len());
        // The third row is untouched, not skipped: 2d refetches from the fault.
        assert!(chunk.report.bytes_consumed < body.len());
    }

    // -- line boundaries ----------------------------------------------------

    #[test]
    fn split_complete_lines_carries_a_partial_row_forward() {
        let (complete, remainder) = split_complete_lines(b"a,b\r\nc,d\r\ne,");
        assert_eq!(complete, b"a,b\r\nc,d\r\n");
        assert_eq!(remainder, b"e,");
    }

    #[test]
    fn split_complete_lines_handles_the_empty_and_whole_cases() {
        assert_eq!(split_complete_lines(b""), (&b""[..], &b""[..]));
        assert_eq!(
            split_complete_lines(b"a,b\r\n"),
            (&b"a,b\r\n"[..], &b""[..])
        );
        // No terminator at all: nothing is complete yet.
        assert_eq!(split_complete_lines(b"a,b"), (&b""[..], &b"a,b"[..]));
    }

    /// The case a naive `rfind` for CRLF gets wrong. Python's writer quotes any field
    /// containing `\r` or `\n`, so a multi-line annotation is reachable — and cutting
    /// a record in half here would advance `byte_offset` past the wreckage.
    #[test]
    fn a_crlf_inside_a_quoted_annotation_is_not_a_record_boundary() {
        let bytes = b"a,\"two\r\nlines\",b\r\nc,d,";
        let (complete, remainder) = split_complete_lines(bytes);
        assert_eq!(complete, b"a,\"two\r\nlines\",b\r\n");
        assert_eq!(remainder, b"c,d,");
    }

    #[test]
    fn split_first_record_stops_at_the_first_unquoted_terminator() {
        let (first, rest) =
            split_first_record(b"a,b\r\nc,d\r\ne,").expect("one record is complete");
        assert_eq!(first, b"a,b\r\n");
        assert_eq!(rest, b"c,d\r\ne,");

        // A quoted terminator is not a boundary here either.
        let (first, rest) = split_first_record(b"a,\"two\r\nlines\",b\r\nc,d\r\n").unwrap();
        assert_eq!(first, b"a,\"two\r\nlines\",b\r\n");
        assert_eq!(rest, b"c,d\r\n");

        // One terminated record leaves nothing behind.
        assert_eq!(
            split_first_record(b"a,b\r\n"),
            Some((&b"a,b\r\n"[..], &b""[..]))
        );

        // Nothing is complete yet.
        assert_eq!(split_first_record(b"a,b"), None);
        assert_eq!(split_first_record(b""), None);
    }

    /// A doubled quote returns the scanner to "inside the field", not outside it.
    #[test]
    fn an_escaped_quote_does_not_confuse_the_boundary_scan() {
        let bytes = b"a,\"he said \"\"hi\"\"\r\nstill inside\",b\r\nnext";
        let (complete, remainder) = split_complete_lines(bytes);
        assert_eq!(complete, b"a,\"he said \"\"hi\"\"\r\nstill inside\",b\r\n");
        assert_eq!(remainder, b"next");
    }

    /// What `split_complete_lines` hands back must itself parse cleanly, and the two
    /// halves must account for every byte — the tail is re-fetched, not carried, so a
    /// byte lost here is a byte lost from the file.
    #[test]
    fn a_split_chunk_round_trips_through_the_parser() {
        let whole = "2025/09/18 00:00:00Z,18.0,18.0,,17.0,17.0,,21.0,0,C,17\r\n\
                     2025/09/18 00:00:30Z,18.1,18.0,,17.0,17.0,,21.0,0,C,17\r\n\
                     2025/09/18 00:01:00Z,18.2,18.0,";
        let (complete, remainder) = split_complete_lines(whole.as_bytes());

        let chunk = parse_with_header("test_full.csv", HEADER, complete).expect("should parse");
        assert_eq!(chunk.rows.len(), 2);
        assert_eq!(chunk.report.rows_skipped, 0);
        assert_eq!(remainder, b"2025/09/18 00:01:00Z,18.2,18.0,");
        assert_eq!(complete.len() + remainder.len(), whole.len());
    }
}
