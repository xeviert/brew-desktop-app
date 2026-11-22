//! Phase Vd. One `brewd` journal, polled.
//!
//! The same loop as `fermentrack::source`, with three differences that each follow
//! from `brewd` being ours:
//!
//! - **The base URL and the iSpindle gravity unit come from the live config every
//!   poll.** Neither is per source, and a unit recorded after the fact must reach a
//!   source that has been refusing to parse for want of it.
//! - **A restart keeps the readings.** A journal row is a fact stamped at receipt; see
//!   [`JournalCsv::deletes_on_restart`](super::dialect::JournalCsv).
//! - **Rotation is drained, not lost.** When its header changes `brewd` renames
//!   `<dev>.csv` to `<dev>.<epoch>.csv` and starts a new file. A resume then finds the
//!   live file shorter than its offset, or finds itself mid-record. Before restarting
//!   at 0 the source looks for the rotated file that holds the rest of what it was
//!   reading, and reads it from the stored offset to the end — otherwise every row
//!   written between the last poll and the rotation would be a gap. Rebuild reads every
//!   rotated file whole, oldest first, for the same reason.

use crate::{
    Error, Result,
    brewd::{
        dialect::{self, JournalRow},
        discovery,
    },
    config::{ConfigStore, IspindelUnit},
    db::Db,
    journal::{
        Client, CommitReport, JournalEntry, Restart, RestartReason, SyncState, SyncStep, log_url,
        rotated_stem, split_first_record, store,
    },
    sync::{PollReport, ReadingSource, blocking},
    units::Timestamp,
};

pub struct BrewdSource {
    db: Db,
    client: Client,
    config: ConfigStore,
    source_id: i64,
}

/// Build the source for a `source` row. Fails when it has no `sync_state`, for the
/// reason `fermentrack::build_source` gives.
pub fn build_source(
    db: &Db,
    client: &Client,
    config: &ConfigStore,
    source_id: i64,
) -> Result<BrewdSource> {
    store::load_sync_state(db, source_id)?.ok_or(Error::MissingSyncState { source_id })?;
    Ok(BrewdSource {
        db: db.clone(),
        client: client.clone(),
        config: config.clone(),
        source_id,
    })
}

/// What a poll needs from the live config.
struct Live {
    base_url: String,
    unit: Option<IspindelUnit>,
}

impl BrewdSource {
    fn live(&self) -> Result<Live> {
        let config = self.config.get();
        Ok(Live {
            base_url: discovery::base_url(&config.brewd.base_url)?.to_owned(),
            unit: config.gravity.ispindel_unit,
        })
    }

    async fn state(&self) -> Result<SyncState> {
        let db = self.db.clone();
        let source_id = self.source_id;
        blocking(move || {
            store::load_sync_state(&db, source_id)?.ok_or(Error::MissingSyncState { source_id })
        })
        .await
    }

    /// Commit one step onto `state`, moving it only once the transaction has.
    async fn commit(
        &self,
        state: &mut SyncState,
        step: SyncStep<JournalRow>,
    ) -> Result<CommitReport> {
        let db = self.db.clone();
        let mut next = state.clone();
        let report;
        (*state, report) = blocking(move || {
            let report = dialect::commit(&db, &mut next, &step, Timestamp::now())?;
            Ok((next, report))
        })
        .await?;
        Ok(report)
    }

    /// `rows_added` is counted rather than summed, because a drained rotation commits
    /// more than once in a poll.
    async fn report(
        &self,
        state: &SyncState,
        before: i64,
        rows_ignored: usize,
    ) -> Result<PollReport> {
        let db = self.db.clone();
        let source_id = self.source_id;
        let fault = state.last_error.clone();
        blocking(move || {
            db.read(|conn| {
                let after = crate::db::tier1::count_readings(conn, source_id)?;
                Ok(PollReport {
                    rows_added: usize::try_from(after - before).unwrap_or(0),
                    rows_ignored,
                    fault,
                    newest_reading: crate::db::tier1::newest_reading(conn, source_id)?,
                })
            })
        })
        .await
    }

    async fn reading_count(&self) -> Result<i64> {
        let db = self.db.clone();
        let source_id = self.source_id;
        blocking(move || db.read(|conn| crate::db::tier1::count_readings(conn, source_id))).await
    }

    /// Read a rotated journal from `state.byte_offset` to its end and commit it, parsed
    /// against `state.header` — or against its own header when the offset is 0.
    ///
    /// The `sync_state` row is written as if this were the live journal. The caller
    /// commits a [`RestartReason::Rotated`] straight after, which is what puts it back.
    async fn read_through(
        &self,
        live: &Live,
        entry: &JournalEntry,
        state: &mut SyncState,
    ) -> Result<()> {
        let remote_path = discovery::remote_path(&entry.name)?;
        let url = log_url(&live.base_url, &remote_path)?;
        let outcome = self.client.fetch(&url, state.byte_offset).await?;
        // Only the name in error messages changes; the offset and header are the ones
        // this state was reading with.
        let named = SyncState {
            remote_path,
            ..state.clone()
        };
        let step = dialect::ingest(live.unit, &named, outcome)?;
        if matches!(step, SyncStep::Advance(_)) {
            self.commit(state, step).await?;
        }
        Ok(())
    }

