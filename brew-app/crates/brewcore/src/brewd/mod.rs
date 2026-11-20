//! Phase Vd. `brewd`'s journals, read by the desktop app.
//!
//! A new module beside `fermentrack/`, as docs/BUILDOUT.md's Option 2 door intended:
//! the byte-range sync is [`crate::journal`]'s, and all this adds is a
//! [`Dialect`](crate::journal::Dialect), a [`ReadingSource`](crate::sync::ReadingSource)
//! and discovery. Nothing here imports `fermentrack`.

pub mod alerts;
pub mod dialect;
pub mod discovery;
pub mod source;

pub use dialect::{JournalCsv, JournalHeader, JournalRow, MappedColumn, inspect_header};
pub use discovery::{
    JOURNALS_PATH, JournalProbe, JournalSourceConfig, add_source, list_journals, probe_journal,
};
pub use source::{BrewdSource, build_source};
