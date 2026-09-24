//! Phase 3e. Manual measurements: what a human typed, and the number it became.
//!
//! **The entry record is Tier 2 and the derived value is Tier 1.** `reading` is
//! `(stream_id, ts, value)` and has to stay that shape; a manual reading is typed once
//! and is never reconstructible. So [`record_measurement`] writes a `manual_measurement`
//! row *and*, in the same transaction, one `reading` through the batch's manual source.
//! The chart, `batch_stream` and Phase 4 then find it with no second read path.
//! `0006_manual.sql` argues the split at length.
//!
//! **The corrections never mix.** A refractometer reading after pitch goes through
//! Terrill's cubic and is not also temperature-corrected — the cubic is fitted against
//! reference-temperature hydrometer values and the instrument is ATC, so both would
//! double-count. A hydrometer reading is temperature-corrected and never sees the wort
//! correction factor. That is what [`derive_value`] is: the checklist's "do not silently
//! mix the two", as a decision tree that refuses rather than guesses.
//!
//! **A manual gravity does not write `batch.og` or `batch.fg`.** Decisive reason: the
//! post-pitch refractometer correction *reads* `batch.og`, so a reading that wrote it
//! back would corrupt every later correction, silently and cumulatively. OG is also the
//! gravity at pitch, and a `ts` does not say the user meant this one as OG.
//! `batch::set_batch_measurements` stays the deliberate act it documents itself as.
//!
//! Unlike every other `db::*` module the write functions take `&Transaction<'_>`, not
//! `&Connection`: this is the one place that writes both tiers, and
//! [`tier1::create_stream`] / [`tier1::insert_readings`] require a transaction. Nothing
//! else changes — `&Transaction` derefs to `&Connection`, so `timeline::add_event`
//! composes inside the same boundary.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{
    Metric,
    batch::{BatchRow, ensure_open, get_batch},
    enums::db_enum,
    tier1::{SourceKind, create_source, create_stream, insert_readings},
    timeline::{EventKind, NewBatchEvent, add_event, bind_stream, set_binding_start},
};
use crate::{
    Error, Result,
    calc::{
        brix_to_sg, correct_for_temperature, final_gravity_from_brix, original_gravity_from_brix,
        plato_to_sg,
    },
    units::{Celsius, GramsPerLitre, Ph, Sg, TempScale, Timestamp},
};

db_enum!(
    /// What took the reading. Optional on entry — a number off an unrecorded device is
    /// still a number — but it decides which correction runs, so a wrong one is not a
    /// cosmetic error.
    Instrument,
    UnknownInstrument,
    (Hydrometer, "hydrometer"),
    (Refractometer, "refractometer"),
    (PhMeter, "ph_meter"),
    (Thermometer, "thermometer"),
    (Titration, "titration"),
);

db_enum!(
    /// The scale the human typed in. Rule 1c's entry half: `manual_measurement.value`
    /// is canonical and `(entry_value, entry_unit)` is what was on the instrument.
    EntryUnit,
    UnknownEntryUnit,
    (Sg, "sg"),
    (Plato, "plato"),
    (Brix, "brix"),
    (Celsius, "c"),
    (Fahrenheit, "f"),
    (Ph, "ph"),
    (GramsPerLitre, "g_per_l"),
);

impl EntryUnit {
    /// The one metric this unit can express. Brix is a gravity and nothing else; °C is a
    /// temperature and nothing else. Making it a function rather than a lookup table at
    /// the call site is what makes "Brix of a pH" a refusal instead of a guess about
    /// which half of the pair was the typo.
    pub fn metric(self) -> Metric {
        match self {
            EntryUnit::Sg | EntryUnit::Plato | EntryUnit::Brix => Metric::Sg,
            EntryUnit::Celsius | EntryUnit::Fahrenheit => Metric::TempC,
            EntryUnit::Ph => Metric::Ph,
            EntryUnit::GramsPerLitre => Metric::Ta,
        }
    }
}

impl Instrument {
    /// The one metric this device reads.
    pub fn metric(self) -> Metric {
        match self {
            Instrument::Hydrometer | Instrument::Refractometer => Metric::Sg,
            Instrument::PhMeter => Metric::Ph,
            Instrument::Thermometer => Metric::TempC,
            Instrument::Titration => Metric::Ta,
        }
    }
}

/// One reading as typed. The corrections are inputs, not requests: supplying both
/// temperatures *is* asking for the hydrometer correction, and supplying a wort
/// correction factor *is* asking for the refractometer one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewMeasurement {
    pub metric: Metric,
    pub entry_value: f64,
    pub entry_unit: EntryUnit,
    #[serde(default)]
    pub instrument: Option<Instrument>,
    #[serde(default)]
    pub sample_temp_c: Option<Celsius>,
    #[serde(default)]
    pub calibration_temp_c: Option<Celsius>,
    #[serde(default)]
    pub wort_correction: Option<f64>,
}

