//! Brewery logic: database, Fermentrack ingest, and calculations.
//!
//! This crate has no Tauri dependency and never will — see `tests/no_tauri.rs`.
//! Everything interesting lives here so a plain `cargo test` can drive it without
//! a window, and so the v2 controller becomes a new module rather than a new
//! project (docs/BUILDOUT.md, "Keeping the Option 2 door open").

pub mod calc;
pub mod config;
pub mod db;
pub mod error;
pub mod fermentrack;
pub mod units;

pub use error::{Error, Result};
