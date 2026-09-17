//! Phase 2. Every Fermentrack HTTP call and CSV parse lives here, behind a trait,
//! so that replacing Fermentrack with our own controller is a new impl rather than
//! a rewrite. Fermentrack's `Beer` naming is quarantined to this module.
