//! Phase 2e's `_full.csv` side. The transaction moved to [`crate::journal::store`] in
//! Va; this keeps the Phase 2 names so no caller changed.

use crate::{
    Result,
    db::Db,
    fermentrack::{SyncState, SyncStep, sync::FullCsv},
    units::Timestamp,
};

pub(crate) use crate::journal::store::upsert_sync_state_in;
pub use crate::journal::store::{CommitReport, load_sync_state, upsert_sync_state};

/// [`journal::commit`](crate::journal::commit) for a `_full.csv`.
pub fn commit(
    db: &Db,
    state: &mut SyncState,
    step: &SyncStep,
    at: Timestamp,
) -> Result<CommitReport> {
    crate::journal::commit::<FullCsv>(db, state, step, at)
}