/// Everything from one moment at the bench. The checklist's "two clicks from the batch":
/// gravity, pH, temp, TA and a taste note in one call, at one `ts`.
///
/// One timestamp for all of them on purpose — two readings taken together must not land
/// a second apart, or Phase 4's chart draws a trend out of the typing order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuickEntry {
    pub ts: Timestamp,
    pub measurements: Vec<NewMeasurement>,
    /// Goes to `batch_event` as an [`EventKind::Note`], not to a column here: a taste
    /// note is something you observed, not a number on a time series.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MeasurementRow {
    pub id: i64,
    pub batch_id: i64,
    pub stream_id: i64,
    pub ts: Timestamp,
    pub metric: Metric,
    /// Canonical and already corrected: SG, °C, pH or g/L as tartaric acid, by `metric`.
    pub value: f64,
    pub entry_value: f64,
    pub entry_unit: EntryUnit,
    pub instrument: Option<Instrument>,
    pub sample_temp_c: Option<Celsius>,
    pub calibration_temp_c: Option<Celsius>,
    pub wort_correction: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuickEntryResult {
    pub measurements: Vec<MeasurementRow>,
    /// `None` when there was no note. A note is not a measurement and does not pretend
    /// to be one.
    pub event_id: Option<i64>,
}

const MEASUREMENT_COLUMNS: &str = "id, batch_id, stream_id, ts, metric, value, entry_value, \
     entry_unit, instrument, sample_temp_c, calibration_temp_c, wort_correction";

fn measurement_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MeasurementRow> {
    Ok(MeasurementRow {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        stream_id: row.get(2)?,
        ts: row.get(3)?,
        metric: row.get(4)?,
        value: row.get(5)?,
        entry_value: row.get(6)?,
        entry_unit: row.get(7)?,
        instrument: row.get(8)?,
        sample_temp_c: row.get(9)?,
        calibration_temp_c: row.get(10)?,
        wort_correction: row.get(11)?,
    })
}

// ------------------------------------------------------------ source and streams

/// `stream.external_id` for a batch's manual series. MQTT-shaped, because v2's broker
/// topics will want the same shape and a manual stream is the first thing that is not
/// Fermentrack's. Nothing outside this module parses it; it is pinned by a test because
/// changing it would orphan every existing manual stream.
fn manual_external_id(batch_id: i64, metric: Metric) -> String {
    format!("manual/{batch_id}/{metric}")
}

/// The batch's manual source, if it has one.
///
/// Found through the `batch_stream` join and not through `source.config`: Tier 1 must
/// not reference Tier 2, so there is no `batch_id` column on `source` to look at, and
/// the JSON is a record rather than an index. Total, because
/// [`ensure_manual_source`] never creates a source without binding a stream to the batch
/// in the same transaction.
pub fn manual_source_for(conn: &Connection, batch_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT src.id FROM batch_stream bs \
             JOIN stream st ON st.id = bs.stream_id \
             JOIN source src ON src.id = st.source_id \
             WHERE bs.batch_id = ?1 AND src.kind = ?2 \
             LIMIT 1",
            params![batch_id, SourceKind::Manual],
            |row| row.get(0),
        )
        .optional()?)
}

fn ensure_manual_source(tx: &Transaction<'_>, batch: &BatchRow) -> Result<i64> {
    if let Some(source_id) = manual_source_for(tx, batch.id)? {
        return Ok(source_id);
    }
    let config = format!("{{\"batch_id\":{}}}", batch.id);
    create_source(
        tx,
        SourceKind::Manual,
        &format!("Manual ({})", batch.code),
        Some(&config),
    )
}

/// The stream for one metric of one batch's manual source, created and bound on first
/// use.
///
/// `create_stream` is already idempotent, so the work here is the binding. A new one
/// starts at the earlier of this reading and the pitch, because a pre-pitch OG is a
/// measurement of this batch however long before the yeast went in. An existing one is
/// widened when a backdated reading arrives — routinely, since the first measurement
/// *typed* is not usually the earliest one *taken*, and a reading outside its own
/// window is invisible to `batch_for_stream_at`.
fn ensure_manual_stream(
    tx: &Transaction<'_>,
    batch: &BatchRow,
    source_id: i64,
    metric: Metric,
    ts: Timestamp,
) -> Result<i64> {
    let external_id = manual_external_id(batch.id, metric);
    let stream_id = create_stream(tx, source_id, &external_id, metric)?;

    let binding: Option<i64> = tx
        .query_row(
            "SELECT id FROM batch_stream WHERE stream_id = ?1 AND batch_id = ?2 \
             ORDER BY starts_at LIMIT 1",
            params![stream_id, batch.id],
            |row| row.get(0),
        )
        .optional()?;

    match binding {
        Some(binding_id) => {
            set_binding_start(tx, binding_id, ts)?;
        }
        None => {
            let starts_at = match batch.pitched_at {
                Some(pitched) if pitched < ts => pitched,
                _ => ts,
            };
            bind_stream(tx, batch.id, stream_id, starts_at, None)?;
        }
    }
    Ok(stream_id)
}

// ------------------------------------------------------------------- derivation

