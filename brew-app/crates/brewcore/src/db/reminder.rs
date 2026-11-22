//! Day-N reminders: a check on the calendar, counted from the pitch. See
//! `0014_reminder.sql`.
//!
//! **Done is an event**, as for [`schedule`](crate::db::schedule): [`complete_reminder`]
//! writes a `reminder_done` `batch_event` and links the reminder to it, so the timeline
//! stays the one record; deleting the event un-ticks the reminder.
//!
//! Same contract as the rest of `db`: a borrowed connection, never a transaction of its
//! own except in [`write_reminder`].

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    batch::{ensure_open, get_batch},
    timeline::{EventKind, NewBatchEvent, add_event, delete_event},
};
use crate::{Error, Result, units::Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewReminder {
    /// Since the pitch.
    pub after_hours: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReminderRow {
    pub id: i64,
    pub batch_id: i64,
    pub after_hours: f64,
    pub text: String,
    pub done_event_id: Option<i64>,
    /// The `reminder_done` event's time.
    pub done_at: Option<Timestamp>,
}

/// A batch's reminders and the pitch they count from.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Reminders {
    pub reminders: Vec<ReminderRow>,
    pub pitched_at: Option<Timestamp>,
}

const COLUMNS: &str = "r.id, r.batch_id, r.after_hours, r.text, r.done_event_id, e.ts";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReminderRow> {
    Ok(ReminderRow {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        after_hours: row.get(2)?,
        text: row.get(3)?,
        done_event_id: row.get(4)?,
        done_at: row.get(5)?,
    })
}

fn validate(reminder: &NewReminder) -> Result<()> {
    if reminder.text.trim().is_empty() {
        return Err(Error::EntryEmpty);
    }
    let value = reminder.after_hours;
    if !value.is_finite() {
        return Err(Error::NotFinite {
            field: "after_hours",
            value,
        });
    }
    if value < 0.0 {
        return Err(Error::NotPositive {
            field: "after_hours",
            value,
        });
    }
    Ok(())
}

pub fn batch_reminders(conn: &Connection, batch_id: i64) -> Result<Reminders> {
    Ok(Reminders {
        pitched_at: get_batch(conn, batch_id)?.pitched_at,
        reminders: list_reminders(conn, batch_id)?,
    })
}

/// One write against a reminder in its own transaction, then the batch's reminders —
/// what every reminder command answers with.
pub fn write_reminder(
    conn: &mut Connection,
    reminder_id: i64,
    write: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Reminders> {
    let tx = conn.transaction()?;
    let batch_id = get_reminder(&tx, reminder_id)?.batch_id;
    write(&tx)?;
    let reminders = batch_reminders(&tx, batch_id)?;
    tx.commit()?;
    Ok(reminders)
}

/// Soonest first.
pub fn list_reminders(conn: &Connection, batch_id: i64) -> Result<Vec<ReminderRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM batch_reminder r \
         LEFT JOIN batch_event e ON e.id = r.done_event_id \
         WHERE r.batch_id = ? ORDER BY r.after_hours, r.id"
    ))?;
    let rows = stmt.query_map([batch_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_reminder(conn: &Connection, reminder_id: i64) -> Result<ReminderRow> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM batch_reminder r \
             LEFT JOIN batch_event e ON e.id = r.done_event_id WHERE r.id = ?"
        ),
        [reminder_id],
        from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchReminder { reminder_id })
}

