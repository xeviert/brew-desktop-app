//! Phase 2e. The Tier 1 write primitives: sources, streams, readings.
//!
//! **Nothing here imports `fermentrack`.** That is the Option 2 door, stated the same
//! way `db::metric` states it: the vocabulary is ours, not Fermentrack's. Phase 3e's
//! manual measurements and v2's MQTT `ReadingSource` write readings through these same
//! functions, and neither goes anywhere near a CSV. What *is* Fermentrack-shaped — the
//! byte offset, the header, the `SyncState` row — lives in
//! [`fermentrack::store`](crate::fermentrack::store), which calls into here.
//!
//! Every function takes a borrowed connection or transaction and never opens its own.
//! The transaction boundary belongs to the caller, because the whole point of 2e is
//! that readings and the offset that claims them commit together or not at all.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::enums::db_enum;
use crate::{Error, Result, db::Metric, units::Timestamp};

/// One row of `reading`, ready to insert. A tuple rather than a struct: it is only
/// ever built and immediately consumed, and a `Vec` of it is the batch.
pub type NewReading = (i64, Timestamp, f64);

/// One row of `annotation`, ready to insert. A tuple for the same reason
/// [`NewReading`] is one.
pub type NewAnnotation<'a> = (Timestamp, AnnotationChannel, &'a str);

db_enum!(
    /// Which side of the chamber an annotation is about. Not a `CHECK` in the schema —
    /// see `0005_timeline.sql`.
    ///
    /// Our vocabulary, not Fermentrack's: `beer_ann` and `fridge_ann` are column names
    /// that stay inside [`csv_log`](crate::fermentrack::csv_log), and the schema
    /// linter would reject either of them here anyway.
    AnnotationChannel,
    UnknownAnnotationChannel,
    (Vessel, "vessel"),
    (Chamber, "chamber"),
);