/// What was typed, into what gets stored. Every refusal in here is a case where the
/// formulas would otherwise answer confidently and wrongly.
fn derive_value(batch: &BatchRow, ts: Timestamp, entry: &NewMeasurement) -> Result<f64> {
    let metric = entry.metric;

    if !entry.entry_value.is_finite() {
        return Err(Error::NotFinite {
            field: "measurement",
            value: entry.entry_value,
        });
    }
    if entry.entry_unit.metric() != metric {
        return Err(Error::MeasurementUnitMismatch {
            metric,
            unit: entry.entry_unit,
        });
    }
    if let Some(instrument) = entry.instrument
        && instrument.metric() != metric
    {
        return Err(Error::MeasurementInstrumentMismatch { metric, instrument });
    }

    match metric {
        Metric::Sg => derive_gravity(batch, ts, entry).map(Sg::get),
        Metric::TempC => {
            let scale = match entry.entry_unit {
                EntryUnit::Celsius => TempScale::C,
                _ => TempScale::F,
            };
            let celsius = scale.to_celsius(entry.entry_value)?;
            if !celsius.is_plausible() {
                return Err(Error::MeasurementImplausible {
                    metric,
                    value: celsius.get(),
                });
            }
            Ok(celsius.get())
        }
        Metric::Ph => {
            let ph = Ph::new(entry.entry_value)?;
            if !ph.is_plausible() {
                return Err(Error::MeasurementImplausible {
                    metric,
                    value: ph.get(),
                });
            }
            Ok(ph.get())
        }
        Metric::Ta => {
            let ta = GramsPerLitre::new(entry.entry_value)?;
            if !ta.is_plausible() {
                return Err(Error::MeasurementImplausible {
                    metric,
                    value: ta.get(),
                });
            }
            Ok(ta.get())
        }
        // Every remaining metric is a controller series, and no `EntryUnit` names one,
        // so the unit check above has already rejected this. Repeating the refusal
        // rather than `unreachable!` keeps a future `Metric` from panicking.
        _ => Err(Error::MeasurementUnitMismatch {
            metric,
            unit: entry.entry_unit,
        }),
    }
}

/// The gravity branch: two instruments, two corrections, and no path that applies both.
fn derive_gravity(batch: &BatchRow, ts: Timestamp, entry: &NewMeasurement) -> Result<Sg> {
    let sg = match entry.instrument {
        Some(Instrument::Refractometer) => {
            // Brix only. An SG off a refractometer is the instrument's own uncorrected
            // conversion — accepting it would mean correcting a number that has already
            // been through a conversion we cannot see. Plato is refused for the same
            // reason: Terrill's OB/FB are defined as the reading divided by the wort
            // factor, and a scale that claims to be extract already is not that.
            if entry.entry_unit != EntryUnit::Brix {
                return Err(Error::MeasurementInstrumentMismatch {
                    metric: entry.metric,
                    instrument: Instrument::Refractometer,
                });
            }
            // The wort correction factor is instrument-specific and 1.0 for a sugar
            // wash, so there is no default to fall back on; see
            // `calc::refractometer::WORT_CORRECTION_DEFAULT`. The temperatures belong to
            // the hydrometer correction and would double-count against the cubic.
            let Some(wort_correction) = entry.wort_correction else {
                return Err(Error::MeasurementInstrumentMismatch {
                    metric: entry.metric,
                    instrument: Instrument::Refractometer,
                });
            };
            if entry.sample_temp_c.is_some() || entry.calibration_temp_c.is_some() {
                return Err(Error::MeasurementInstrumentMismatch {
                    metric: entry.metric,
                    instrument: Instrument::Refractometer,
                });
            }

            let post_pitch = batch.pitched_at.is_some_and(|pitched| ts >= pitched);
            if post_pitch {
                let og = batch
                    .og
                    .ok_or(Error::NoOriginalGravity { batch_id: batch.id })?;
                final_gravity_from_brix(og, entry.entry_value, wort_correction)?
            } else {
                original_gravity_from_brix(entry.entry_value, wort_correction)?
            }
        }
        instrument => {
            // A wort correction factor on a hydrometer is a form filled in against the
            // wrong instrument. Ignoring it silently would leave the user believing a
            // correction ran.
            if entry.wort_correction.is_some() {
                return Err(Error::MeasurementInstrumentMismatch {
                    metric: entry.metric,
                    instrument: instrument.unwrap_or(Instrument::Hydrometer),
                });
            }

            let raw = match entry.entry_unit {
                EntryUnit::Sg => Sg::new(entry.entry_value)?,
                EntryUnit::Plato => plato_to_sg(entry.entry_value)?,
                _ => brix_to_sg(entry.entry_value)?,
            };
            // Both temperatures or neither. One alone does not say what to correct
            // against, and assuming the other is how a reading picks up a correction
            // nobody asked for.
            match (entry.sample_temp_c, entry.calibration_temp_c) {
                (Some(sample), Some(calibration)) => {
                    correct_for_temperature(raw, sample, calibration)?
                }
                _ => raw,
            }
        }
    };

    if !sg.is_plausible() {
        return Err(Error::MeasurementImplausible {
            metric: entry.metric,
            value: sg.get(),
        });
    }
    Ok(sg)
}

// ------------------------------------------------------------------------ writes

