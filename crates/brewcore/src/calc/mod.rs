//! Phase 3. Pure functions: gravity conversions, attenuation, ABV, refractometer
//! correction, pitch rates, nutrient math. No I/O, every formula unit-tested
//! against a worked example with its source cited.
//!
//! Everything here is a fit of measured data rather than a definition, which is the
//! line [`crate::units`] draws: a conversion that needs a literature reference is not
//! a unit conversion. Each function therefore carries its source, the range it was
//! fitted over, and an error band — and refuses inputs outside that range with
//! [`crate::Error::CalcOutOfRange`] rather than extrapolating quietly.
//!
//! Two exceptions to that framing, both deliberate. [`attenuation::apparent`] is
//! arithmetic on two readings and needs no source; it lives beside the Balling
//! formulas because nobody would look for it anywhere else. And [`cost`] is exact
//! integer money, here rather than in `db` so the folding is testable without a
//! database — `db::batch::batch_cost` supplies the rows.
//!
//! Phase 4b adds [`smooth`], a filter over a time series rather than a formula: there
//! is no literature value to check it against, so it is tested on generated curves.
//! Phase 4c's [`stability`] is the same kind of thing, and tested the same way; 4f's
//! [`health`] is two heuristics, and says so. Phase 5a's [`batch_metrics`] reads the
//! same series and 4c's history, and shares 4c's sliding least-squares fit.
//!
//! Nothing in this module reads the config or the database, and nothing defaults a
//! parameter that changes the answer: the wort correction factor, the nitrogen
//! demand factor and yeast viability are all arguments, each with a documented
//! constant a caller may choose to pass.

pub mod abv;
pub mod attenuation;
pub mod batch_metrics;
pub mod cost;
mod fit;
pub mod gravity;
pub mod health;
pub mod hydrometer;
pub mod pitch;
pub mod refractometer;
pub mod smooth;
pub mod stability;
pub mod sugar;
pub mod yan;

pub use gravity::{brix_to_sg, plato_to_sg, sg_to_brix, sg_to_plato};
pub use hydrometer::correct_for_temperature;
pub use refractometer::{
    WORT_CORRECTION_DEFAULT, final_gravity_from_brix, original_gravity_from_brix,
};
pub use sugar::{blend, dilute, og_from_sugar, sugar_for_og};

// `abv`, `attenuation`, `cost`, `pitch` and `yan` are not re-exported flat: each has
// a `simple`/`real`/`total` on it whose meaning comes entirely from its module.
// `calc::abv::simple` reads; `calc::simple` does not.
