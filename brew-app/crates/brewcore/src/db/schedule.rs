//! A batch's fermentation schedule: ordered steps, each with the targets that say
//! it's time to move on. See `0013_schedule.sql`.
//!
//! **Done is an event.** [`complete_step`] writes a `step_done` `batch_event` and links
//! the step to it, so the timeline stays the one record; deleting the event un-ticks
//! the step. Steps are done in order, because a step starts when the one before it is
//! done — the first at the pitch.
//!
//! Same contract as [`timeline`](crate::db::timeline): a borrowed connection, never a
//! transaction of its own. [`complete_step`] and [`move_step`] are two writes each, so
//! callers run them in one.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    batch::{ensure_open, get_batch},
    timeline::{EventKind, NewBatchEvent, add_event, delete_event},
};
use crate::{
    Error, Result,
    units::{Sg, Timestamp},
};

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewStep {
    pub name: String,
    pub temp_min_c: Option<f64>,
    pub temp_max_c: Option<f64>,
    /// Reached once gravity is at or below it.
    pub target_sg: Option<Sg>,
    /// Since the step started.
    pub min_hours: Option<f64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StepRow {
    pub id: i64,
    pub batch_id: i64,
    pub position: i64,
    pub name: String,
    pub temp_min_c: Option<f64>,
    pub temp_max_c: Option<f64>,
    pub target_sg: Option<Sg>,
    pub min_hours: Option<f64>,
    pub note: Option<String>,
    pub done_event_id: Option<i64>,
    /// The `step_done` event's time.
    pub done_at: Option<Timestamp>,
}

const STEP_COLUMNS: &str = "s.id, s.batch_id, s.position, s.name, s.temp_min_c, s.temp_max_c, \
                            s.target_sg, s.min_hours, s.note, s.done_event_id, e.ts";

fn step_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StepRow> {
    Ok(StepRow {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        position: row.get(2)?,
        name: row.get(3)?,
        temp_min_c: row.get(4)?,
        temp_max_c: row.get(5)?,
        target_sg: row.get(6)?,
        min_hours: row.get(7)?,
        note: row.get(8)?,
        done_event_id: row.get(9)?,
        done_at: row.get(10)?,
    })
}

fn validate(step: &NewStep) -> Result<()> {
    if step.name.trim().is_empty() {
        return Err(Error::EntryEmpty);
    }
    for (field, value) in [
        ("temp_min_c", step.temp_min_c),
        ("temp_max_c", step.temp_max_c),
        ("min_hours", step.min_hours),
    ] {
        if let Some(value) = value.filter(|v| !v.is_finite()) {
            return Err(Error::NotFinite { field, value });
        }
    }
    if let (Some(lo), Some(hi)) = (step.temp_min_c, step.temp_max_c) {
        if lo > hi {
            return Err(Error::RangeInverted { field: "temp band" });
        }
    }
    if let Some(value) = step.min_hours.filter(|h| *h < 0.0) {
        return Err(Error::NotPositive {
            field: "min_hours",
            value,
        });
    }
    Ok(())
}

/// A batch's steps and the pitch the first one starts at.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Schedule {
    pub steps: Vec<StepRow>,
    pub pitched_at: Option<Timestamp>,
}

pub fn batch_schedule(conn: &Connection, batch_id: i64) -> Result<Schedule> {
    Ok(Schedule {
        pitched_at: get_batch(conn, batch_id)?.pitched_at,
        steps: list_steps(conn, batch_id)?,
    })
}