/// Record one measurement: the entry row in Tier 2 and its `reading` in Tier 1.
///
/// Order matters. The value is derived before anything is created, so a rejected entry
/// leaves no orphan source or stream behind even inside a transaction the caller goes on
/// to commit.
pub fn record_measurement(
    tx: &Transaction<'_>,
    batch_id: i64,
    ts: Timestamp,
    entry: &NewMeasurement,
) -> Result<MeasurementRow> {
    let batch = get_batch(tx, batch_id)?;
    ensure_open(&batch)?;

    let value = derive_value(&batch, ts, entry)?;
    let source_id = ensure_manual_source(tx, &batch)?;
    let stream_id = ensure_manual_stream(tx, &batch, source_id, entry.metric, ts)?;

    // `insert_readings` is documented as `ON CONFLICT DO NOTHING`, so a count of zero
    // *is* the duplicate detection — no second `SELECT`, and no window between the two.
    if insert_readings(tx, &[(stream_id, ts, value)])? == 0 {
        return Err(Error::MeasurementExists {
            metric: entry.metric,
            ts,
        });
    }

    tx.execute(
        "INSERT INTO manual_measurement \
         (batch_id, stream_id, ts, metric, value, entry_value, entry_unit, instrument, \
          sample_temp_c, calibration_temp_c, wort_correction) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            batch_id,
            stream_id,
            ts,
            entry.metric,
            value,
            entry.entry_value,
            entry.entry_unit,
            entry.instrument,
            entry.sample_temp_c,
            entry.calibration_temp_c,
            entry.wort_correction,
        ],
    )?;

    get_measurement(tx, tx.last_insert_rowid())
}

/// Everything from one bench moment, in one call: the checklist's quick entry, minus the
/// form. All at `entry.ts`, and the note last so a rejected reading takes the note with
/// it when the caller rolls back.
pub fn record_quick_entry(
    tx: &Transaction<'_>,
    batch_id: i64,
    entry: &QuickEntry,
) -> Result<QuickEntryResult> {
    let mut measurements = Vec::with_capacity(entry.measurements.len());
    for measurement in &entry.measurements {
        measurements.push(record_measurement(tx, batch_id, entry.ts, measurement)?);
    }

    let event_id = match entry
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(note) => Some(add_event(
            tx,
            batch_id,
            &NewBatchEvent {
                ts: entry.ts,
                kind: EventKind::Note,
                payload: None,
                note: Some(note.to_owned()),
            },
        )?),
        None => None,
    };

    Ok(QuickEntryResult {
        measurements,
        event_id,
    })
}

/// Remove a measurement and the reading it produced.
///
/// Both halves or neither: a measurement deleted without its reading leaves Tier 1
/// asserting a number Tier 2 has disavowed, with nothing to detect it. The reading is
/// deleted by `(stream_id, ts)` and not through `tier1::delete_readings_for_source`,
/// which is source-wide and would take every other manual measurement of the batch.
/// The stream and its binding stay: a metric measured once is still a metric this batch
/// has, and an empty series is not a wrong one.
pub fn delete_measurement(tx: &Transaction<'_>, measurement_id: i64) -> Result<bool> {
    let row = get_measurement(tx, measurement_id)?;
    tx.execute(
        "DELETE FROM reading WHERE stream_id = ? AND ts = ?",
        params![row.stream_id, row.ts],
    )?;
    let changed = tx.execute(
        "DELETE FROM manual_measurement WHERE id = ?",
        [measurement_id],
    )?;
    Ok(changed == 1)
}

// ------------------------------------------------------------------------- reads

pub fn get_measurement(conn: &Connection, measurement_id: i64) -> Result<MeasurementRow> {
    conn.query_row(
        &format!("SELECT {MEASUREMENT_COLUMNS} FROM manual_measurement WHERE id = ?"),
        [measurement_id],
        measurement_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchMeasurement { measurement_id })
}

