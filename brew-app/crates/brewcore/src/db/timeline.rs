//! Phase 3d. The timeline, and the join that binds a sensor to a batch.
//!
//! **Events are not readings.** A reading is a number on a time series; an event is
//! something you *did*. `plan_vs_actual` already covers an addition of a known lot;
//! nothing before this module could express a cold crash, a transfer or a temp ramp,
//! and those are what a batch is diagnosed from.
//!
//! **`batch_stream` is the join that makes everything else work.** Without it the same
//! fermenter's readings run together across batch 17 and batch 18, and Phase 4's chart
//! has nothing to scope to.
//!
//! Two rules carry this module. Pitch is written in one place — [`record_pitch`] sets
//! the event, `batch.pitched_at` and the status together, because they are one fact and
//! 3c's gate out of `planned` reads the column. And a stream belongs to at most one
//! batch at a time: SQLite has no exclusion constraint, so [`bind_stream`] checks the
//! overlap on write, on half-open windows so a fermenter turnaround is not a collision.
//!
//! Same contract as [`batch`](crate::db::batch) and [`recipe`](crate::db::recipe):
//! every function takes a borrowed connection and never opens or commits its own
//! transaction. `&Transaction` derefs to `&Connection`, so "record the pitch and log
//! the first nutrient addition" is one boundary at the call site.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    batch::{BatchStatus, ensure_open, get_batch, set_pitched_at, transition_batch},
    enums::db_enum,
    tier1::AnnotationChannel,
};
use crate::{Error, Result, units::Timestamp};

db_enum!(
    /// What was done. A closed set, so Phase 5's analytics can rely on it. Not a
    /// `CHECK` in the schema; see `0005_timeline.sql`.
    ///
    /// `Note` is beyond the checklist's list on purpose: an imported annotation that
    /// maps to no process step still has to land somewhere, and free text with no kind
    /// at all is how a closed set quietly stops being closed.
    EventKind,
    UnknownEventKind,
    (Pitch, "pitch"),
    (NutrientAddition, "nutrient_addition"),
    (FlavoringAddition, "flavoring_addition"),
    (TempRamp, "temp_ramp"),
    (ColdCrash, "cold_crash"),
    (Transfer, "transfer"),
    (Fining, "fining"),
    (Carbonation, "carbonation"),
    (Package, "package"),
    (Note, "note"),
);

// ----------------------------------------------------------- batch_event

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewBatchEvent {
    pub ts: Timestamp,
    pub kind: EventKind,
    /// Opaque JSON. Kind-specific fields stay out of the schema.
    pub payload: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchEventRow {
    pub id: i64,
    pub batch_id: i64,
    pub ts: Timestamp,
    pub kind: EventKind,
    pub payload: Option<String>,
    pub note: Option<String>,
}

const EVENT_COLUMNS: &str = "id, batch_id, ts, kind, payload, note";

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchEventRow> {
    Ok(BatchEventRow {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        ts: row.get(2)?,
        kind: row.get(3)?,
        payload: row.get(4)?,
        note: row.get(5)?,
    })
}

/// Log something that was done to a batch.
///
/// [`EventKind::Pitch`] is refused here: it is three writes, not one, and an event that
/// recorded only the first would leave a batch carrying a pitch it could not ferment
/// from. [`record_pitch`] is the door.
pub fn add_event(conn: &Connection, batch_id: i64, event: &NewBatchEvent) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;
    if event.kind == EventKind::Pitch {
        return Err(Error::UsePitchRecorder { batch_id });
    }
    insert_event(conn, batch_id, event)
}

