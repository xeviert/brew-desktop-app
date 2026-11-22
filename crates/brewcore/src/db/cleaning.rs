//! Phase 7b. The cleaning and sanitation log, per vessel. See `0016_cleaning.sql`.
//!
//! [`vessel_hygiene`] is what the log is for: whether a vessel was sanitised since the
//! last batch in it, asked by the Vessels pane and by the pitch. It warns and never
//! refuses: a missing entry is a gap in the log at least as often as a dirty vessel,
//! and a pitch that happened must still be recordable.
//!
//! An entry is a real delete, as an event is: a mistyped one records nothing.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    batch::BatchStatus,
    enums::db_enum,
    reference::{VesselRow, get_vessel},
};
use crate::{Error, Result, units::Timestamp};

db_enum!(
    /// What was done. `CleanAndSanitize` counts as both. Not a `CHECK` in the schema —
    /// see `0016_cleaning.sql`.
    CleaningKind,
    UnknownCleaningKind,
    (Clean, "clean"),
    (Sanitize, "sanitize"),
    (CleanAndSanitize, "clean_and_sanitize"),
    (Rinse, "rinse"),
);

impl CleaningKind {
    pub fn cleans(self) -> bool {
        matches!(self, Self::Clean | Self::CleanAndSanitize)
    }

