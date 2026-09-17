//! Phase 1c. The single owner of every unit conversion.
//!
//! Storage is canonical and conversion happens at the edges: °C, SG, litres,
//! grams, UTC epoch seconds, integer cents. Display preference is config, never
//! storage.