fn insert_event(conn: &Connection, batch_id: i64, event: &NewBatchEvent) -> Result<i64> {
    conn.execute(
        "INSERT INTO batch_event (batch_id, ts, kind, payload, note) VALUES (?, ?, ?, ?, ?)",
        params![batch_id, event.ts, event.kind, event.payload, event.note],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The timeline, oldest first. `id` breaks a tie so two events at one second keep the
/// order they were entered in — the same rule `list_batch_ingredients` uses.
pub fn list_batch_events(conn: &Connection, batch_id: i64) -> Result<Vec<BatchEventRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM batch_event WHERE batch_id = ? ORDER BY ts, id"
    ))?;
    let rows = stmt.query_map([batch_id], event_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_event(conn: &Connection, event_id: i64) -> Result<BatchEventRow> {
    conn.query_row(
        &format!("SELECT {EVENT_COLUMNS} FROM batch_event WHERE id = ?"),
        [event_id],
        event_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchBatchEvent { event_id })
}

/// Returns whether a row was removed. A mis-typed event is a mistake, not a record of
/// anything, so unlike a batch this is a real delete.
///
/// The pitch is the exception: it owns two other pieces of state, so [`undo_pitch`] is
/// its reverse.
pub fn delete_event(conn: &Connection, event_id: i64) -> Result<bool> {
    let event = get_event(conn, event_id)?;
    if event.kind == EventKind::Pitch {
        return Err(Error::UsePitchRecorder {
            batch_id: event.batch_id,
        });
    }
    let changed = conn.execute("DELETE FROM batch_event WHERE id = ?", [event_id])?;
    Ok(changed == 1)
}

/// Record t=0: the event, `batch.pitched_at` and the move out of `planned`, in that
/// order and in the caller's transaction.
///
/// 3c left `pitched_at` as the gate out of `planned` and said this call would write it
/// rather than replace the rule. Doing all three here is what keeps them from
/// disagreeing — the partial unique index on `batch_event(batch_id) WHERE kind='pitch'`
/// makes a second pitch a constraint error rather than a silent second t=0.
pub fn record_pitch(
    conn: &Connection,
    batch_id: i64,
    at: Timestamp,
    note: Option<&str>,
) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;
    let id = insert_event(
        conn,
        batch_id,
        &NewBatchEvent {
            ts: at,
            kind: EventKind::Pitch,
            payload: None,
            note: note.map(str::to_owned),
        },
    )?;
    set_pitched_at(conn, batch_id, at)?;
    transition_batch(conn, batch_id, BatchStatus::Fermenting)?;
    Ok(id)
}

/// Undo a pitch entered by mistake: delete the event, clear `pitched_at`, and put the
/// batch back in `planned`.
///
/// Only while the batch is still `fermenting` — once it has conditioned, the pitch is
/// load-bearing for everything recorded since, and "this never happened" stops being
/// true. This is the one backwards status move in the app, and it exists precisely
/// because [`transition_batch`] refuses to be the general-purpose one.
pub fn undo_pitch(conn: &Connection, batch_id: i64) -> Result<bool> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;
    if batch.status != BatchStatus::Fermenting && batch.status != BatchStatus::Planned {
        return Err(Error::PitchEventLocked { batch_id });
    }

    let removed = conn.execute(
        "DELETE FROM batch_event WHERE batch_id = ? AND kind = ?",
        params![batch_id, EventKind::Pitch],
    )?;
    conn.execute(
        "UPDATE batch SET pitched_at = NULL, status = ? WHERE id = ?",
        params![BatchStatus::Planned, batch_id],
    )?;
    Ok(removed == 1)
}

// ---------------------------------------------------------- batch_stream

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchStreamRow {
    pub id: i64,
    pub batch_id: i64,
    pub stream_id: i64,
    pub starts_at: Timestamp,
    pub ends_at: Option<Timestamp>,
}

const BINDING_COLUMNS: &str = "id, batch_id, stream_id, starts_at, ends_at";

fn binding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchStreamRow> {
    Ok(BatchStreamRow {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        stream_id: row.get(2)?,
        starts_at: row.get(3)?,
        ends_at: row.get(4)?,
    })
}

