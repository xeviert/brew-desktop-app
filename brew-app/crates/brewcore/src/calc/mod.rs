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
//! Phase 3e landed the three formulas manual entry needs. The rest of 3f — attenuation,
//! real extract, ABV, sugar-mass → OG, dilution, pitch rate, YAN — is still to come.

pub mod gravity;
pub mod hydrometer;
pub mod refractometer;

pub use gravity::{brix_to_sg, plato_to_sg, sg_to_brix, sg_to_plato};
pub use hydrometer::correct_for_temperature;
pub use refractometer::{
    WORT_CORRECTION_DEFAULT, final_gravity_from_brix, original_gravity_from_brix,
};
