//! Phase 2e. One [`SyncStep`] becomes one transaction.
//!
//! 2d decides what a fetch means and touches nothing; this module is the only place
//! that writes what it decided. Everything below exists to defend a single sentence:
//! **the offset can never claim bytes whose rows did not land.**
//!
//! Three rules follow from it, and each is the difference between a gap you can see
//! and one you cannot:
//!
//! 1. **Readings, offset, header, `last_ts` and `last_error` share one transaction.**
//!    A crash mid-sync leaves the file at the state before the poll or the state
//!    after it, never at an offset past rows that were rolled back — which would be a
//!    permanent hole no later poll would ever refetch.
//! 2. **The in-memory [`SyncState`] is only updated after `commit()` returns.**
//!    [`SyncStep::apply_to`] folds onto a clone, so a rolled-back transaction leaves
//!    the caller's offset exactly where it was and the next poll retries the same
//!    bytes. Mutating in place first would move the offset past rows that no longer
//!    exist, which is rule 1 defeated from the other side.
//! 3. **A restart deletes the source's readings before refetching.** Otherwise
//!    `ON CONFLICT DO NOTHING` keeps whatever is already there, and a rebuild — the
//!    escape hatch for a parser bug — re-reads bytes it then ignores.
//!
//! The reading primitives themselves are in [`db::tier1`](crate::db::tier1) and know
//! nothing about Fermentrack. This module is the Fermentrack-shaped half: byte
//! offsets, header lines, log rows.

use rusqlite::{OptionalExtension, params};

use crate::{
    Result,
    db::{Db, tier1},
    fermentrack::{LogRow, SyncState, SyncStep},
    units::Timestamp,
};

/// What one commit did. Phase 2g's `sync://status` event reports `rows_inserted`; the
/// rest is what makes a wrong number diagnosable without re-reading the log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct CommitReport {
    pub rows_inserted: usize,
    /// Already present, so skipped. Large and steady means the server is ignoring
    /// `Range` and re-sending the whole file every poll — correct, but worth seeing.
    pub rows_ignored: usize,
    pub streams_created: usize,
    /// Non-zero only on a restart.
    pub readings_deleted: usize,
}

/// Read a source's sync state back. `None` when the source has never been set up.
pub fn load_sync_state(db: &Db, source_id: i64) -> Result<Option<SyncState>> {
    db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT source_id, remote_path, byte_offset, header, last_ts, last_error, \
                 synced_at FROM sync_state WHERE source_id = ?",
                [source_id],
                |row| {
                    Ok(SyncState {
                        source_id: row.get(0)?,
                        remote_path: row.get(1)?,
                        // SQLite's INTEGER is signed and `rusqlite` maps no `u64`.
                        // A byte offset into a file never approaches `i64::MAX`, and
                        // the CHECK in `0001_tier1.sql` already refuses a negative
                        // one, so the cast is total in both directions.
                        byte_offset: row.get::<_, i64>(2)? as u64,
                        header: row.get(3)?,
                        last_ts: row.get(4)?,
                        last_error: row.get(5)?,
                        synced_at: row.get(6)?,
                    })
                },
            )
            .optional()?)
    })
}

/// Create the `sync_state` row for a source, or repoint an existing one at a new
/// `remote_path`.
///
/// A changed path resets the offset: Fermentrack puts the beer name in the filename,
/// so a different path is a different file, and byte 4 000 of one says nothing about
/// byte 4 000 of the other. `remote_path` is stored **decoded** — `log_url` does the
/// percent-encoding at the one boundary that builds a URL.
pub fn upsert_sync_state(db: &Db, source_id: i64, remote_path: &str) -> Result<()> {
    db.write(|conn| {
        conn.execute(
            "INSERT INTO sync_state \
               (source_id, remote_path, byte_offset, header, last_ts, last_error, synced_at) \
             VALUES (?, ?, 0, NULL, NULL, NULL, ?) \
             ON CONFLICT(source_id) DO UPDATE SET \
               remote_path = excluded.remote_path, \
               byte_offset = CASE WHEN sync_state.remote_path = excluded.remote_path \
                                  THEN sync_state.byte_offset ELSE 0 END, \
               header      = CASE WHEN sync_state.remote_path = excluded.remote_path \
                                  THEN sync_state.header ELSE NULL END",
            params![source_id, remote_path, Timestamp::now()],
        )?;
        Ok(())
    })
}