    pub fn sanitizes(self) -> bool {
        matches!(self, Self::Sanitize | Self::CleanAndSanitize)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewCleaning {
    pub kind: CleaningKind,
    pub chemical: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CleaningRow {
    pub id: i64,
    pub vessel_id: i64,
    pub cleaned_at: Timestamp,
    pub kind: CleaningKind,
    pub chemical: Option<String>,
    pub notes: Option<String>,
}

/// The latest batch pitched into a vessel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VesselBatch {
    pub id: i64,
    pub code: String,
    pub pitched_at: Timestamp,
    /// Fermenting or conditioning, and not archived: the batch is in the vessel now.
    pub in_vessel: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VesselHygiene {
    pub vessel_id: i64,
    pub last_cleaned: Option<CleaningRow>,
    pub last_sanitized: Option<CleaningRow>,
    pub last_batch: Option<VesselBatch>,
    /// A batch has been in it since the last sanitise, or it never had one, and the
    /// vessel is empty now. Measured from that batch's pitch: a sanitise while it was
    /// still in the vessel is not a thing that happens.
    pub needs_sanitizing: bool,
}

/// A vessel's log, newest first, and what it adds up to.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CleaningLog {
    pub entries: Vec<CleaningRow>,
    pub hygiene: VesselHygiene,
}

const COLUMNS: &str = "id, vessel_id, cleaned_at, kind, chemical, notes";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CleaningRow> {
    Ok(CleaningRow {
        id: row.get(0)?,
        vessel_id: row.get(1)?,
        cleaned_at: row.get(2)?,
        kind: row.get(3)?,
        chemical: row.get(4)?,
        notes: row.get(5)?,
    })
}

fn trimmed(text: &Option<String>) -> Option<&str> {
    text.as_deref().map(str::trim).filter(|t| !t.is_empty())
}

fn open_vessel(conn: &Connection, vessel_id: i64) -> Result<VesselRow> {
    let vessel = get_vessel(conn, vessel_id)?;
    match vessel.archived_at {
        None => Ok(vessel),
        Some(_) => Err(Error::VesselArchived { vessel_id }),
    }
}

/// Newest first.
pub fn list_cleanings(conn: &Connection, vessel_id: i64) -> Result<Vec<CleaningRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM vessel_cleaning WHERE vessel_id = ? \
         ORDER BY cleaned_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([vessel_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_cleaning(conn: &Connection, cleaning_id: i64) -> Result<CleaningRow> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM vessel_cleaning WHERE id = ?"),
        [cleaning_id],
        from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchCleaning { cleaning_id })
}

pub fn cleaning_log(conn: &Connection, vessel_id: i64) -> Result<CleaningLog> {
    Ok(CleaningLog {
        hygiene: vessel_hygiene(conn, vessel_id)?,
        entries: list_cleanings(conn, vessel_id)?,
    })
}

pub fn vessel_hygiene(conn: &Connection, vessel_id: i64) -> Result<VesselHygiene> {
    get_vessel(conn, vessel_id)?;
    let entries = list_cleanings(conn, vessel_id)?;
    let last_cleaned = entries.iter().find(|e| e.kind.cleans()).cloned();
    let last_sanitized = entries.iter().find(|e| e.kind.sanitizes()).cloned();

    let last_batch = conn
        .query_row(
            "SELECT id, code, pitched_at, status, archived_at FROM batch \
             WHERE vessel_id = ? AND pitched_at IS NOT NULL \
             ORDER BY pitched_at DESC, id DESC LIMIT 1",
            [vessel_id],
            |row| {
                let status: BatchStatus = row.get(3)?;
                let archived_at: Option<Timestamp> = row.get(4)?;
                Ok(VesselBatch {
                    id: row.get(0)?,
                    code: row.get(1)?,
                    pitched_at: row.get(2)?,
                    in_vessel: archived_at.is_none()
                        && matches!(status, BatchStatus::Fermenting | BatchStatus::Conditioning),
                })
            },
        )
        .optional()?;

    let needs_sanitizing = match &last_batch {
        Some(b) if !b.in_vessel => last_sanitized
            .as_ref()
            .is_none_or(|s| s.cleaned_at < b.pitched_at),
        _ => false,
    };
    Ok(VesselHygiene {
        vessel_id,
        last_cleaned,
        last_sanitized,
        last_batch,
        needs_sanitizing,
    })
}

pub fn add_cleaning(
    conn: &Connection,
    vessel_id: i64,
    at: Timestamp,
    entry: &NewCleaning,
) -> Result<i64> {
    open_vessel(conn, vessel_id)?;
    conn.execute(
        "INSERT INTO vessel_cleaning (vessel_id, cleaned_at, kind, chemical, notes) \
         VALUES (?, ?, ?, ?, ?)",
        params![
            vessel_id,
            at,
            entry.kind,
            trimmed(&entry.chemical),
            trimmed(&entry.notes)
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_cleaning(
    conn: &Connection,
    cleaning_id: i64,
    at: Timestamp,
    entry: &NewCleaning,
) -> Result<()> {
    open_vessel(conn, get_cleaning(conn, cleaning_id)?.vessel_id)?;
    conn.execute(
        "UPDATE vessel_cleaning SET cleaned_at = ?, kind = ?, chemical = ?, notes = ? \
         WHERE id = ?",
        params![
            at,
            entry.kind,
            trimmed(&entry.chemical),
            trimmed(&entry.notes),
            cleaning_id
        ],
    )?;
    Ok(())
}

pub fn delete_cleaning(conn: &Connection, cleaning_id: i64) -> Result<()> {
    open_vessel(conn, get_cleaning(conn, cleaning_id)?.vessel_id)?;
    conn.execute("DELETE FROM vessel_cleaning WHERE id = ?", [cleaning_id])?;
    Ok(())
}

/// One write against an entry in its own transaction, then the vessel's log — what every
/// entry command answers with.
pub fn write_cleaning(
    conn: &mut Connection,
    cleaning_id: i64,
    write: impl FnOnce(&Connection) -> Result<()>,
) -> Result<CleaningLog> {
    let tx = conn.transaction()?;
    let vessel_id = get_cleaning(&tx, cleaning_id)?.vessel_id;
    write(&tx)?;
    let log = cleaning_log(&tx, vessel_id)?;
    tx.commit()?;
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, archive_batch, create_batch, transition_batch},
        recipe::BeverageType,
        reference::{VesselKind, archive_vessel, create_vessel, restore_vessel, update_vessel},
        timeline::record_pitch,
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const DAY: i64 = 86_400;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn day(n: i64) -> Timestamp {
        T0.saturating_add_secs(n * DAY)
    }

    fn vessel(conn: &Connection) -> i64 {
        create_vessel(conn, "FV1", VesselKind::Fermenter, None, None).unwrap()
    }

    fn pitched(conn: &Connection, code: &str, vessel_id: i64, at: Timestamp) -> i64 {
        let id = create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: Some(vessel_id),
                volume_l: None,
                og: None,
                notes: None,
            },
            at,
        )
        .unwrap();
        record_pitch(conn, id, at, None).unwrap();
        id
    }

    fn entry(kind: CleaningKind) -> NewCleaning {
        NewCleaning {
            kind,
            chemical: Some(" Star San ".into()),
            notes: Some("  ".into()),
        }
    }

    #[test]
    fn the_log_is_newest_first_and_trimmed() {
        let conn = db();
        let v = vessel(&conn);
        add_cleaning(&conn, v, day(1), &entry(CleaningKind::Clean)).unwrap();
        add_cleaning(&conn, v, day(2), &entry(CleaningKind::Sanitize)).unwrap();
        let log = cleaning_log(&conn, v).unwrap();
        assert_eq!(log.entries[0].kind, CleaningKind::Sanitize);
        assert_eq!(log.entries[0].chemical.as_deref(), Some("Star San"));
        assert_eq!(log.entries[0].notes, None);
        assert_eq!(log.hygiene.last_cleaned.unwrap().cleaned_at, day(1));
        assert_eq!(log.hygiene.last_sanitized.unwrap().cleaned_at, day(2));
        assert!(
            !log.hygiene.needs_sanitizing,
            "never used, nothing to warn of"
        );
    }

    #[test]
    fn a_vessel_needs_sanitising_once_a_batch_has_left_it() {
        let conn = db();
        let v = vessel(&conn);
        add_cleaning(&conn, v, day(0), &entry(CleaningKind::CleanAndSanitize)).unwrap();
        let b = pitched(&conn, "B1", v, day(1));

        let h = vessel_hygiene(&conn, v).unwrap();
        assert!(h.last_batch.as_ref().unwrap().in_vessel);
        assert!(!h.needs_sanitizing, "in use, not dirty");

        transition_batch(&conn, b, BatchStatus::Conditioning, day(8)).unwrap();
        transition_batch(&conn, b, BatchStatus::Packaged, day(14)).unwrap();
        let h = vessel_hygiene(&conn, v).unwrap();
        assert_eq!(h.last_batch.as_ref().unwrap().code, "B1");
        assert!(h.needs_sanitizing);

        add_cleaning(&conn, v, day(14), &entry(CleaningKind::Clean)).unwrap();
        assert!(
            vessel_hygiene(&conn, v).unwrap().needs_sanitizing,
            "cleaned is not sanitised"
        );
        add_cleaning(&conn, v, day(15), &entry(CleaningKind::Sanitize)).unwrap();
        assert!(!vessel_hygiene(&conn, v).unwrap().needs_sanitizing);
    }

    #[test]
    fn an_archived_batch_has_left_its_vessel() {
        let conn = db();
        let v = vessel(&conn);
        let b = pitched(&conn, "B1", v, day(1));
        archive_batch(&conn, b, day(3)).unwrap();
        let h = vessel_hygiene(&conn, v).unwrap();
        assert!(!h.last_batch.unwrap().in_vessel);
        assert!(h.needs_sanitizing, "never sanitised at all");
    }

    #[test]
    fn entries_edit_and_delete_until_the_vessel_is_archived() {
        let mut conn = db();
        let v = vessel(&conn);
        let id = add_cleaning(&conn, v, day(1), &entry(CleaningKind::Rinse)).unwrap();
        let log = write_cleaning(&mut conn, id, |tx| {
            update_cleaning(tx, id, day(2), &entry(CleaningKind::Sanitize))
        })
        .unwrap();
        assert_eq!(
            (log.entries[0].kind, log.entries[0].cleaned_at),
            (CleaningKind::Sanitize, day(2))
        );

        archive_vessel(&conn, v, day(3)).unwrap();
        let code = |r: Result<()>| r.unwrap_err().code();
        assert_eq!(
            code(add_cleaning(&conn, v, day(4), &entry(CleaningKind::Clean)).map(|_| ())),
            "db.vessel_archived"
        );
        assert_eq!(code(delete_cleaning(&conn, id)), "db.vessel_archived");
        assert_eq!(
            code(update_vessel(
                &conn,
                v,
                "FV2",
                VesselKind::Fermenter,
                None,
                None
            )),
            "db.vessel_archived"
        );

        assert!(restore_vessel(&conn, v).unwrap());
        delete_cleaning(&conn, id).unwrap();
        assert_eq!(code(delete_cleaning(&conn, id)), "db.no_such_cleaning");
        assert_eq!(
            code(restore_vessel(&conn, 99).map(|_| ())),
            "db.no_such_vessel"
        );
    }

    #[test]
    fn a_vessel_renames_and_refuses_a_taken_name() {
        let conn = db();
        let v = vessel(&conn);
        create_vessel(&conn, "Brite", VesselKind::Brite, None, None).unwrap();
        update_vessel(&conn, v, " FV-A ", VesselKind::Carboy, None, Some("glass")).unwrap();
        let row = get_vessel(&conn, v).unwrap();
        assert_eq!((row.name.as_str(), row.kind), ("FV-A", VesselKind::Carboy));
        assert_eq!(
            update_vessel(&conn, v, "Brite", VesselKind::Carboy, None, None)
                .unwrap_err()
                .code(),
            "db.duplicate"
        );
    }
}