/// The `source.kind` vocabulary, mirroring the `CHECK` in `0001_tier1.sql`.
///
/// An enum rather than a `&str` so that a typo is a compile error instead of a
/// `SQLITE_CONSTRAINT` at the first sync of the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    FermentrackBeerLog,
    FermentrackGravityLog,
    /// Phase 3e. Manual measurements go to their own source so that they are visibly
    /// not Fermentrack's.
    Manual,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FermentrackBeerLog => "fermentrack_beer_log",
            Self::FermentrackGravityLog => "fermentrack_gravity_log",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::FermentrackBeerLog,
            Self::FermentrackGravityLog,
            Self::Manual,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == s)
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The stored string, not a derived variant name — the UI and the database must
/// agree, and `as_str` is the one place that decides what the string is.
impl serde::Serialize for SourceKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl rusqlite::ToSql for SourceKind {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl rusqlite::types::FromSql for SourceKind {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::parse(text).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(Box::new(crate::Error::UnknownSourceKind(
                text.to_owned(),
            )))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceRow {
    pub id: i64,
    pub kind: SourceKind,
    pub name: String,
    /// Opaque JSON. 2f decides what goes in it.
    pub config: Option<String>,
}

pub fn create_source(
    conn: &Connection,
    kind: SourceKind,
    name: &str,
    config: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO source (kind, name, config) VALUES (?, ?, ?)",
        params![kind, name, config],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_sources(conn: &Connection) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare("SELECT id, kind, name, config FROM source ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok(SourceRow {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            config: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every stream this source already has, keyed by metric.
///
/// One indexed query over at most eight rows (`stream_source_idx`). 2e loads it once
/// per commit rather than caching it across polls on purpose: a map that survives a
/// Rebuild holds ids for streams that may since have been rewritten, and a stale
/// `stream_id` writes readings into the wrong series with nothing to detect it
/// afterwards. At a 60 s cadence the query is free.
pub fn stream_ids_for(conn: &Connection, source_id: i64) -> Result<HashMap<Metric, i64>> {
    let mut stmt = conn.prepare("SELECT metric, id FROM stream WHERE source_id = ?")?;
    let rows = stmt.query_map([source_id], |row| {
        Ok((row.get::<_, Metric>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Get or create the stream for `(source_id, external_id, metric)`.
///
/// `DO NOTHING` then read the id back, rather than `INSERT` and catch the error: the
/// row may already exist from an earlier poll, and a unique violation is not an
/// exceptional condition here.
pub fn create_stream(
    tx: &Transaction<'_>,
    source_id: i64,
    external_id: &str,
    metric: Metric,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO stream (source_id, external_id, metric) VALUES (?, ?, ?) \
         ON CONFLICT(source_id, external_id, metric) DO NOTHING",
        params![source_id, external_id, metric],
    )?;
    Ok(tx.query_row(
        "SELECT id FROM stream WHERE source_id = ? AND external_id = ? AND metric = ?",
        params![source_id, external_id, metric],
        |row| row.get(0),
    )?)
}

/// Insert readings, ignoring ones already present. Returns how many were *new*.
///
/// One prepared statement reused across the loop — a week of backfill is ~20 000 rows
/// fanning out to ~160 000 inserts, and re-preparing the statement for each would
/// dominate the cost. The caller's transaction is what makes the whole batch atomic.
///
/// `ON CONFLICT(stream_id, ts) DO NOTHING` names its conflict target deliberately.
/// `INSERT OR IGNORE` would swallow a foreign-key failure too, which is how a bad
/// `stream_id` becomes silent data loss instead of an error.
pub fn insert_readings(tx: &Transaction<'_>, rows: &[NewReading]) -> Result<usize> {
    let mut stmt = tx.prepare(
        "INSERT INTO reading (stream_id, ts, value) VALUES (?, ?, ?) \
         ON CONFLICT(stream_id, ts) DO NOTHING",
    )?;
    let mut inserted = 0;
    for (stream_id, ts, value) in rows {
        inserted += stmt.execute(params![stream_id, ts, value])?;
    }
    Ok(inserted)
}

/// Insert annotations, ignoring ones already present. Returns how many were *new*.
///
/// The same shape as [`insert_readings`] and for the same reasons: one prepared
/// statement across the batch, and a named conflict target rather than
/// `INSERT OR IGNORE`, so a bad `source_id` is an error instead of silent loss.
///
/// Two annotations on one `(source, ts, channel)` cannot happen — the CSV has one
/// cell per channel per row — so the conflict only ever fires on re-ingested bytes.
pub fn insert_annotations(
    tx: &Transaction<'_>,
    source_id: i64,
    rows: &[NewAnnotation<'_>],
) -> Result<usize> {
    let mut stmt = tx.prepare(
        "INSERT INTO annotation (source_id, ts, channel, text) VALUES (?, ?, ?, ?) \
         ON CONFLICT(source_id, ts, channel) DO NOTHING",
    )?;
    let mut inserted = 0;
    for (ts, channel, text) in rows {
        inserted += stmt.execute(params![source_id, ts, channel, text])?;
    }
    Ok(inserted)
}

/// Drop every annotation belonging to a source. The other half of Rebuild.
///
/// Annotations are Tier 1, derived from the same bytes as the readings. A rebuild that
/// re-derived one and not the other would leave a timeline whose suggestions came from
/// a file the readings no longer agree with.
pub fn delete_annotations_for_source(tx: &Transaction<'_>, source_id: i64) -> Result<usize> {
    Ok(tx.execute("DELETE FROM annotation WHERE source_id = ?", [source_id])?)
}

/// Drop every reading belonging to a source. The "Rebuild from scratch" primitive;
/// Phase 2g wires the command.
///
/// Without this, a rebuild only re-reads bytes that `DO NOTHING` then ignores, so the
/// wrong values a parser bug wrote would survive the one operation meant to fix them.
/// The `stream` rows stay: their ids are referenced by Phase 3's `batch_stream`, and a
/// rebuild is not a reason to unbind a batch from its curve.
pub fn delete_readings_for_source(tx: &Transaction<'_>, source_id: i64) -> Result<usize> {
    Ok(tx.execute(
        "DELETE FROM reading WHERE stream_id IN (SELECT id FROM stream WHERE source_id = ?)",
        [source_id],
    )?)
}

/// The newest reading across a source's streams. The status footer's "age of the
/// newest reading"; `sync_state.last_ts` is the same number seen from the log's side.
pub fn newest_reading(conn: &Connection, source_id: i64) -> Result<Option<Timestamp>> {
    Ok(conn
        .query_row(
            "SELECT MAX(ts) FROM reading WHERE stream_id IN \
             (SELECT id FROM stream WHERE source_id = ?)",
            [source_id],
            |row| row.get::<_, Option<Timestamp>>(0),
        )
        .optional()?
        .flatten())
}

/// How many readings a source owns.
///
/// Phase 2g's Rebuild confirmation names this number: "delete 4,312 readings" is a
/// decision the user can make, and "delete this source's readings" is not.
pub fn count_readings(conn: &Connection, source_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM reading WHERE stream_id IN \
         (SELECT id FROM stream WHERE source_id = ?)",
        [source_id],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn source(conn: &Connection, name: &str) -> i64 {
        create_source(conn, SourceKind::FermentrackBeerLog, name, None).unwrap()
    }

    #[test]
    fn the_source_kinds_match_the_schema_check() {
        let conn = db();
        for kind in [
            SourceKind::FermentrackBeerLog,
            SourceKind::FermentrackGravityLog,
            SourceKind::Manual,
        ] {
            create_source(&conn, kind, "s", None)
                .unwrap_or_else(|e| panic!("{kind} was refused: {e}"));
        }
        assert_eq!(list_sources(&conn).unwrap().len(), 3);
        assert_eq!(SourceKind::parse("beer"), None);
    }

    #[test]
    fn creating_the_same_stream_twice_returns_the_same_id() {
        let mut conn = db();
        let source_id = source(&conn, "s");
        let tx = conn.transaction().unwrap();

        let first = create_stream(&tx, source_id, "beer_temp", Metric::TempC).unwrap();
        let again = create_stream(&tx, source_id, "beer_temp", Metric::TempC).unwrap();
        assert_eq!(first, again);

        // A different metric on the same source is a different stream.
        let other = create_stream(&tx, source_id, "room_temp", Metric::RoomTempC).unwrap();
        assert_ne!(first, other);

        let found = stream_ids_for(&tx, source_id).unwrap();
        assert_eq!(found.get(&Metric::TempC), Some(&first));
        assert_eq!(found.get(&Metric::RoomTempC), Some(&other));
        assert_eq!(found.len(), 2);
    }

    /// The dedupe that makes a Range-ignoring proxy harmless: the same bytes arrive
    /// twice and the second pass inserts nothing.
    #[test]
    fn a_reading_already_present_is_ignored_and_counted_as_such() {
        let mut conn = db();
        let source_id = source(&conn, "s");
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, "beer_temp", Metric::TempC).unwrap();

        let rows = [
            (stream_id, Timestamp::from_secs(10), 18.0),
            (stream_id, Timestamp::from_secs(20), 18.5),
        ];
        assert_eq!(insert_readings(&tx, &rows).unwrap(), 2);
        assert_eq!(insert_readings(&tx, &rows).unwrap(), 0);

        // And the first write wins, so a replayed chunk cannot rewrite history.
        let overlapping = [(stream_id, Timestamp::from_secs(10), 99.0)];
        assert_eq!(insert_readings(&tx, &overlapping).unwrap(), 0);
        let kept: f64 = tx
            .query_row(
                "SELECT value FROM reading WHERE stream_id = ? AND ts = 10",
                [stream_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, 18.0);
    }

    #[test]
    fn a_reading_on_a_stream_that_does_not_exist_is_an_error_not_a_silent_drop() {
        let mut conn = db();
        let tx = conn.transaction().unwrap();
        let rows = [(404, Timestamp::from_secs(10), 18.0)];
        assert!(insert_readings(&tx, &rows).is_err());
    }

    #[test]
    fn deleting_a_sources_readings_leaves_another_sources_alone() {
        let mut conn = db();
        let mine = source(&conn, "mine");
        let theirs = source(&conn, "theirs");
        let tx = conn.transaction().unwrap();

        let mut streams = Vec::new();
        for source_id in [mine, theirs] {
            let stream_id = create_stream(&tx, source_id, "beer_temp", Metric::TempC).unwrap();
            insert_readings(&tx, &[(stream_id, Timestamp::from_secs(10), 18.0)]).unwrap();
            streams.push(stream_id);
        }

        assert_eq!(delete_readings_for_source(&tx, mine).unwrap(), 1);
        assert_eq!(newest_reading(&tx, mine).unwrap(), None);
        assert_eq!(
            newest_reading(&tx, theirs).unwrap(),
            Some(Timestamp::from_secs(10))
        );
        // The stream survives: Phase 3's `batch_stream` points at its id.
        assert!(
            stream_ids_for(&tx, mine)
                .unwrap()
                .contains_key(&Metric::TempC)
        );
    }
}