/// Write one step, then move the caller's state onto it.
///
/// `at` is the clock as an argument so a test can compare whole [`SyncState`]s by
/// value — the same reason [`SyncStep::apply_to`] takes it.
///
/// [`SyncStep::Nothing`] still commits: `synced_at` is what the footer's "last
/// checked" reads, and a poll that found nothing is still a poll that happened.
pub fn commit(
    db: &Db,
    state: &mut SyncState,
    step: &SyncStep,
    at: Timestamp,
) -> Result<CommitReport> {
    // Folded before the transaction, discarded if it rolls back. See rule 2.
    let mut next = state.clone();
    step.apply_to(&mut next, at);

    let report = db.write(|conn| {
        let tx = conn.transaction()?;
        let mut report = CommitReport::default();

        match step {
            SyncStep::Nothing => {}
            SyncStep::Restart(_) => {
                report.readings_deleted = tier1::delete_readings_for_source(&tx, state.source_id)?;
            }
            SyncStep::Advance(advance) => {
                let (rows, streams_created) = fan_out(&tx, state.source_id, &advance.rows)?;
                report.streams_created = streams_created;
                report.rows_inserted = tier1::insert_readings(&tx, &rows)?;
                report.rows_ignored = rows.len() - report.rows_inserted;
            }
        }

        let updated = tx.execute(
            "UPDATE sync_state SET byte_offset = ?, header = ?, last_ts = ?, last_error = ?, \
             synced_at = ? WHERE source_id = ?",
            params![
                next.byte_offset as i64,
                next.header,
                next.last_ts,
                next.last_error,
                next.synced_at,
                next.source_id,
            ],
        )?;
        // An `UPDATE` matching nothing is `Ok(0)`, not an error. Without this check a
        // source whose `sync_state` row went missing would commit readings under an
        // offset that is not stored anywhere — every poll would then refetch from 0,
        // and rule 1 would be defeated by the one statement written to uphold it.
        if updated == 0 {
            return Err(crate::Error::MissingSyncState {
                source_id: next.source_id,
            });
        }

        tx.commit()?;
        Ok(report)
    })?;

    // Only now. An offset that moved while the transaction rolled back is exactly the
    // corruption the single boundary exists to prevent.
    *state = next;

    if let Some(error) = &state.last_error {
        tracing::warn!(source_id = state.source_id, %error, "sync fault recorded");
    }
    tracing::debug!(
        source_id = state.source_id,
        byte_offset = state.byte_offset,
        rows_inserted = report.rows_inserted,
        rows_ignored = report.rows_ignored,
        "committed a sync step"
    );
    Ok(report)
}

/// One CSV row to up to eight `(stream_id, ts, value)` triples, creating any stream
/// this source has not carried before.
///
/// The stream map is loaded once for the whole chunk, not once per row, and a stream
/// is created only for a metric that actually carried a value — [`LogRow::readings`]
/// already skips the absent ones, so a fermenter with no room probe never grows an
/// empty series for the UI to draw.
fn fan_out(
    tx: &rusqlite::Transaction<'_>,
    source_id: i64,
    rows: &[LogRow],
) -> Result<(Vec<tier1::NewReading>, usize)> {
    let mut streams = tier1::stream_ids_for(tx, source_id)?;
    let known = streams.len();
    let mut out = Vec::with_capacity(rows.len() * 4);

    for row in rows {
        for (column, metric, value) in row.readings() {
            let stream_id = match streams.get(&metric) {
                Some(id) => *id,
                None => {
                    let id = tier1::create_stream(tx, source_id, column, metric)?;
                    streams.insert(metric, id);
                    id
                }
            };
            out.push((stream_id, row.ts, value));
        }
    }

    Ok((out, streams.len() - known))
}
