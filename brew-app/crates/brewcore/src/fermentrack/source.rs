//! Phase 2g. The only implementation of [`ReadingSource`], and the seam for Option 2.
//!
//! Everything here is 2c–2e wired end to end; nothing new is decided. `poll` is the
//! nine-step sync loop from docs/BUILDOUT.md, and it is short because each step already
//! has a home:
//!
//! ```text
//! load_sync_state → log_url → Client::fetch → sync::ingest → store::commit
//! ```
//!
//! Two choices are worth stating, because they are the ones a reader would otherwise
//! assume the other way round.
//!
//! **The `SyncState` is re-read from the database every poll, never cached.** 2f's
//! `upsert_sync_state` can repoint a live source at a different log, and a cached
//! `remote_path` would go on fetching the old file with nothing in the UI to explain
//! it. It is one indexed single-row query per minute. 2e's rule that the in-memory
//! state is replaced only after `commit()` returns is unaffected — it governs the copy
//! *within* a poll, which is exactly the copy `store::commit` still owns.
//!
//! **Every database step runs on `spawn_blocking`.** `rusqlite` is synchronous, and
//! "app closed for a week" commits a week of rows in one transaction. Doing that on a
//! tokio worker thread is the "sync never blocks a UI query" invariant broken from the
//! thread-pool side rather than the SQLite side.

use crate::{
    Error, Result,
    db::Db,
    fermentrack::{
        Client, Restart, RestartReason, SyncState, SyncStep, discovery::SourceConfig, ingest,
        log_url, store,
    },
    sync::{PollReport, ReadingSource},
    units::Timestamp,
};

/// One Fermentrack CSV log, polled.
pub struct FermentrackSource {
    db: Db,
    client: Client,
    source_id: i64,
    /// From `source.config`. The path is *not* held here — see the module docs.
    base_url: String,
}

/// Build the source for a `source` row, reading its `base_url` back out of
/// `source.config`.
///
/// Fails when the row has no config, no `sync_state`, or a config that is not 2f's
/// [`SourceConfig`] — all three mean a half-written source, and a task that polled one
/// would report the same error once a minute forever.
pub fn build_source(db: &Db, client: &Client, source_id: i64) -> Result<FermentrackSource> {
    let config: Option<String> = db.read(|conn| {
        Ok(conn.query_row(
            "SELECT config FROM source WHERE id = ?",
            [source_id],
            |row| row.get(0),
        )?)
    })?;

    let config: SourceConfig = config
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .ok_or(Error::MissingSyncState { source_id })?;

    // Proves the `sync_state` row exists before a task is spawned against it, so the
    // failure is reported by whoever added the source rather than by a poll at 3 a.m.
    store::load_sync_state(db, source_id)?.ok_or(Error::MissingSyncState { source_id })?;

    Ok(FermentrackSource {
        db: db.clone(),
        client: client.clone(),
        source_id,
        base_url: config.base_url,
    })
}

impl FermentrackSource {
    /// The `sync_state` row as it stands right now.
    async fn state(&self) -> Result<SyncState> {
        let db = self.db.clone();
        let source_id = self.source_id;
        blocking(move || {
            store::load_sync_state(&db, source_id)?.ok_or(Error::MissingSyncState { source_id })
        })
        .await
    }

    /// Write one step and describe what it did.
    async fn commit(&self, mut state: SyncState, step: SyncStep) -> Result<PollReport> {
        let db = self.db.clone();
        let source_id = self.source_id;
        blocking(move || {
            let report = store::commit(&db, &mut state, &step, Timestamp::now())?;
            let newest = db.read(|conn| crate::db::tier1::newest_reading(conn, source_id))?;
            Ok(PollReport {
                rows_added: report.rows_inserted,
                rows_ignored: report.rows_ignored,
                // Straight off the committed state, not off the step: this is the text
                // the footer shows and the database holds, and two sources for one
                // string is how they come to disagree.
                fault: state.last_error.clone(),
                newest_reading: newest,
            })
        })
        .await
    }
}

#[async_trait::async_trait]
impl ReadingSource for FermentrackSource {
    fn source_id(&self) -> i64 {
        self.source_id
    }

    async fn poll(&mut self) -> Result<PollReport> {
        let state = self.state().await?;
        // Rebuilt every poll, from the path we just read. `remote_path` is stored
        // decoded; `log_url` is the one boundary that percent-encodes it.
        let url = log_url(&self.base_url, &state.remote_path)?;

        let outcome = self.client.fetch(&url, state.byte_offset).await?;
        let step = ingest(&state, outcome)?;
        self.commit(state, step).await
    }

    async fn rebuild(&mut self) -> Result<PollReport> {
        // A synthetic restart, committed through the ordinary path. That is what makes
        // Rebuild actually rebuild: `store::commit` answers a `Restart` by calling
        // `delete_readings_for_source`, and without the delete the `ON CONFLICT DO
        // NOTHING` on the way back in would preserve precisely the bad rows Rebuild
        // exists to remove.
        let state = self.state().await?;
        tracing::info!(source_id = self.source_id, "rebuilding from the beginning");
        self.commit(
            state,
            SyncStep::Restart(Restart {
                reason: RestartReason::Requested,
            }),
        )
        .await?;

        // The reset itself stored nothing, so the report the scheduler sees must come
        // from the refetch that follows it — otherwise a rebuild always reports zero
        // rows and the status footer says it did nothing.
        self.poll().await
    }
}

/// `spawn_blocking`, with a panic turned into an error.
///
/// A panicking commit must back the source off like any other failure. Left as a
/// `JoinError` it would be unwrapped into a panic inside the scheduler's task, and the
/// source would stop being polled with nothing said about it.
async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(err) => Err(Error::BackgroundTask {
            detail: err.to_string(),
        }),
    }
}