/// Bind a sensor to a batch for a window. `None` for `ends_at` means "still bound".
///
/// Overlapping this stream with a *different* batch is refused. SQLite has no exclusion
/// constraint, so the check is a query before the insert, inside the caller's
/// transaction. The window is half-open — `[starts_at, ends_at)` — so batch 18 starting
/// at exactly the second batch 17 ended is the fermenter turnaround this table exists
/// for, not a collision.
///
/// Two windows of the *same* batch may overlap: a sensor unplugged and reconnected is
/// one batch's curve in two pieces, and there is nothing ambiguous about it.
pub fn bind_stream(
    conn: &Connection,
    batch_id: i64,
    stream_id: i64,
    starts_at: Timestamp,
    ends_at: Option<Timestamp>,
) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;

    let clash: Option<i64> = conn
        .query_row(
            "SELECT batch_id FROM batch_stream \
             WHERE stream_id = ?1 AND batch_id <> ?2 \
               AND (?4 IS NULL OR starts_at < ?4) \
               AND (ends_at IS NULL OR ends_at > ?3) \
             LIMIT 1",
            params![stream_id, batch_id, starts_at, ends_at],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(other) = clash {
        return Err(Error::StreamAlreadyBound {
            stream_id,
            batch_id: other,
        });
    }

    conn.execute(
        "INSERT INTO batch_stream (batch_id, stream_id, starts_at, ends_at) VALUES (?, ?, ?, ?)",
        params![batch_id, stream_id, starts_at, ends_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Close an open binding at `at`. `false` if it was already closed.
///
/// The binding is not deleted: the readings it scoped are still that batch's, and a
/// window that vanishes takes a chunk of the curve with it.
pub fn unbind_stream(conn: &Connection, binding_id: i64, at: Timestamp) -> Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM batch_stream WHERE id = ?",
            [binding_id],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(Error::NoSuchBatchStream { binding_id });
    }

    let changed = conn.execute(
        "UPDATE batch_stream SET ends_at = ? WHERE id = ? AND ends_at IS NULL AND ? > starts_at",
        params![at, binding_id, at],
    )?;
    Ok(changed == 1)
}

pub fn list_batch_streams(conn: &Connection, batch_id: i64) -> Result<Vec<BatchStreamRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BINDING_COLUMNS} FROM batch_stream WHERE batch_id = ? ORDER BY starts_at, id"
    ))?;
    let rows = stmt.query_map([batch_id], binding_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Which batch owned this stream at `ts`, if any. The lookup Phase 4 scopes a chart
/// with, and the reason the overlap rule has to hold: with two answers there is no way
/// to pick one afterwards.
pub fn batch_for_stream_at(
    conn: &Connection,
    stream_id: i64,
    ts: Timestamp,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT batch_id FROM batch_stream \
             WHERE stream_id = ?1 AND starts_at <= ?2 AND (ends_at IS NULL OR ends_at > ?2) \
             LIMIT 1",
            params![stream_id, ts],
            |row| row.get(0),
        )
        .optional()?)
}

// ------------------------------------------------------ annotation import

/// A Fermentrack annotation that falls inside one of this batch's stream windows.
///
/// Not an event until someone says so: `beer_ann` is free text a human typed into
/// another app, and guessing a `kind` from it well enough to write it into the record
/// unattended is not a thing this can do. [`guess_kind`] offers; the caller decides.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SuggestedEvent {
    pub ts: Timestamp,
    pub channel: AnnotationChannel,
    pub text: String,
    /// A keyword guess, or `None` when nothing matched.
    pub kind: Option<EventKind>,
}

