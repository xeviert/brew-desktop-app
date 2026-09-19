//! Phase 2. Every Fermentrack HTTP call and CSV parse lives here, behind a trait,
//! so that replacing Fermentrack with our own controller is a new impl rather than
//! a rewrite. Fermentrack's `Beer` naming is quarantined to this module.

pub mod csv_log;
mod state;

pub use csv_log::{LogRow, ParseReport, ParsedChunk, parse_full_log, parse_with_header};
pub use state::ControllerState;
