//! Hydrometer temperature correction.
//!
//! A hydrometer floats at the depth the *density* of the liquid puts it, and density
//! falls as the liquid warms. A sample read hot therefore reads low, and by enough to
//! matter: 10 °C above calibration is worth about two and a half gravity points, which
//! is the difference between "still fermenting" and "done".
//!
//! Sources: the water-density cubic in general homebrew use, cross-checked against the
//! correction charts in Palmer's *How to Brew* and Noonan's *Brewing Lager Beer*.
//! <https://www.homebrewtalk.com/threads/temp-correction-formula-for-hydrometer.10684/>
//! <https://abvcalculator.org/hydrometer-temperature-correction/>

use crate::{
    Error, Result,
    units::{Celsius, Sg},
};

/// The band this correction is asserted against. The polynomial is defined far
/// wider — it is a smooth cubic — but agreement with the published charts is only
/// claimed inside this, and outside it the fit is extrapolation dressed as an answer.
pub const VALID_C: std::ops::RangeInclusive<f64> = 0.0..=100.0;

/// Water's relative density at `f` degrees **Fahrenheit**.
///
/// ```text
/// f(T) = 1.00130346 − 1.34722124e-4·T + 2.04052596e-6·T² − 2.32820948e-9·T³
/// ```
///
/// Fahrenheit because that is the scale the coefficients were fitted in. Converting
/// them to Celsius is a fourth opportunity to typo a nine-digit constant, so the
/// conversion happens on the input instead.
fn density_ratio(f: f64) -> f64 {
    1.00130346 - 1.34722124e-4 * f + 2.04052596e-6 * f * f - 2.32820948e-9 * f * f * f
}

/// Correct a hydrometer reading taken at `sample` for an instrument calibrated at
/// `calibration`.
///
/// ```text
/// corrected = reading · f(sample °F) / f(calibration °F)
/// ```
///
/// A **ratio of the polynomial at two temperatures**, not a difference of two
/// polynomial values added to the reading. Several calculator pages publish the
/// difference form with these same coefficients; it is wrong, and wrong in the
/// direction that looks plausible.
///
/// The fit is for pure water, so at high gravity the true correction is marginally
/// larger than this returns. Within 0–40 °C it matches the published charts to
/// ±0.0005 SG, which is smaller than a hydrometer can be read to.
pub fn correct_for_temperature(reading: Sg, sample: Celsius, calibration: Celsius) -> Result<Sg> {
    if !VALID_C.contains(&sample.get()) {
        return Err(Error::CalcOutOfRange {
            what: "hydrometer sample temperature",
            value: sample.get(),
        });
    }
    if !VALID_C.contains(&calibration.get()) {
        return Err(Error::CalcOutOfRange {
            what: "hydrometer calibration temperature",
            value: calibration.get(),
        });
    }

    let ratio = density_ratio(sample.to_fahrenheit()) / density_ratio(calibration.to_fahrenheit());
    Sg::new(reading.get() * ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(v: f64) -> Celsius {
        Celsius::new(v).unwrap()
    }

    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }

    #[test]
    fn a_reading_at_the_calibration_temperature_is_returned_unchanged() {
        let out = correct_for_temperature(sg(1.0374), c(20.0), c(20.0)).unwrap();
        assert!((out.get() - 1.0374).abs() < 1e-12, "{out}");
    }

    /// The case that matters in practice: a sample pulled straight from a warm
    /// fermenter reads low, and the correction has to raise it.
    #[test]
    fn a_warm_sample_reads_low_and_the_correction_raises_it() {
        let out = correct_for_temperature(sg(1.050), c(30.0), c(20.0)).unwrap();
        assert!(out.get() > 1.050);
        assert!((out.get() - 1.0526).abs() < 0.0005, "{out}");
    }

    #[test]
    fn a_cold_sample_reads_high_and_the_correction_lowers_it() {
        let out = correct_for_temperature(sg(1.050), c(5.0), c(20.0)).unwrap();
        assert!(out.get() < 1.050, "{out}");
    }

    /// Palmer's correction table in *How to Brew*, which is published as an **offset**
    /// to add to the reading, for a hydrometer calibrated at 60 °F. Reproducing it is
    /// what justifies the ±0.0005 SG band in [`correct_for_temperature`]'s doc — the
    /// worst disagreement below is 0.00022, at the two ends of the table.
    #[test]
    fn the_published_correction_chart_is_matched_within_half_a_gravity_point() {
        let sixty_f = Celsius::from_fahrenheit(60.0).unwrap();
        // (sample °F, Palmer's offset)
        let chart = [
            (50.0, -0.0005),
            (60.0, 0.0),
            (70.0, 0.0010),
            (80.0, 0.0023),
            (90.0, 0.0040),
            (100.0, 0.0063),
        ];
        for (sample_f, offset) in chart {
            let sample = Celsius::from_fahrenheit(sample_f).unwrap();
            let out = correct_for_temperature(sg(1.040), sample, sixty_f).unwrap();
            assert!(
                (out.get() - (1.040 + offset)).abs() < 0.0005,
                "at {sample_f} °F -> {out}, Palmer says {}",
                1.040 + offset
            );
        }
    }

    /// Older hydrometers are calibrated at 60 °F, and using one as though it were a
    /// 20 °C instrument is worth about a gravity point in the wrong direction.
    #[test]
    fn a_sixty_fahrenheit_hydrometer_corrects_the_other_way_at_twenty_degrees() {
        let sixty_f = Celsius::from_fahrenheit(60.0).unwrap();
        let out = correct_for_temperature(sg(1.050), c(20.0), sixty_f).unwrap();
        assert!(out.get() > 1.050, "{out}");
        assert!((out.get() - 1.050).abs() < 0.002, "{out}");
    }

    #[test]
    fn a_temperature_outside_the_fitted_band_is_refused() {
        let err = correct_for_temperature(sg(1.050), c(-5.0), c(20.0)).unwrap_err();
        assert_eq!(err.code(), "calc.out_of_range");
        let err = correct_for_temperature(sg(1.050), c(20.0), c(140.0)).unwrap_err();
        assert_eq!(err.code(), "calc.out_of_range");
    }
}