pub fn add_reminder(conn: &Connection, batch_id: i64, reminder: &NewReminder) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;
    validate(reminder)?;
    conn.execute(
        "INSERT INTO batch_reminder (batch_id, after_hours, text) VALUES (?, ?, ?)",
        params![batch_id, reminder.after_hours, reminder.text.trim()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_reminder(conn: &Connection, reminder_id: i64, reminder: &NewReminder) -> Result<()> {
    let current = get_reminder(conn, reminder_id)?;
    ensure_open(&get_batch(conn, current.batch_id)?)?;
    validate(reminder)?;
    conn.execute(
        "UPDATE batch_reminder SET after_hours = ?, text = ? WHERE id = ?",
        params![reminder.after_hours, reminder.text.trim(), reminder_id],
    )?;
    Ok(())
}

/// The `reminder_done` event, if any, stays on the timeline: it happened.
pub fn delete_reminder(conn: &Connection, reminder_id: i64) -> Result<()> {
    let reminder = get_reminder(conn, reminder_id)?;
    ensure_open(&get_batch(conn, reminder.batch_id)?)?;
    conn.execute("DELETE FROM batch_reminder WHERE id = ?", [reminder_id])?;
    Ok(())
}

/// Writes the `reminder_done` event and links it. Returns the event's id.
pub fn complete_reminder(
    conn: &Connection,
    reminder_id: i64,
    at: Timestamp,
    note: Option<&str>,
) -> Result<i64> {
    let reminder = get_reminder(conn, reminder_id)?;
    if reminder.done_event_id.is_some() {
        return Err(Error::ReminderDone { reminder_id });
    }
    let event_id = add_event(
        conn,
        reminder.batch_id,
        &NewBatchEvent {
            ts: at,
            kind: EventKind::ReminderDone,
            payload: Some(serde_json::json!({ "reminder_id": reminder_id }).to_string()),
            note: Some(match note.map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => format!("{}: {n}", reminder.text),
                None => reminder.text.clone(),
            }),
        },
    )?;
    conn.execute(
        "UPDATE batch_reminder SET done_event_id = ? WHERE id = ?",
        params![event_id, reminder_id],
    )?;
    Ok(event_id)
}

/// Un-tick, by deleting its event.
pub fn undo_reminder(conn: &Connection, reminder_id: i64) -> Result<()> {
    match get_reminder(conn, reminder_id)?.done_event_id {
        Some(event_id) => {
            delete_event(conn, event_id)?;
            Ok(())
        }
        None => Err(Error::ReminderNotDone { reminder_id }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, archive_batch, create_batch},
        recipe::BeverageType,
        timeline::{list_batch_events, record_pitch},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection) -> i64 {
        let id = create_batch(
            conn,
            &NewBatch {
                code: "B1".into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Mead,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: None,
            },
            T0,
        )
        .unwrap();
        record_pitch(conn, id, T0, None).unwrap();
        id
    }

    fn day(n: f64, text: &str) -> NewReminder {
        NewReminder {
            after_hours: n * 24.0,
            text: text.into(),
        }
    }

    #[test]
    fn reminders_list_soonest_first_and_refuse_nonsense() {
        let conn = db();
        let id = batch(&conn);
        add_reminder(&conn, id, &day(10.0, "Taste")).unwrap();
        add_reminder(&conn, id, &day(2.0, "  Degas ")).unwrap();
        let texts: Vec<_> = list_reminders(&conn, id)
            .unwrap()
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(texts, ["Degas", "Taste"]);

        assert_eq!(
            add_reminder(&conn, id, &day(1.0, " ")).unwrap_err().code(),
            Error::EntryEmpty.code()
        );
        assert!(add_reminder(&conn, id, &day(-1.0, "x")).is_err());
        assert!(add_reminder(&conn, id, &day(f64::NAN, "x")).is_err());
    }

    #[test]
    fn done_is_an_event_and_deleting_it_un_ticks() {
        let conn = db();
        let id = batch(&conn);
        let r = add_reminder(&conn, id, &day(10.0, "Taste")).unwrap();
        let at = T0.saturating_add_secs(240 * 3_600);
        let event = complete_reminder(&conn, r, at, Some("a bit hot")).unwrap();

        let done = get_reminder(&conn, r).unwrap();
        assert_eq!((done.done_event_id, done.done_at), (Some(event), Some(at)));
        let events = list_batch_events(&conn, id).unwrap();
        let e = events.iter().find(|e| e.id == event).unwrap();
        assert_eq!(e.kind, EventKind::ReminderDone);
        assert_eq!(e.note.as_deref(), Some("Taste: a bit hot"));
        assert_eq!(
            complete_reminder(&conn, r, at, None).unwrap_err().code(),
            "db.reminder_done"
        );

        undo_reminder(&conn, r).unwrap();
        assert_eq!(get_reminder(&conn, r).unwrap().done_event_id, None);
        assert_eq!(
            undo_reminder(&conn, r).unwrap_err().code(),
            "db.reminder_not_done"
        );
    }

    #[test]
    fn an_archived_batchs_reminders_are_read_only() {
        let conn = db();
        let id = batch(&conn);
        let r = add_reminder(&conn, id, &day(1.0, "Degas")).unwrap();
        archive_batch(&conn, id, T0).unwrap();
        assert!(add_reminder(&conn, id, &day(2.0, "x")).is_err());
        assert!(update_reminder(&conn, r, &day(2.0, "x")).is_err());
        assert!(delete_reminder(&conn, r).is_err());
    }
}
