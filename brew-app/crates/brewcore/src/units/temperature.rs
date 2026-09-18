//! Temperature. Stored °C; Fahrenheit exists only at the edges.

use super::canonical_float;

canonical_float!(
    /// Degrees Celsius — the only temperature representation the database sees.
    Celsius,
    "temperature",
    " °C"
);

impl Celsius {
    /// Anything outside this is a sensor fault or a parse error, not a reading.
    /// Advisory: the ingest path decides what to do about it (Phase 2b logs a
    /// warning and stores `None` rather than poisoning the series).
    pub const PLAUSIBLE: std::ops::RangeInclusive<f64> = -50.0..=150.0;

    pub fn from_fahrenheit(f: f64) -> crate::Result<Self> {
        Self::new((f - 32.0) * 5.0 / 9.0)
    }

    pub fn to_fahrenheit(self) -> f64 {
        self.get() * 9.0 / 5.0 + 32.0
    }

    pub fn is_plausible(self) -> bool {
        Self::PLAUSIBLE.contains(&self.get())
    }
}

/// The scale a value arrived in. Fermentrack writes `C` or `F` into every CSV row,
/// and it can change mid-file, so conversion is per-row rather than per-file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TempScale {
    C,
    F,
}

impl TempScale {
    pub fn to_celsius(self, value: f64) -> crate::Result<Celsius> {
        match self {
            TempScale::C => Celsius::new(value),
            TempScale::F => Celsius::from_fahrenheit(value),
        }
    }

    pub fn from_celsius(self, c: Celsius) -> f64 {
        match self {
            TempScale::C => c.get(),
            TempScale::F => c.to_fahrenheit(),
        }
    }
}

impl std::str::FromStr for TempScale {
    type Err = crate::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        match s.trim() {
            "C" | "c" => Ok(TempScale::C),
            "F" | "f" => Ok(TempScale::F),
            other => Err(crate::Error::UnknownUnit(other.to_owned())),
        }
    }
}

/// UI preference only. Storage is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TempDisplay {
    #[default]
    C,
    F,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::close;

    /// Exact by definition, so these can be asserted tightly.
    #[test]
    fn known_fixed_points() {
        assert!(close(Celsius::from_fahrenheit(32.0).unwrap().get(), 0.0));
        assert!(close(Celsius::from_fahrenheit(212.0).unwrap().get(), 100.0));
        assert!(close(Celsius::new(0.0).unwrap().to_fahrenheit(), 32.0));
        assert!(close(Celsius::new(100.0).unwrap().to_fahrenheit(), 212.0));
    }

    /// The one temperature that reads the same on both scales.
    #[test]
    fn forty_below_is_the_same_on_both_scales() {
        assert!(close(Celsius::from_fahrenheit(-40.0).unwrap().get(), -40.0));
        assert!(close(Celsius::new(-40.0).unwrap().to_fahrenheit(), -40.0));
    }

    #[test]
    fn round_trips() {
        for f in [-40.0, 0.0, 32.0, 68.0, 98.6, 212.0, 451.0] {
            let c = Celsius::from_fahrenheit(f).unwrap();
            assert!(close(c.to_fahrenheit(), f), "{f} did not survive");
        }
        for c in [-40.0, -17.7, 0.0, 18.5, 20.0, 100.0] {
            let back = Celsius::from_fahrenheit(Celsius::new(c).unwrap().to_fahrenheit()).unwrap();
            assert!(close(back.get(), c), "{c} did not survive");
        }
    }

    #[test]
    fn scale_drives_conversion_per_value() {
        assert!(close(TempScale::C.to_celsius(18.5).unwrap().get(), 18.5));
        assert!(close(TempScale::F.to_celsius(65.3).unwrap().get(), 18.5));
    }

    #[test]
    fn non_finite_is_rejected() {
        assert!(Celsius::new(f64::NAN).is_err());
        assert!(Celsius::new(f64::INFINITY).is_err());
        assert!(Celsius::from_fahrenheit(f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn plausibility_is_advisory_not_enforced() {
        let absurd = Celsius::new(4000.0).unwrap();
        assert!(!absurd.is_plausible());
        assert!(Celsius::new(18.5).unwrap().is_plausible());
    }
}