pub fn list_measurements(conn: &Connection, batch_id: i64) -> Result<Vec<MeasurementRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEASUREMENT_COLUMNS} FROM manual_measurement \
         WHERE batch_id = ? ORDER BY ts, id"
    ))?;
    let rows = stmt.query_map([batch_id], measurement_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn list_measurements_for_metric(
    conn: &Connection,
    batch_id: i64,
    metric: Metric,
) -> Result<Vec<MeasurementRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEASUREMENT_COLUMNS} FROM manual_measurement \
         WHERE batch_id = ? AND metric = ? ORDER BY ts, id"
    ))?;
    let rows = stmt.query_map(params![batch_id, metric], measurement_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, archive_batch, create_batch, set_batch_measurements},
        recipe::BeverageType,
        timeline::{list_batch_events, list_batch_streams, record_pitch},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);

    fn at(offset_secs: i64) -> Timestamp {
        Timestamp::from_secs(T0.as_secs() + offset_secs)
    }

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection, code: &str) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    /// Every write here wants a transaction, and every test owns its connection, so one
    /// short-lived one around the call is the whole ceremony.
    fn record(
        conn: &mut Connection,
        batch_id: i64,
        ts: Timestamp,
        entry: &NewMeasurement,
    ) -> Result<MeasurementRow> {
        let tx = conn.transaction().unwrap();
        let row = record_measurement(&tx, batch_id, ts, entry);
        if row.is_ok() {
            tx.commit().unwrap();
        }
        row
    }

    fn gravity(entry_value: f64, entry_unit: EntryUnit) -> NewMeasurement {
        simple(Metric::Sg, entry_value, entry_unit)
    }

    fn simple(metric: Metric, entry_value: f64, entry_unit: EntryUnit) -> NewMeasurement {
        NewMeasurement {
            metric,
            entry_value,
            entry_unit,
            instrument: None,
            sample_temp_c: None,
            calibration_temp_c: None,
            wort_correction: None,
        }
    }

    fn refracto(brix: f64) -> NewMeasurement {
        NewMeasurement {
            metric: Metric::Sg,
            entry_value: brix,
            entry_unit: EntryUnit::Brix,
            instrument: Some(Instrument::Refractometer),
            sample_temp_c: None,
            calibration_temp_c: None,
            wort_correction: Some(1.04),
        }
    }

    fn readings(conn: &Connection, stream_id: i64) -> Vec<(i64, f64)> {
        let mut stmt = conn
            .prepare("SELECT ts, value FROM reading WHERE stream_id = ? ORDER BY ts")
            .unwrap();
        stmt.query_map([stream_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    // ---------------------------------------------------------------- vocabulary

    #[test]
    fn the_stored_strings_are_pinned() {
        assert_eq!(
            Instrument::ALL
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            [
                "hydrometer",
                "refractometer",
                "ph_meter",
                "thermometer",
                "titration"
            ]
        );
        assert_eq!(
            EntryUnit::ALL
                .iter()
                .map(|u| u.as_str())
                .collect::<Vec<_>>(),
            ["sg", "plato", "brix", "c", "f", "ph", "g_per_l"]
        );
    }

    /// The external_id is in the database. Changing its shape would orphan every manual
    /// stream that already exists, so it is pinned rather than trusted to stay put.
    #[test]
    fn the_external_id_shape_is_pinned() {
        assert_eq!(manual_external_id(17, Metric::Sg), "manual/17/sg");
        assert_eq!(manual_external_id(17, Metric::TempC), "manual/17/temp_c");
    }

    // ----------------------------------------------------------- source and stream

    #[test]
    fn the_manual_source_is_created_and_bound_on_first_use_and_reused_after() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        assert_eq!(manual_source_for(&conn, batch_id).unwrap(), None);

        let first = record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();
        let source_id = manual_source_for(&conn, batch_id).unwrap().unwrap();

        let second = record(
            &mut conn,
            batch_id,
            at(60),
            &simple(Metric::Ph, 3.4, EntryUnit::Ph),
        )
        .unwrap();
        assert_eq!(manual_source_for(&conn, batch_id).unwrap(), Some(source_id));

        // One source, but one stream per metric — they are different series.
        assert_ne!(first.stream_id, second.stream_id);
        assert_eq!(list_batch_streams(&conn, batch_id).unwrap().len(), 2);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM source WHERE kind = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn each_batch_gets_its_own_manual_source() {
        let mut conn = db();
        let seventeen = batch(&conn, "SELTZER-017");
        let eighteen = batch(&conn, "SELTZER-018");

        record(&mut conn, seventeen, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();
        record(&mut conn, eighteen, at(0), &gravity(1.050, EntryUnit::Sg)).unwrap();

        assert_ne!(
            manual_source_for(&conn, seventeen).unwrap(),
            manual_source_for(&conn, eighteen).unwrap()
        );
    }

    /// `source.config` is a record, not an index — the lookup is the `batch_stream`
    /// join, because Tier 1 must not reference Tier 2. It is asserted anyway so that a
    /// database opened by hand says which batch a manual source belongs to.
    #[test]
    fn the_source_records_its_batch_in_config_without_a_column_for_it() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();

        let (name, config): (String, String) = conn
            .query_row(
                "SELECT name, config FROM source WHERE kind = 'manual'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Manual (SELTZER-017)");
        assert_eq!(config, format!("{{\"batch_id\":{batch_id}}}"));
    }

    /// A manual thermometer and the fermenter probe share `Metric::TempC` on purpose;
    /// what separates them is the source, and therefore the stream.
    #[test]
    fn a_manual_temperature_shares_the_metric_but_not_the_stream() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        let row = record(
            &mut conn,
            batch_id,
            at(0),
            &simple(Metric::TempC, 68.0, EntryUnit::Fahrenheit),
        )
        .unwrap();

        assert_eq!(row.metric, Metric::TempC);
        assert!((row.value - 20.0).abs() < 1e-9, "{}", row.value);

        let source_id: i64 = conn
            .query_row(
                "SELECT source_id FROM stream WHERE id = ?",
                [row.stream_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(manual_source_for(&conn, batch_id).unwrap(), Some(source_id));
    }

    /// The first measurement typed is routinely not the earliest one taken.
    #[test]
    fn a_backdated_measurement_widens_the_binding_rather_than_falling_outside_it() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        record_pitch(&conn, batch_id, at(1_000), None).unwrap();

        record(
            &mut conn,
            batch_id,
            at(2_000),
            &gravity(1.010, EntryUnit::Sg),
        )
        .unwrap();
        assert_eq!(
            list_batch_streams(&conn, batch_id).unwrap()[0].starts_at,
            at(1_000),
            "a first binding starts at the pitch, not at the reading"
        );

        record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();
        assert_eq!(
            list_batch_streams(&conn, batch_id).unwrap()[0].starts_at,
            at(0)
        );
    }

    // ------------------------------------------------------------------ scale entry

    #[test]
    fn plato_and_brix_are_stored_as_sg_with_the_entry_kept() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let plato = record(&mut conn, batch_id, at(0), &gravity(11.9, EntryUnit::Plato)).unwrap();
        assert!((plato.value - 1.048).abs() < 0.001, "{}", plato.value);
        assert_eq!(plato.entry_value, 11.9);
        assert_eq!(plato.entry_unit, EntryUnit::Plato);

        let brix = record(&mut conn, batch_id, at(60), &gravity(12.0, EntryUnit::Brix)).unwrap();
        assert!((brix.value - 1.048).abs() < 0.002, "{}", brix.value);
        assert_eq!(brix.entry_unit, EntryUnit::Brix);
    }

    #[test]
    fn a_hydrometer_reading_is_temperature_corrected_and_both_temps_are_recorded() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = gravity(1.050, EntryUnit::Sg);
        entry.instrument = Some(Instrument::Hydrometer);
        entry.sample_temp_c = Some(Celsius::new(30.0).unwrap());
        entry.calibration_temp_c = Some(Celsius::new(20.0).unwrap());

        let row = record(&mut conn, batch_id, at(0), &entry).unwrap();
        assert!(row.value > 1.050, "a warm sample reads low: {}", row.value);
        assert!((row.value - 1.0526).abs() < 0.0005, "{}", row.value);
        assert_eq!(row.sample_temp_c, Some(Celsius::new(30.0).unwrap()));
        assert_eq!(row.calibration_temp_c, Some(Celsius::new(20.0).unwrap()));
        assert_eq!(row.entry_value, 1.050);
    }

    /// One temperature alone does not say what to correct against, and assuming the
    /// other is how a reading picks up a correction nobody asked for.
    #[test]
    fn a_hydrometer_reading_with_only_one_temperature_is_left_uncorrected() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = gravity(1.050, EntryUnit::Sg);
        entry.instrument = Some(Instrument::Hydrometer);
        entry.sample_temp_c = Some(Celsius::new(30.0).unwrap());

        let row = record(&mut conn, batch_id, at(0), &entry).unwrap();
        assert_eq!(row.value, 1.050);
        assert_eq!(row.calibration_temp_c, None);
    }

    #[test]
    fn a_wort_correction_factor_on_a_hydrometer_is_refused_not_ignored() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = gravity(1.050, EntryUnit::Sg);
        entry.instrument = Some(Instrument::Hydrometer);
        entry.wort_correction = Some(1.04);

        let err = record(&mut conn, batch_id, at(0), &entry).unwrap_err();
        assert_eq!(err.code(), "db.measurement_instrument_mismatch");
    }

    // ---------------------------------------------------------------- refractometer

    #[test]
    fn a_pre_pitch_refractometer_reading_is_not_alcohol_corrected() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let row = record(&mut conn, batch_id, at(0), &refracto(12.5)).unwrap();
        assert!((row.value - 1.048).abs() < 0.001, "{}", row.value);
        assert_eq!(row.wort_correction, Some(1.04));
    }

    #[test]
    fn a_post_pitch_refractometer_reading_is_corrected_against_the_batch_og() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        record_pitch(&conn, batch_id, at(0), None).unwrap();
        set_batch_measurements(&conn, batch_id, None, Some(Sg::new(1.048).unwrap()), None).unwrap();

        let row = record(&mut conn, batch_id, at(10_000), &refracto(4.16)).unwrap();
        assert!((row.value - 1.0039).abs() < 0.002, "{}", row.value);

        // The naive conversion of 4.16 °Bx is around 1.016 — removing the alcohol term
        // is the whole point of the correction.
        assert!(row.value < 1.010, "{}", row.value);
    }

    #[test]
    fn a_post_pitch_refractometer_reading_without_an_og_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        record_pitch(&conn, batch_id, at(0), None).unwrap();

        let err = record(&mut conn, batch_id, at(10_000), &refracto(4.16)).unwrap_err();
        assert_eq!(err.code(), "db.no_original_gravity");
    }

    /// Terrill's cubic is fitted against reference-temperature hydrometer values and the
    /// instrument is ATC; applying both corrections would double-count.
    #[test]
    fn a_refractometer_reading_is_never_also_temperature_corrected() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = refracto(12.5);
        entry.sample_temp_c = Some(Celsius::new(30.0).unwrap());

        let err = record(&mut conn, batch_id, at(0), &entry).unwrap_err();
        assert_eq!(err.code(), "db.measurement_instrument_mismatch");
    }

    #[test]
    fn a_refractometer_reading_typed_in_sg_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = refracto(12.5);
        entry.entry_unit = EntryUnit::Sg;
        entry.entry_value = 1.048;

        let err = record(&mut conn, batch_id, at(0), &entry).unwrap_err();
        assert_eq!(err.code(), "db.measurement_instrument_mismatch");
    }

    /// The right factor is 1.0 for a sugar wash and instrument-specific otherwise, so
    /// there is nothing to default to.
    #[test]
    fn a_refractometer_reading_without_a_wort_correction_factor_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = refracto(12.5);
        entry.wort_correction = None;

        let err = record(&mut conn, batch_id, at(0), &entry).unwrap_err();
        assert_eq!(err.code(), "db.measurement_instrument_mismatch");
    }

    // --------------------------------------------------------------------- refusals

    #[test]
    fn a_unit_that_does_not_express_the_metric_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let err = record(
            &mut conn,
            batch_id,
            at(0),
            &simple(Metric::Ph, 12.0, EntryUnit::Brix),
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.measurement_unit_mismatch");
    }

    #[test]
    fn an_instrument_that_does_not_read_the_metric_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let mut entry = simple(Metric::Ph, 3.4, EntryUnit::Ph);
        entry.instrument = Some(Instrument::Hydrometer);

        let err = record(&mut conn, batch_id, at(0), &entry).unwrap_err();
        assert_eq!(err.code(), "db.measurement_instrument_mismatch");
    }

    /// A controller series has no entry unit, so it cannot be typed in at all.
    #[test]
    fn a_controller_metric_cannot_be_entered_by_hand() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let err = record(
            &mut conn,
            batch_id,
            at(0),
            &simple(Metric::State, 2.0, EntryUnit::Sg),
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.measurement_unit_mismatch");
    }

    /// Enforced, not advised: a sensor sending 47 has a next sample, a human typing it
    /// does not.
    #[test]
    fn an_implausible_reading_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        assert_eq!(
            record(
                &mut conn,
                batch_id,
                at(0),
                &simple(Metric::Ph, 47.0, EntryUnit::Ph)
            )
            .unwrap_err()
            .code(),
            "db.measurement_implausible"
        );
        assert_eq!(
            record(
                &mut conn,
                batch_id,
                at(0),
                &simple(Metric::TempC, 900.0, EntryUnit::Celsius)
            )
            .unwrap_err()
            .code(),
            "db.measurement_implausible"
        );
    }

    #[test]
    fn a_non_finite_entry_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let err = record(
            &mut conn,
            batch_id,
            at(0),
            &gravity(f64::NAN, EntryUnit::Sg),
        )
        .unwrap_err();
        assert_eq!(err.code(), "units.not_finite");
    }

    /// `insert_readings` would swallow the second one as a duplicate; saying so is the
    /// only way the user learns that the first reading is still the stored one.
    #[test]
    fn a_second_measurement_of_the_same_metric_at_the_same_second_is_refused() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();
        let err = record(&mut conn, batch_id, at(0), &gravity(1.049, EntryUnit::Sg)).unwrap_err();
        assert_eq!(err.code(), "db.measurement_exists");

        assert_eq!(list_measurements(&conn, batch_id).unwrap().len(), 1);
    }

    #[test]
    fn an_archived_batch_takes_no_further_measurements() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");
        archive_batch(&conn, batch_id, at(1_000)).unwrap();

        let err = record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap_err();
        assert_eq!(err.code(), "db.batch_archived");
    }

    /// A rejected entry must not leave a source or a stream behind, even inside a
    /// transaction the caller goes on to commit.
    #[test]
    fn a_rejected_entry_creates_no_source_or_stream() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let tx = conn.transaction().unwrap();
        record_measurement(
            &tx,
            batch_id,
            at(0),
            &simple(Metric::Ph, 47.0, EntryUnit::Ph),
        )
        .unwrap_err();
        tx.commit().unwrap();

        assert_eq!(manual_source_for(&conn, batch_id).unwrap(), None);
        let streams: i64 = conn
            .query_row("SELECT COUNT(*) FROM stream", [], |r| r.get(0))
            .unwrap();
        assert_eq!(streams, 0);
    }

    // ---------------------------------------------------------------------- fan-out

    #[test]
    fn the_derived_value_lands_in_tier_one_at_the_same_second() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let row = record(&mut conn, batch_id, at(0), &gravity(11.9, EntryUnit::Plato)).unwrap();
        assert_eq!(
            readings(&conn, row.stream_id),
            vec![(at(0).as_secs(), row.value)]
        );
    }

    // ------------------------------------------------------------------ quick entry

    #[test]
    fn a_quick_entry_writes_every_reading_and_its_note_in_one_call() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let tx = conn.transaction().unwrap();
        let result = record_quick_entry(
            &tx,
            batch_id,
            &QuickEntry {
                ts: at(500),
                measurements: vec![
                    gravity(1.010, EntryUnit::Sg),
                    simple(Metric::Ph, 3.4, EntryUnit::Ph),
                    simple(Metric::TempC, 19.5, EntryUnit::Celsius),
                    simple(Metric::Ta, 6.2, EntryUnit::GramsPerLitre),
                ],
                note: Some("  still a bit sulphury  ".into()),
            },
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!(result.measurements.len(), 4);
        assert!(result.measurements.iter().all(|m| m.ts == at(500)));
        assert!(result.event_id.is_some());

        // The note is an event, not a measurement.
        assert_eq!(list_measurements(&conn, batch_id).unwrap().len(), 4);
        let events = list_batch_events(&conn, batch_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Note);
        assert_eq!(events[0].note.as_deref(), Some("still a bit sulphury"));
    }

    #[test]
    fn a_quick_entry_with_no_note_logs_no_event() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let tx = conn.transaction().unwrap();
        let result = record_quick_entry(
            &tx,
            batch_id,
            &QuickEntry {
                ts: at(0),
                measurements: vec![gravity(1.048, EntryUnit::Sg)],
                note: Some("   ".into()),
            },
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!(result.event_id, None);
        assert!(list_batch_events(&conn, batch_id).unwrap().is_empty());
    }

    /// The caller owns the transaction boundary, so a half-failed quick entry is nothing
    /// at all — which is what makes "type four numbers and get one wrong" safe.
    #[test]
    fn a_quick_entry_that_fails_part_way_writes_nothing() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let tx = conn.transaction().unwrap();
        let err = record_quick_entry(
            &tx,
            batch_id,
            &QuickEntry {
                ts: at(0),
                measurements: vec![
                    gravity(1.048, EntryUnit::Sg),
                    simple(Metric::Ph, 47.0, EntryUnit::Ph),
                ],
                note: Some("should not survive".into()),
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.measurement_implausible");
        tx.rollback().unwrap();

        assert!(list_measurements(&conn, batch_id).unwrap().is_empty());
        assert!(list_batch_events(&conn, batch_id).unwrap().is_empty());
        assert_eq!(manual_source_for(&conn, batch_id).unwrap(), None);
    }

    // ----------------------------------------------------------------- read/delete

    #[test]
    fn measurements_come_back_oldest_first_and_filter_by_metric() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        record(&mut conn, batch_id, at(200), &gravity(1.020, EntryUnit::Sg)).unwrap();
        record(&mut conn, batch_id, at(100), &gravity(1.048, EntryUnit::Sg)).unwrap();
        record(
            &mut conn,
            batch_id,
            at(150),
            &simple(Metric::Ph, 3.4, EntryUnit::Ph),
        )
        .unwrap();

        let all = list_measurements(&conn, batch_id).unwrap();
        assert_eq!(
            all.iter().map(|m| m.ts).collect::<Vec<_>>(),
            [at(100), at(150), at(200)]
        );

        let gravities = list_measurements_for_metric(&conn, batch_id, Metric::Sg).unwrap();
        assert_eq!(gravities.len(), 2);
        assert!(gravities.iter().all(|m| m.metric == Metric::Sg));
    }

    #[test]
    fn a_measurement_that_does_not_exist_says_so() {
        let conn = db();
        assert_eq!(
            get_measurement(&conn, 404).unwrap_err().code(),
            "db.no_such_measurement"
        );
    }

    #[test]
    fn deleting_a_measurement_takes_its_reading_and_leaves_the_others_alone() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let first = record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();
        let second = record(&mut conn, batch_id, at(600), &gravity(1.020, EntryUnit::Sg)).unwrap();
        assert_eq!(first.stream_id, second.stream_id);

        let tx = conn.transaction().unwrap();
        assert!(delete_measurement(&tx, first.id).unwrap());
        tx.commit().unwrap();

        assert_eq!(
            readings(&conn, second.stream_id),
            vec![(at(600).as_secs(), second.value)]
        );
        assert_eq!(list_measurements(&conn, batch_id).unwrap().len(), 1);

        // The stream and its binding outlive the reading: a metric measured once is
        // still a metric this batch has.
        assert_eq!(list_batch_streams(&conn, batch_id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_measurement_that_does_not_exist_says_so() {
        let mut conn = db();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            delete_measurement(&tx, 404).unwrap_err().code(),
            "db.no_such_measurement"
        );
    }

    // ----------------------------------------------------------------- end to end

    /// The decisive one. A gravity that wrote `batch.og` back would corrupt every later
    /// post-pitch refractometer correction, silently and cumulatively.
    #[test]
    fn a_manual_gravity_does_not_touch_batch_og_or_fg() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        record(&mut conn, batch_id, at(0), &gravity(1.048, EntryUnit::Sg)).unwrap();
        record(&mut conn, batch_id, at(600), &gravity(1.002, EntryUnit::Sg)).unwrap();

        let batch_row = get_batch(&conn, batch_id).unwrap();
        assert_eq!(batch_row.og, None);
        assert_eq!(batch_row.fg, None);
    }

    /// A whole bench session without a UI: an OG off the refractometer, the pitch, then
    /// a post-pitch reading with a pH and a taste note through the quick entry.
    #[test]
    fn a_bench_session_lands_on_the_timeline_the_chart_already_reads() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-017");

        let og = record(&mut conn, batch_id, at(0), &refracto(12.5)).unwrap();
        record_pitch(&conn, batch_id, at(100), None).unwrap();
        set_batch_measurements(
            &conn,
            batch_id,
            None,
            Some(Sg::new(og.value).unwrap()),
            None,
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        let result = record_quick_entry(
            &tx,
            batch_id,
            &QuickEntry {
                ts: at(86_400),
                measurements: vec![refracto(4.16), simple(Metric::Ph, 3.3, EntryUnit::Ph)],
                note: Some("dry, faint sulphur".into()),
            },
        )
        .unwrap();
        tx.commit().unwrap();

        // Tier 2 and Tier 1 agree, reading for reading.
        let measurements = list_measurements(&conn, batch_id).unwrap();
        assert_eq!(measurements.len(), 3);
        for measurement in &measurements {
            assert!(
                readings(&conn, measurement.stream_id)
                    .contains(&(measurement.ts.as_secs(), measurement.value)),
                "{measurement:?} has no reading"
            );
        }

        // The note is on the timeline, both series are bound, and the batch's own
        // columns are untouched but for the OG that was set deliberately.
        assert!(result.event_id.is_some());
        assert_eq!(list_batch_events(&conn, batch_id).unwrap().len(), 2); // pitch + note
        assert_eq!(list_batch_streams(&conn, batch_id).unwrap().len(), 2);

        let batch_row = get_batch(&conn, batch_id).unwrap();
        assert_eq!(batch_row.og.map(Sg::get), Some(og.value));
        assert_eq!(batch_row.fg, None);
    }
}