    /// Rotated journals of this source's device, as `(epoch, entry)`.
    async fn rotated(&self, live: &Live, state: &SyncState) -> Result<Vec<(i64, JournalEntry)>> {
        let Some(device) = state
            .remote_path
            .strip_prefix(discovery::JOURNALS_PATH)
            .and_then(|name| name.strip_suffix(".csv"))
        else {
            return Ok(Vec::new());
        };
        let device = device.to_owned();
        Ok(discovery::list_journals(&self.client, &live.base_url)
            .await?
            .into_iter()
            .filter_map(|entry| match rotated_stem(&entry.name) {
                Some((stem, epoch)) if stem == device => Some((epoch, entry)),
                _ => None,
            })
            .collect())
    }

    /// Read the rest of the journal we were resuming out of the file `brewd` rotated it
    /// to. Returns that file's name, or `None` when there is no such file — a journal
    /// deleted or hand-edited rather than rotated.
    ///
    /// A candidate must be at least as long as our offset and begin with our stored
    /// header; the newest such file wins. Headers are compared rather than trusting the
    /// epoch alone because a device can rotate twice between polls.
    async fn drain_rotated(&self, live: &Live, state: &mut SyncState) -> Result<Option<String>> {
        let Some(header) = state.header.clone() else {
            return Ok(None);
        };
        let mut candidates = self.rotated(live, state).await?;
        candidates.retain(|(_, entry)| entry.bytes >= state.byte_offset);
        candidates.sort_by_key(|(epoch, _)| std::cmp::Reverse(*epoch));

        for (_, entry) in candidates {
            let url = log_url(&live.base_url, &discovery::remote_path(&entry.name)?)?;
            let prefix = self
                .client
                .fetch_prefix(&url, discovery::PROBE_PREFIX_BYTES)
                .await?;
            let Some((line, _)) = split_first_record(&prefix) else {
                continue;
            };
            if line.strip_suffix(b"\n").unwrap_or(line) != header.as_bytes() {
                continue;
            }
            if entry.bytes > state.byte_offset {
                self.read_through(live, &entry, state).await?;
            }
            tracing::info!(
                source_id = self.source_id,
                rotated_to = %entry.name,
                "drained a rotated journal"
            );
            return Ok(Some(entry.name));
        }
        Ok(None)
    }

    async fn rotated_restart(&self, state: &mut SyncState, rotated_to: String) -> Result<()> {
        self.commit(
            state,
            SyncStep::Restart(Restart {
                reason: RestartReason::Rotated { rotated_to },
            }),
        )
        .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ReadingSource for BrewdSource {
    fn source_id(&self) -> i64 {
        self.source_id
    }

    async fn poll(&mut self) -> Result<PollReport> {
        let live = self.live()?;
        let before = self.reading_count().await?;

        let mut state = self.state().await?;
        let url = log_url(&live.base_url, &state.remote_path)?;
        let outcome = self.client.fetch(&url, state.byte_offset).await?;
        let step = dialect::ingest(live.unit, &state, outcome)?;

        let mut rows_ignored = 0;
        if let SyncStep::Restart(Restart {
            reason: RestartReason::Truncated { .. } | RestartReason::NotOnARecordBoundary,
        }) = &step
            && state.byte_offset > 0
            && let Some(rotated_to) = self.drain_rotated(&live, &mut state).await?
        {
            self.rotated_restart(&mut state, rotated_to).await?;
        } else {
            rows_ignored = self.commit(&mut state, step).await?.rows_ignored;
        }
        self.report(&state, before, rows_ignored).await
    }

    /// Delete, then re-derive from every file `brewd` still has for this device: each
    /// rotated journal whole, oldest first, then the live one.
    async fn rebuild(&mut self) -> Result<PollReport> {
        let live = self.live()?;
        let mut state = self.state().await?;
        tracing::info!(source_id = self.source_id, "rebuilding from the beginning");
        self.commit(
            &mut state,
            SyncStep::Restart(Restart {
                reason: RestartReason::Requested,
            }),
        )
        .await?;

        let mut rotated = self.rotated(&live, &state).await?;
        rotated.sort_by_key(|(epoch, _)| *epoch);
        for (_, entry) in rotated {
            self.read_through(&live, &entry, &mut state).await?;
            self.rotated_restart(&mut state, entry.name).await?;
        }

        // Counted from zero: everything the rebuild stored is new.
        let mut report = self.poll().await?;
        report.rows_added = usize::try_from(self.reading_count().await?).unwrap_or(0);
        Ok(report)
    }
}
