//! Phase 2. Every Fermentrack HTTP call and CSV parse lives here, so that replacing
//! Fermentrack with our own controller is a new module beside these rather than a
//! rewrite. Fermentrack's `Beer` naming is quarantined to this module.
//!
//! The trait the Option 2 door needs is [`crate::sync::ReadingSource`], and it lives in
//! `sync/` rather than here: it belongs at the level of *readings*, not bytes, because
//! not every source is a byte range — manual entry is not. It
//! arrived in 2g, when the scheduler and the manual Resync command became two callers —
//! writing it in 2c, around the only implementation that existed, would have cemented
//! the byte offset into the scheduler, which is the coupling the door prevents.
//! [`source::FermentrackSource`] is the one impl.
//!
//! The format-agnostic half — the `Range` client, record splitting, offset arithmetic
//! and the commit — moved to [`crate::journal`] in Va so `brewd`'s journals can share
//! it. What stays here is the `_full.csv` dialect ([`sync::FullCsv`]), discovery, and
//! re-exports that keep every Phase 2 path valid.

pub mod client;
pub mod csv_log;
pub mod discovery;
pub mod source;
mod state;
pub mod store;
pub mod sync;

pub use client::{Client, ClientSettings, FetchOutcome, RetryPolicy, log_url};
pub use csv_log::{
    HeaderInfo, LogRow, ParseReport, ParsedChunk, inspect_header, parse_full_log,
    parse_with_header, split_complete_lines, split_first_record,
};
pub use discovery::{
    LogProbe, SourceConfig, SourceSummary, VALIDATION_PREFIX_BYTES, add_source, check_fermentrack,
    probe_log, source_summaries,
};
pub use source::{FermentrackSource, build_source};
pub use state::ControllerState;
pub use store::{CommitReport, commit, load_sync_state, upsert_sync_state};
pub use sync::{
    Advance, FullCsv, MAX_PARTIAL_RECORD_BYTES, Restart, RestartReason, SyncFault, SyncState,
    SyncStep, ingest,
};

pub(crate) use crate::journal::{preview, preview_bytes};
