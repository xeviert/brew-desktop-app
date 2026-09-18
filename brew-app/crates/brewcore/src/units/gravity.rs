//! Gravity. Stored SG.
//!
//! Only the newtype lives here. SG↔Plato and SG↔Brix are polynomial fits of
//! measured data, not definitions, so they belong in [`crate::calc`] with their
//! sources cited — see the module docs on [`super`].

use super::canonical_float;

canonical_float!(
    /// Specific gravity, e.g. `1.0374`. Never Plato, never Brix.
    Sg,
    "gravity",
    ""
);

impl Sg {
    /// Water is 1.000; a sugar wash starts near 1.040 and finishes near 1.000.
    /// Anything outside this band came from the wrong CSV column.
    ///
    /// This is the check that catches `docs/BUILDOUT.md`'s warning about reading
    /// gravity out of the `state` column and landing 4.0 in the SG series.
    pub const PLAUSIBLE: std::ops::RangeInclusive<f64> = 0.900..=1.200;

    pub fn is_plausible(self) -> bool {
        Self::PLAUSIBLE.contains(&self.get())
    }
}

/// UI preference only. Converting to Plato or Brix for display is [`crate::calc`]'s
/// job; storage is always SG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GravityDisplay {
    #[default]
    Sg,
    Plato,
    Brix,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plausibility_catches_a_misread_column() {
        assert!(Sg::new(1.0374).unwrap().is_plausible());
        assert!(Sg::new(1.000).unwrap().is_plausible());
        // A `state` enum value read as gravity.
        assert!(!Sg::new(4.0).unwrap().is_plausible());
        // A temperature read as gravity.
        assert!(!Sg::new(18.5).unwrap().is_plausible());
    }

    #[test]
    fn non_finite_is_rejected() {
        assert!(Sg::new(f64::NAN).is_err());
        assert!(Sg::new(f64::INFINITY).is_err());
    }

    #[test]
    fn ordering_works_for_the_finish_detector() {
        assert!(Sg::new(1.000).unwrap() < Sg::new(1.040).unwrap());
    }
}