/// Annotations inside this batch's bindings that have not already been accepted.
///
/// The join is `annotation → source → stream → batch_stream`: an annotation belongs to
/// a log file, a binding to one of that file's streams. `DISTINCT` because a file
/// usually has several streams bound to the same batch, and the annotation is one fact
/// about the file, not one per series.
pub fn suggest_events_for_batch(conn: &Connection, batch_id: i64) -> Result<Vec<SuggestedEvent>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT a.ts, a.channel, a.text \
           FROM annotation a \
           JOIN stream s ON s.source_id = a.source_id \
           JOIN batch_stream bs ON bs.stream_id = s.id \
          WHERE bs.batch_id = ?1 \
            AND a.ts >= bs.starts_at \
            AND (bs.ends_at IS NULL OR a.ts < bs.ends_at) \
            AND NOT EXISTS ( \
                  SELECT 1 FROM batch_event e \
                   WHERE e.batch_id = ?1 AND e.ts = a.ts AND e.note = a.text) \
          ORDER BY a.ts, a.channel",
    )?;
    let rows = stmt.query_map([batch_id], |row| {
        let text: String = row.get(2)?;
        Ok(SuggestedEvent {
            ts: row.get(0)?,
            channel: row.get(1)?,
            kind: guess_kind(&text),
            text,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A keyword guess at what an annotation describes, or `None`.
///
/// Deliberately shallow. It exists to pre-select a dropdown, and a cleverer version
/// would be a classifier whose mistakes end up in the one tier that is never
/// reconstructible.
pub fn guess_kind(text: &str) -> Option<EventKind> {
    let text = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    match () {
        _ if has(&["pitch"]) => Some(EventKind::Pitch),
        _ if has(&["nutrient", "dap", "fermaid", "yan"]) => Some(EventKind::NutrientAddition),
        _ if has(&["flavor", "flavour", "dry hop", "zest", "puree"]) => {
            Some(EventKind::FlavoringAddition)
        }
        _ if has(&["crash", "cold-crash"]) => Some(EventKind::ColdCrash),
        // "temp set to" and "temp profile" are Fermentrack's own wording for a
        // setpoint change; they arrive verbatim from the log's annotation columns.
        _ if has(&[
            "ramp",
            "raised",
            "lowered",
            "setpoint",
            "temp set to",
            "temp profile",
        ]) =>
        {
            Some(EventKind::TempRamp)
        }
        _ if has(&["transfer", "racked", "racking"]) => Some(EventKind::Transfer),
        _ if has(&["fining", "gelatin", "isinglass", "clarif"]) => Some(EventKind::Fining),
        _ if has(&["carbonat", "force carb", "co2"]) => Some(EventKind::Carbonation),
        _ if has(&["package", "packaged", "bottle", "keg", "canned"]) => Some(EventKind::Package),
        _ => None,
    }
}

/// Turn a suggestion into a real event under the kind the caller chose.
///
/// The payload records where it came from, so a timeline can tell "Fermentrack said
/// this" from "I typed this" forever after — and so a re-import of the same annotation
/// is recognisable as one.
///
/// `EventKind::Pitch` goes through [`record_pitch`], for the reason [`add_event`] gives.
pub fn accept_suggestion(
    conn: &Connection,
    batch_id: i64,
    suggestion: &SuggestedEvent,
    kind: EventKind,
) -> Result<i64> {
    if kind == EventKind::Pitch {
        return record_pitch(conn, batch_id, suggestion.ts, Some(&suggestion.text));
    }
    add_event(
        conn,
        batch_id,
        &NewBatchEvent {
            ts: suggestion.ts,
            kind,
            payload: Some(format!(
                r#"{{"origin":"fermentrack_annotation","channel":"{}"}}"#,
                suggestion.channel
            )),
            note: Some(suggestion.text.clone()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        Metric,
        batch::{BatchStatus, NewBatch, archive_batch, create_batch, get_batch, transition_batch},
        recipe::BeverageType,
        tier1::{SourceKind, create_source, create_stream, insert_annotations},
    };
    use rusqlite::Connection;

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

    /// A source with one stream on it. `create_stream` wants a transaction; every test
    /// here owns its connection, so one short-lived one is the whole ceremony.
    fn stream(conn: &mut Connection, name: &str) -> (i64, i64) {
        let source_id = create_source(conn, SourceKind::FermentrackBeerLog, name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, "beer_temp", Metric::TempC).unwrap();
        tx.commit().unwrap();
        (source_id, stream_id)
    }

    fn note(ts: Timestamp, text: &str) -> NewBatchEvent {
        NewBatchEvent {
            ts,
            kind: EventKind::Note,
            payload: None,
            note: Some(text.into()),
        }
    }

    // ------------------------------------------------------------- events

    #[test]
    fn the_stored_strings_are_pinned() {
        // These go in the database. Renaming a variant must not silently rewrite what
        // every existing row means.
        let all: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            all,
            [
                "pitch",
                "nutrient_addition",
                "flavoring_addition",
                "temp_ramp",
                "cold_crash",
                "transfer",
                "fining",
                "carbonation",
                "package",
                "note",
            ]
        );
    }

    #[test]
    fn events_come_back_oldest_first() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");

        add_event(&conn, batch_id, &note(at(3_600), "second")).unwrap();
        add_event(&conn, batch_id, &note(at(60), "first")).unwrap();

        let notes: Vec<_> = list_batch_events(&conn, batch_id)
            .unwrap()
            .into_iter()
            .map(|e| e.note.unwrap())
            .collect();
        assert_eq!(notes, ["first", "second"]);
    }

    #[test]
    fn an_archived_batch_takes_no_further_events() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        archive_batch(&conn, batch_id, T0).unwrap();

        let err = add_event(&conn, batch_id, &note(at(60), "too late")).unwrap_err();
        assert_eq!(err.code(), "db.batch_archived");
    }

    #[test]
    fn a_pitch_cannot_be_logged_as_a_plain_event() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");

        let err = add_event(
            &conn,
            batch_id,
            &NewBatchEvent {
                ts: at(0),
                kind: EventKind::Pitch,
                payload: None,
                note: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "db.use_pitch_recorder");
    }

    #[test]
    fn recording_a_pitch_writes_the_event_the_column_and_the_status() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");

        record_pitch(&conn, batch_id, at(0), Some("US-05, 11.5 g")).unwrap();

        let row = get_batch(&conn, batch_id).unwrap();
        assert_eq!(row.pitched_at, Some(at(0)));
        assert_eq!(row.status, BatchStatus::Fermenting);

        let events = list_batch_events(&conn, batch_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Pitch);
    }

    /// The whole reason for the partial unique index. A second t=0 is not a second
    /// event, it is a batch with two answers to the question every comparison starts
    /// from.
    #[test]
    fn a_batch_cannot_be_pitched_twice() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        record_pitch(&conn, batch_id, at(0), None).unwrap();

        let err = record_pitch(&conn, batch_id, at(600), None).unwrap_err();
        assert_eq!(err.code(), "db.sqlite");
        assert!(err.to_string().contains("UNIQUE"), "{err}");
    }

    #[test]
    fn deleting_a_pitch_goes_through_undo_not_delete_event() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let event_id = record_pitch(&conn, batch_id, at(0), None).unwrap();

        let err = delete_event(&conn, event_id).unwrap_err();
        assert_eq!(err.code(), "db.use_pitch_recorder");
    }

    #[test]
    fn undoing_a_pitch_clears_the_column_and_the_status_too() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        record_pitch(&conn, batch_id, at(0), None).unwrap();

        assert!(undo_pitch(&conn, batch_id).unwrap());

        let row = get_batch(&conn, batch_id).unwrap();
        assert_eq!(row.pitched_at, None);
        assert_eq!(row.status, BatchStatus::Planned);
        assert!(list_batch_events(&conn, batch_id).unwrap().is_empty());
    }

    /// Once the batch has conditioned, the pitch is load-bearing for everything
    /// recorded since, and "this never happened" stops being true.
    #[test]
    fn a_pitch_cannot_be_undone_once_the_batch_has_moved_on() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        record_pitch(&conn, batch_id, at(0), None).unwrap();
        transition_batch(&conn, batch_id, BatchStatus::Conditioning).unwrap();

        let err = undo_pitch(&conn, batch_id).unwrap_err();
        assert_eq!(err.code(), "db.pitch_event_locked");
    }

    #[test]
    fn an_unknown_kind_in_the_database_is_an_error_not_a_panic() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let id = add_event(&conn, batch_id, &note(at(0), "x")).unwrap();
        conn.execute("UPDATE batch_event SET kind = 'brewed' WHERE id = ?", [id])
            .unwrap();

        let err = get_event(&conn, id).unwrap_err();
        assert_eq!(err.code(), "db.sqlite");
        assert!(err.to_string().contains("brewed"), "{err}");
    }

    #[test]
    fn deleting_an_event_reports_whether_it_was_there() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let id = add_event(&conn, batch_id, &note(at(0), "oops")).unwrap();

        assert!(delete_event(&conn, id).unwrap());
        assert_eq!(
            delete_event(&conn, id).unwrap_err().code(),
            "db.no_such_batch_event"
        );
    }

    // ------------------------------------------------------ stream binding

    #[test]
    fn a_stream_cannot_be_bound_to_two_batches_at_once() {
        let mut conn = db();
        let (_, stream_id) = stream(&mut conn, "FV1");
        let seventeen = batch(&conn, "SELTZER-017");
        let eighteen = batch(&conn, "SELTZER-018");

        bind_stream(&conn, seventeen, stream_id, at(0), Some(at(1_000))).unwrap();

        let err = bind_stream(&conn, eighteen, stream_id, at(500), None).unwrap_err();
        assert_eq!(err.code(), "db.stream_already_bound");
    }

    /// The fermenter turnaround this table exists for. Windows are half-open, so 18
    /// starting at exactly the second 17 ended is not an overlap.
    #[test]
    fn two_batches_may_touch_at_the_boundary() {
        let mut conn = db();
        let (_, stream_id) = stream(&mut conn, "FV1");
        let seventeen = batch(&conn, "SELTZER-017");
        let eighteen = batch(&conn, "SELTZER-018");

        bind_stream(&conn, seventeen, stream_id, at(0), Some(at(1_000))).unwrap();
        bind_stream(&conn, eighteen, stream_id, at(1_000), None).unwrap();

        assert_eq!(
            batch_for_stream_at(&conn, stream_id, at(999)).unwrap(),
            Some(seventeen)
        );
        assert_eq!(
            batch_for_stream_at(&conn, stream_id, at(1_000)).unwrap(),
            Some(eighteen)
        );
    }

    /// A sensor unplugged and reconnected is one batch's curve in two pieces. Nothing
    /// about that is ambiguous, so the overlap rule only looks at *other* batches.
    #[test]
    fn a_batch_may_overlap_its_own_bindings() {
        let mut conn = db();
        let (_, stream_id) = stream(&mut conn, "FV1");
        let batch_id = batch(&conn, "SELTZER-017");

        bind_stream(&conn, batch_id, stream_id, at(0), Some(at(1_000))).unwrap();
        bind_stream(&conn, batch_id, stream_id, at(500), None).unwrap();

        assert_eq!(list_batch_streams(&conn, batch_id).unwrap().len(), 2);
    }

    #[test]
    fn an_open_binding_blocks_everything_after_it() {
        let mut conn = db();
        let (_, stream_id) = stream(&mut conn, "FV1");
        let seventeen = batch(&conn, "SELTZER-017");
        let eighteen = batch(&conn, "SELTZER-018");

        bind_stream(&conn, seventeen, stream_id, at(0), None).unwrap();

        let err = bind_stream(&conn, eighteen, stream_id, at(100_000), None).unwrap_err();
        assert_eq!(err.code(), "db.stream_already_bound");
    }

    #[test]
    fn unbinding_closes_the_window_and_is_idempotent() {
        let mut conn = db();
        let (_, stream_id) = stream(&mut conn, "FV1");
        let batch_id = batch(&conn, "SELTZER-017");
        let binding = bind_stream(&conn, batch_id, stream_id, at(0), None).unwrap();

        assert!(unbind_stream(&conn, binding, at(1_000)).unwrap());
        assert!(!unbind_stream(&conn, binding, at(2_000)).unwrap());

        let rows = list_batch_streams(&conn, batch_id).unwrap();
        assert_eq!(rows[0].ends_at, Some(at(1_000)));
    }

    #[test]
    fn unbinding_a_binding_that_does_not_exist_says_so() {
        let conn = db();
        let err = unbind_stream(&conn, 404, at(0)).unwrap_err();
        assert_eq!(err.code(), "db.no_such_batch_stream");
    }

    #[test]
    fn a_stream_bound_to_nothing_at_that_moment_has_no_batch() {
        let mut conn = db();
        let (_, stream_id) = stream(&mut conn, "FV1");
        let batch_id = batch(&conn, "SELTZER-017");
        bind_stream(&conn, batch_id, stream_id, at(1_000), Some(at(2_000))).unwrap();

        assert_eq!(batch_for_stream_at(&conn, stream_id, at(0)).unwrap(), None);
        assert_eq!(
            batch_for_stream_at(&conn, stream_id, at(2_000)).unwrap(),
            None
        );
    }

    // -------------------------------------------------- annotation import

    fn annotated(conn: &mut Connection, source_id: i64, rows: &[(i64, &str)]) {
        let tx = conn.transaction().unwrap();
        let rows: Vec<_> = rows
            .iter()
            .map(|(offset, text)| (at(*offset), AnnotationChannel::Vessel, *text))
            .collect();
        insert_annotations(&tx, source_id, &rows).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn only_annotations_inside_a_binding_window_are_suggested() {
        let mut conn = db();
        let (source_id, stream_id) = stream(&mut conn, "FV1");
        annotated(
            &mut conn,
            source_id,
            &[(-60, "before"), (600, "added DAP"), (9_000, "after")],
        );

        let batch_id = batch(&conn, "SELTZER-017");
        bind_stream(&conn, batch_id, stream_id, at(0), Some(at(3_600))).unwrap();

        let suggested = suggest_events_for_batch(&conn, batch_id).unwrap();
        assert_eq!(suggested.len(), 1);
        assert_eq!(suggested[0].text, "added DAP");
        assert_eq!(suggested[0].kind, Some(EventKind::NutrientAddition));
    }

    #[test]
    fn an_accepted_suggestion_is_not_offered_again() {
        let mut conn = db();
        let (source_id, stream_id) = stream(&mut conn, "FV1");
        annotated(&mut conn, source_id, &[(600, "cold crash started")]);

        let batch_id = batch(&conn, "SELTZER-017");
        bind_stream(&conn, batch_id, stream_id, at(0), None).unwrap();

        let suggested = suggest_events_for_batch(&conn, batch_id).unwrap();
        assert_eq!(suggested[0].kind, Some(EventKind::ColdCrash));
        accept_suggestion(&conn, batch_id, &suggested[0], EventKind::ColdCrash).unwrap();

        assert!(
            suggest_events_for_batch(&conn, batch_id)
                .unwrap()
                .is_empty()
        );

        let events = list_batch_events(&conn, batch_id).unwrap();
        assert_eq!(events[0].kind, EventKind::ColdCrash);
        assert!(
            events[0]
                .payload
                .as_deref()
                .unwrap()
                .contains("fermentrack_annotation"),
            "{:?}",
            events[0].payload
        );
    }

    /// One log file feeds up to eight streams, and a batch usually binds several of
    /// them. The annotation is one fact about the file, not one per series.
    #[test]
    fn an_annotation_is_suggested_once_however_many_streams_are_bound() {
        let mut conn = db();
        let (source_id, first) = stream(&mut conn, "FV1");
        let tx = conn.transaction().unwrap();
        let second = create_stream(&tx, source_id, "fridge_temp", Metric::FridgeTempC).unwrap();
        tx.commit().unwrap();
        annotated(&mut conn, source_id, &[(600, "racked to secondary")]);

        let batch_id = batch(&conn, "SELTZER-017");
        bind_stream(&conn, batch_id, first, at(0), None).unwrap();
        bind_stream(&conn, batch_id, second, at(0), None).unwrap();

        let suggested = suggest_events_for_batch(&conn, batch_id).unwrap();
        assert_eq!(suggested.len(), 1);
        assert_eq!(suggested[0].kind, Some(EventKind::Transfer));
    }

    #[test]
    fn an_annotation_that_matches_nothing_gets_no_guess() {
        assert_eq!(guess_kind("tastes like grapefruit"), None);
        assert_eq!(guess_kind("Pitched US-05"), Some(EventKind::Pitch));
    }

    /// A suggestion the caller reads as a pitch has to take the pitch door, or the
    /// batch would carry an event its status machine cannot see.
    #[test]
    fn accepting_a_suggestion_as_a_pitch_records_it_properly() {
        let mut conn = db();
        let (source_id, stream_id) = stream(&mut conn, "FV1");
        annotated(&mut conn, source_id, &[(600, "Pitched US-05")]);

        let batch_id = batch(&conn, "SELTZER-017");
        bind_stream(&conn, batch_id, stream_id, at(0), None).unwrap();

        let suggested = suggest_events_for_batch(&conn, batch_id).unwrap();
        accept_suggestion(&conn, batch_id, &suggested[0], EventKind::Pitch).unwrap();

        let row = get_batch(&conn, batch_id).unwrap();
        assert_eq!(row.pitched_at, Some(at(600)));
        assert_eq!(row.status, BatchStatus::Fermenting);
    }
}