/// One write against a step in its own transaction, then the schedule it belongs to —
/// what every step command answers with.
pub fn write_step(
    conn: &mut Connection,
    step_id: i64,
    write: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Schedule> {
    let tx = conn.transaction()?;
    let batch_id = get_step(&tx, step_id)?.batch_id;
    write(&tx)?;
    let schedule = batch_schedule(&tx, batch_id)?;
    tx.commit()?;
    Ok(schedule)
}

/// In order.
pub fn list_steps(conn: &Connection, batch_id: i64) -> Result<Vec<StepRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {STEP_COLUMNS} FROM schedule_step s \
         LEFT JOIN batch_event e ON e.id = s.done_event_id \
         WHERE s.batch_id = ? ORDER BY s.position"
    ))?;
    let rows = stmt.query_map([batch_id], step_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_step(conn: &Connection, step_id: i64) -> Result<StepRow> {
    conn.query_row(
        &format!(
            "SELECT {STEP_COLUMNS} FROM schedule_step s \
             LEFT JOIN batch_event e ON e.id = s.done_event_id WHERE s.id = ?"
        ),
        [step_id],
        step_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchScheduleStep { step_id })
}

/// Appended at the end.
pub fn add_step(conn: &Connection, batch_id: i64, step: &NewStep) -> Result<i64> {
    ensure_open(&get_batch(conn, batch_id)?)?;
    validate(step)?;
    conn.execute(
        "INSERT INTO schedule_step \
         (batch_id, position, name, temp_min_c, temp_max_c, target_sg, min_hours, note) \
         VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM schedule_step WHERE batch_id = ?1), \
                 ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            batch_id,
            step.name.trim(),
            step.temp_min_c,
            step.temp_max_c,
            step.target_sg,
            step.min_hours,
            step.note
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_step(conn: &Connection, step_id: i64, step: &NewStep) -> Result<()> {
    let current = get_step(conn, step_id)?;
    ensure_open(&get_batch(conn, current.batch_id)?)?;
    validate(step)?;
    conn.execute(
        "UPDATE schedule_step SET name = ?, temp_min_c = ?, temp_max_c = ?, target_sg = ?, \
         min_hours = ?, note = ? WHERE id = ?",
        params![
            step.name.trim(),
            step.temp_min_c,
            step.temp_max_c,
            step.target_sg,
            step.min_hours,
            step.note,
            step_id
        ],
    )?;
    Ok(())
}

/// The `step_done` event, if any, stays on the timeline: it happened.
pub fn delete_step(conn: &Connection, step_id: i64) -> Result<()> {
    let step = get_step(conn, step_id)?;
    ensure_open(&get_batch(conn, step.batch_id)?)?;
    conn.execute("DELETE FROM schedule_step WHERE id = ?", [step_id])?;
    renumber(conn, step.batch_id)
}

/// Positions back to `1..=n`, in their current order. Through negatives, since
/// `UNIQUE (batch_id, position)` is checked per row.
fn renumber(conn: &Connection, batch_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE schedule_step SET position = -position WHERE batch_id = ?",
        [batch_id],
    )?;
    conn.execute(
        "UPDATE schedule_step SET position = \
         (SELECT COUNT(*) FROM schedule_step o WHERE o.batch_id = schedule_step.batch_id \
          AND o.position >= schedule_step.position) \
         WHERE batch_id = ?",
        [batch_id],
    )?;
    Ok(())
}

/// Swap with the neighbour above (`up`) or below. `false` at either end.
pub fn move_step(conn: &Connection, step_id: i64, up: bool) -> Result<bool> {
    let step = get_step(conn, step_id)?;
    ensure_open(&get_batch(conn, step.batch_id)?)?;
    let other = step.position + if up { -1 } else { 1 };
    let neighbour: Option<i64> = conn
        .query_row(
            "SELECT id FROM schedule_step WHERE batch_id = ? AND position = ?",
            params![step.batch_id, other],
            |row| row.get(0),
        )
        .optional()?;
    let Some(neighbour) = neighbour else {
        return Ok(false);
    };
    conn.execute(
        "UPDATE schedule_step SET position = 0 WHERE id = ?",
        [step_id],
    )?;
    conn.execute(
        "UPDATE schedule_step SET position = ? WHERE id = ?",
        params![step.position, neighbour],
    )?;
    conn.execute(
        "UPDATE schedule_step SET position = ? WHERE id = ?",
        params![other, step_id],
    )?;
    Ok(true)
}

/// When `steps[i]` started: the step before it done, the first at the pitch. `None`
/// until then.
pub fn step_start(steps: &[StepRow], i: usize, pitched_at: Option<Timestamp>) -> Option<Timestamp> {
    match i {
        0 => pitched_at,
        _ => steps.get(i - 1).and_then(|s| s.done_at),
    }
}

/// Tick the current step: the first not done. Writes the `step_done` event and links
/// it. Returns the event's id.
pub fn complete_step(
    conn: &Connection,
    step_id: i64,
    at: Timestamp,
    note: Option<&str>,
) -> Result<i64> {
    let step = get_step(conn, step_id)?;
    let batch = get_batch(conn, step.batch_id)?;
    let steps = list_steps(conn, step.batch_id)?;
    let i = steps
        .iter()
        .position(|s| s.done_event_id.is_none())
        .filter(|&i| steps[i].id == step_id)
        .ok_or(Error::StepOutOfOrder { step_id })?;
    let start = step_start(&steps, i, batch.pitched_at)
        .ok_or(Error::BatchNotPitched { batch_id: batch.id })?;
    if at < start {
        return Err(Error::StepBeforeStart { step_id });
    }
    let event_id = add_event(
        conn,
        step.batch_id,
        &NewBatchEvent {
            ts: at,
            kind: EventKind::StepDone,
            payload: Some(serde_json::json!({ "step_id": step_id }).to_string()),
            note: Some(match note.map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => format!("{}: {n}", step.name),
                None => step.name.clone(),
            }),
        },
    )?;
    conn.execute(
        "UPDATE schedule_step SET done_event_id = ? WHERE id = ?",
        params![event_id, step_id],
    )?;
    Ok(event_id)
}

/// Un-tick the last done step, by deleting its event. Only the last: an earlier one
/// would leave the steps after it done before they started.
pub fn undo_step(conn: &Connection, step_id: i64) -> Result<()> {
    let step = get_step(conn, step_id)?;
    let steps = list_steps(conn, step.batch_id)?;
    let last = steps.iter().rev().find(|s| s.done_event_id.is_some());
    match (last, step.done_event_id) {
        (Some(last), Some(event_id)) if last.id == step_id => {
            delete_event(conn, event_id)?;
            Ok(())
        }
        _ => Err(Error::StepOutOfOrder { step_id }),
    }
}

/// A W-34/70 lager schedule, for a batch with none. Gravity targets from the OG where
/// there is one: half the sugar gone for the diacetyl rest, three quarters (about
/// 2–3 points above this yeast's FG) before ramping down.
pub fn seed_lager_schedule(conn: &Connection, batch_id: i64) -> Result<()> {
    let batch = get_batch(conn, batch_id)?;
    ensure_open(&batch)?;
    if !list_steps(conn, batch_id)?.is_empty() {
        return Err(Error::ScheduleNotEmpty { batch_id });
    }
    let at = |fraction: f64, fallback: f64| {
        let sg = batch.og.map_or(fallback, |og| {
            let og = og.get();
            ((og - (og - 1.0) * fraction) * 1000.0).round() / 1000.0
        });
        Some(Sg::new_unchecked(sg))
    };
    let step = |name: &str, band: (f64, f64), target_sg, min_hours, note: &str| NewStep {
        name: name.into(),
        temp_min_c: Some(band.0),
        temp_max_c: Some(band.1),
        target_sg,
        min_hours,
        note: Some(note.into()).filter(|n: &String| !n.is_empty()),
    };
    let steps = [
        step(
            "Primary",
            (9.0, 12.0),
            at(0.5, 1.020),
            Some(72.0),
            "Fridge at 10 °C.",
        ),
        step(
            "Diacetyl rest",
            (16.0, 18.0),
            at(0.75, 1.012),
            Some(48.0),
            "Let it free-rise to 16–18 °C. Done when a taste shows no butter.",
        ),
        step(
            "Ramp down",
            (2.0, 4.0),
            None,
            Some(48.0),
            "About 2 °C a day.",
        ),
        step("Lager / cold crash", (0.0, 2.0), None, Some(336.0), ""),
        NewStep {
            name: "Package".into(),
            ..NewStep::default()
        },
    ];
    for s in &steps {
        add_step(conn, batch_id, s)?;
    }
    Ok(())
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

    fn batch(conn: &Connection, og: Option<f64>, pitch: bool) -> i64 {
        let id = create_batch(
            conn,
            &NewBatch {
                code: "B1".into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Beer,
                vessel_id: None,
                volume_l: None,
                og: og.map(Sg::new_unchecked),
                notes: None,
            },
            T0,
        )
        .unwrap();
        if pitch {
            record_pitch(conn, id, T0, None).unwrap();
        }
        id
    }

    fn named(name: &str) -> NewStep {
        NewStep {
            name: name.into(),
            ..NewStep::default()
        }
    }

    fn names(conn: &Connection, batch_id: i64) -> Vec<String> {
        list_steps(conn, batch_id)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    const fn at(hours: i64) -> Timestamp {
        Timestamp::from_secs(1_700_000_000 + hours * 3_600)
    }

    #[test]
    fn steps_append_move_and_renumber() {
        let conn = db();
        let b = batch(&conn, None, false);
        let a = add_step(&conn, b, &named("a")).unwrap();
        let s2 = add_step(&conn, b, &named("b")).unwrap();
        add_step(&conn, b, &named("c")).unwrap();
        assert!(move_step(&conn, a, false).unwrap());
        assert_eq!(names(&conn, b), ["b", "a", "c"]);
        assert!(!move_step(&conn, s2, true).unwrap());
        delete_step(&conn, a).unwrap();
        assert_eq!(names(&conn, b), ["b", "c"]);
        let positions: Vec<_> = list_steps(&conn, b)
            .unwrap()
            .iter()
            .map(|s| s.position)
            .collect();
        assert_eq!(positions, [1, 2]);
    }

    #[test]
    fn a_step_is_validated() {
        let conn = db();
        let b = batch(&conn, None, false);
        assert!(matches!(
            add_step(&conn, b, &named(" ")),
            Err(Error::EntryEmpty)
        ));
        let inverted = NewStep {
            temp_min_c: Some(12.0),
            temp_max_c: Some(10.0),
            ..named("x")
        };
        assert!(matches!(
            add_step(&conn, b, &inverted),
            Err(Error::RangeInverted { .. })
        ));
    }

    #[test]
    fn steps_complete_in_order_as_events() {
        let conn = db();
        let b = batch(&conn, None, true);
        let first = add_step(&conn, b, &named("Primary")).unwrap();
        let second = add_step(&conn, b, &named("Rest")).unwrap();

        assert!(matches!(
            complete_step(&conn, second, at(1), None),
            Err(Error::StepOutOfOrder { .. })
        ));
        assert!(matches!(
            complete_step(&conn, first, at(-1), None),
            Err(Error::StepBeforeStart { .. })
        ));

        complete_step(&conn, first, at(72), Some("tasted clean")).unwrap();
        let steps = list_steps(&conn, b).unwrap();
        assert_eq!(steps[0].done_at, Some(at(72)));
        assert_eq!(step_start(&steps, 1, Some(T0)), Some(at(72)));
        let events = list_batch_events(&conn, b).unwrap();
        let done = events
            .iter()
            .find(|e| e.kind == EventKind::StepDone)
            .unwrap();
        assert_eq!(done.note.as_deref(), Some("Primary: tasted clean"));

        assert!(matches!(
            complete_step(&conn, second, at(70), None),
            Err(Error::StepBeforeStart { .. })
        ));
    }

    #[test]
    fn deleting_the_event_unticks_the_step() {
        let conn = db();
        let b = batch(&conn, None, true);
        let first = add_step(&conn, b, &named("Primary")).unwrap();
        let event = complete_step(&conn, first, at(1), None).unwrap();
        delete_event(&conn, event).unwrap();
        assert_eq!(get_step(&conn, first).unwrap().done_event_id, None);
    }

    #[test]
    fn only_the_last_done_step_is_undone() {
        let conn = db();
        let b = batch(&conn, None, true);
        let first = add_step(&conn, b, &named("a")).unwrap();
        let second = add_step(&conn, b, &named("b")).unwrap();
        complete_step(&conn, first, at(1), None).unwrap();
        complete_step(&conn, second, at(2), None).unwrap();
        assert!(matches!(
            undo_step(&conn, first),
            Err(Error::StepOutOfOrder { .. })
        ));
        undo_step(&conn, second).unwrap();
        assert_eq!(get_step(&conn, second).unwrap().done_at, None);
        assert!(get_step(&conn, first).unwrap().done_at.is_some());
    }

    #[test]
    fn an_unpitched_batch_has_no_current_step_to_tick() {
        let conn = db();
        let b = batch(&conn, None, false);
        let first = add_step(&conn, b, &named("a")).unwrap();
        assert!(matches!(
            complete_step(&conn, first, at(1), None),
            Err(Error::BatchNotPitched { .. })
        ));
    }

    #[test]
    fn the_lager_template_targets_the_og() {
        let conn = db();
        let b = batch(&conn, Some(1.046), false);
        seed_lager_schedule(&conn, b).unwrap();
        let steps = list_steps(&conn, b).unwrap();
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[0].target_sg.map(Sg::get), Some(1.023));
        assert_eq!(steps[1].target_sg.map(Sg::get), Some(1.012));
        assert!(matches!(
            seed_lager_schedule(&conn, b),
            Err(Error::ScheduleNotEmpty { .. })
        ));
    }

    #[test]
    fn an_archived_batch_takes_no_steps() {
        let conn = db();
        let b = batch(&conn, None, false);
        archive_batch(&conn, b, T0).unwrap();
        assert!(matches!(
            add_step(&conn, b, &named("a")),
            Err(Error::BatchArchived { .. })
        ));
    }
}
