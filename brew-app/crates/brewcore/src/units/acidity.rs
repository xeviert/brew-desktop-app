//! pH and titratable acidity. Both are manual-entry only (Phase 3e).
//!
//! Neither needs a conversion — pH is dimensionless and acidity is canonical g/L —
//! so what these newtypes buy is the same thing [`super::Sg`] buys: NaN rejection at
//! the entry point, and `ToSql`/`FromSql` that db code binds instead of a bare `f64`.
//!
//! The reference acid is fixed here and nowhere else. `reading` is
//! `(stream_id, ts, value)` and has no room to record which acid a number meant, so
//! a titratable acidity that is sometimes tartaric and sometimes malic is a column
//! that cannot be compared across batches. One acid, chosen once, in the type.

use super::canonical_float;

canonical_float!(
    /// The pH scale. Dimensionless by definition, which is why
    /// `tests/schema_conventions.rs` blesses a bare `ph` column name.
    Ph,
    "ph",
    ""
);

canonical_float!(
    /// Titratable acidity in grams per litre, **expressed as tartaric acid**.
    GramsPerLitre,
    "acidity",
    " g/L"
);

impl Ph {
    /// The scale itself. A ferment lives between roughly 3 and 5; anything outside
    /// 0–14 is a typing slip, not a measurement.
    pub const PLAUSIBLE: std::ops::RangeInclusive<f64> = 0.0..=14.0;

    pub fn is_plausible(self) -> bool {
        Self::PLAUSIBLE.contains(&self.get())
    }
}

impl GramsPerLitre {
    /// Wine runs 4–10 g/L and a seltzer base far less. The ceiling is generous on
    /// purpose: this catches a decimal point in the wrong place, not a bad batch.
    pub const PLAUSIBLE: std::ops::RangeInclusive<f64> = 0.0..=100.0;

    pub fn is_plausible(self) -> bool {
        Self::PLAUSIBLE.contains(&self.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ph_scale_is_the_plausible_band() {
        assert!(Ph::new(3.4).unwrap().is_plausible());
        assert!(Ph::new(0.0).unwrap().is_plausible());
        assert!(Ph::new(14.0).unwrap().is_plausible());
        // A gravity typed into the pH box.
        assert!(!Ph::new(-1.0).unwrap().is_plausible());
        assert!(!Ph::new(47.0).unwrap().is_plausible());
    }

    #[test]
    fn a_titratable_acidity_below_zero_is_not_plausible() {
        assert!(GramsPerLitre::new(6.2).unwrap().is_plausible());
        assert!(GramsPerLitre::new(0.0).unwrap().is_plausible());
        assert!(!GramsPerLitre::new(-0.1).unwrap().is_plausible());
        assert!(!GramsPerLitre::new(620.0).unwrap().is_plausible());
    }

    #[test]
    fn non_finite_is_rejected() {
        assert!(Ph::new(f64::NAN).is_err());
        assert!(Ph::new(f64::INFINITY).is_err());
        assert!(GramsPerLitre::new(f64::NAN).is_err());
        assert!(GramsPerLitre::new(f64::NEG_INFINITY).is_err());
    }
}
