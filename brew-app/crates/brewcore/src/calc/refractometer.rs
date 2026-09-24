//! Refractometer readings, before and after pitch.
//!
//! A refractometer measures how much a liquid bends light, and sugar and ethanol bend
//! it differently. Before pitching that does not matter and the reading is a scale
//! conversion. After pitching it matters enormously: alcohol inflates the reading, and
//! a beer that finished at 1.008 reads around 6 °Bx, which converts naively to 1.024.
//!
//! Two corrections, applied in order and never mixed with the hydrometer's:
//!
//! 1. The **wort correction factor** divides every reading. A refractometer is
//!    calibrated against sucrose and wort is not sucrose.
//! 2. **Terrill's cubic** removes the alcohol term from a post-pitch reading.
//!
//! Sources: Sean Terrill, "Refractometer FG Results", 2011-04-07,
//! <https://seanterrill.com/2011/04/07/refractometer-fg-results/>, coefficients
//! cross-checked against Brewer's Friend's independent rendering,
//! <https://www.brewersfriend.com/2013/04/24/using-your-refractometer-correctly-for-maximum-accuracy-in-home-brewing/>.

use super::gravity::{plato_to_sg, sg_to_plato};
use crate::{Error, Result, units::Sg};

/// The usual starting point for beer wort.
///
/// Instrument-specific in truth — the honest way to get one is to read a wort on the
/// refractometer and again on a hydrometer and divide — and **1.0 for a pure-sucrose
/// wash**, which is most of what this app ferments. That is precisely why nothing in
/// this crate applies a default: the right value for a seltzer is not this one.
pub const WORT_CORRECTION_DEFAULT: f64 = 1.04;

/// Refractometers that need a factor outside this are broken, not calibrated.
pub const VALID_WORT_CORRECTION: std::ops::RangeInclusive<f64> = 0.90..=1.20;

/// The range the scale conversions are meaningful over, in °P after the wort
/// correction. 44 °P is roughly the top of a refractometer's Brix scale.
const VALID_CORRECTED: std::ops::RangeInclusive<f64> = 0.0..=44.0;

/// A refractometer reading taken **before** pitching, where there is no alcohol to
/// distort it.
///
/// ```text
/// SG = plato_to_sg(brix / wort_correction)
/// ```
///
/// [`plato_to_sg`] and not `brix_to_sg`: dividing a wort's refraction reading by the
/// wort correction factor is *defined* as producing extract by mass, which is °Plato.
/// It is also how Terrill defines OB and FB, so this function and
/// [`final_gravity_from_brix`] speak one scale between them.
pub fn original_gravity_from_brix(brix: f64, wort_correction: f64) -> Result<Sg> {
    let corrected = corrected_brix(brix, wort_correction, "refractometer reading")?;
    plato_to_sg(corrected)
}

/// A refractometer reading taken **after** pitching, corrected for alcohol — Sean
/// Terrill's cubic.
///
/// With `OB = sg_to_plato(original)` and `FB = final_brix / wort_correction`:
///
/// ```text
/// FG = 1.0000 − 0.0044993·OB + 0.011774·FB
///             + 0.00027581·OB² − 0.0012717·FB²
///             − 0.0000072800·OB³ + 0.000063293·FB³
/// ```
///
/// **Error band: σ ≈ 0.0015 SG, with a mean bias of about +0.0007 SG.** Fitted on 12
/// beers spanning OG 9–24 °P, apparent residual extract 1.8–5.6 °P and apparent
/// attenuation 73–91 %. Below roughly 50 % apparent attenuation this is extrapolation
/// rather than measurement, and the UI should label anything it returns "estimated".
///
/// AJ deLange's debiased variant differs from this only in the leading constant
/// (0.9993 rather than 1.0000). Terrill pinned his to unity deliberately, so that the
/// formula stays sane outside the range it was interpolated over; that is the version
/// implemented here, and the bias is documented rather than quietly removed.
pub fn final_gravity_from_brix(original: Sg, final_brix: f64, wort_correction: f64) -> Result<Sg> {
    let fb = corrected_brix(final_brix, wort_correction, "refractometer reading")?;
    let ob = sg_to_plato(original);

    // A final reading above the original is a swapped pair, and the cubic answers it
    // with a confident number well above 1.000. Refusing is the only way anyone finds
    // out which way round they typed them.
    if fb > ob {
        return Err(Error::CalcOutOfRange {
            what: "refractometer reading after pitch, which is above the original",
            value: fb,
        });
    }

    let fg = 1.0 - 0.0044993 * ob + 0.011774 * fb + 0.00027581 * ob * ob
        - 0.0012717 * fb * fb
        - 0.0000072800 * ob * ob * ob
        + 0.000063293 * fb * fb * fb;
    Sg::new(fg)
}

/// Divide by the wort correction factor, refusing every way the inputs can be wrong
/// before the caller's formula gets to see them.
fn corrected_brix(brix: f64, wort_correction: f64, what: &'static str) -> Result<f64> {
    if !brix.is_finite() {
        return Err(Error::NotFinite {
            field: "refractometer reading",
            value: brix,
        });
    }
    if !wort_correction.is_finite() {
        return Err(Error::NotFinite {
            field: "wort correction factor",
            value: wort_correction,
        });
    }
    if !VALID_WORT_CORRECTION.contains(&wort_correction) {
        return Err(Error::CalcOutOfRange {
            what: "wort correction factor",
            value: wort_correction,
        });
    }

    let corrected = brix / wort_correction;
    if !VALID_CORRECTED.contains(&corrected) {
        return Err(Error::CalcOutOfRange { what, value: brix });
    }
    Ok(corrected)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brewer's Friend's own worked example: 12.5 °Bx of wort at a 1.04 factor is a
    /// 1.048 OG, and a 4.16 °Bx reading against it finishes just under 1.004.
    #[test]
    fn the_brewers_friend_worked_example_reproduces() {
        let og = original_gravity_from_brix(12.5, 1.04).unwrap();
        assert!((og.get() - 1.048).abs() < 0.001, "{og}");

        let fg = final_gravity_from_brix(og, 4.16, 1.04).unwrap();
        assert!((fg.get() - 1.0039).abs() < 0.002, "{fg}");
    }

    #[test]
    fn a_pre_pitch_reading_is_a_plain_scale_conversion_with_no_alcohol_term() {
        // With no correction factor at all, the reading *is* °Plato.
        let og = original_gravity_from_brix(11.91, 1.0).unwrap();
        let direct = plato_to_sg(11.91).unwrap();
        assert!((og.get() - direct.get()).abs() < 1e-12, "{og} vs {direct}");
    }

    #[test]
    fn the_wort_correction_factor_divides_before_anything_else() {
        let with = original_gravity_from_brix(12.48, 1.04).unwrap();
        let without = original_gravity_from_brix(12.0, 1.0).unwrap();
        assert!(
            (with.get() - without.get()).abs() < 1e-9,
            "{with} vs {without}"
        );
    }

    /// The reason [`WORT_CORRECTION_DEFAULT`] is never applied silently: on a sugar
    /// wash the right factor is 1.0, and using 1.04 costs two gravity points — more
    /// than the difference this app is trying to measure at the end of a ferment.
    #[test]
    fn a_wort_correction_factor_of_one_is_right_for_a_sugar_wash() {
        let wash = original_gravity_from_brix(12.5, 1.0).unwrap();
        let wort = original_gravity_from_brix(12.5, WORT_CORRECTION_DEFAULT).unwrap();
        assert!(wash.get() - wort.get() > 0.002, "{wash} vs {wort}");
    }

    #[test]
    fn a_final_reading_above_the_original_is_refused_rather_than_answered_confidently() {
        let og = original_gravity_from_brix(12.5, 1.04).unwrap();
        let err = final_gravity_from_brix(og, 14.0, 1.04).unwrap_err();
        assert_eq!(err.code(), "calc.out_of_range");
    }

    #[test]
    fn a_wort_correction_factor_outside_the_band_is_refused() {
        assert_eq!(
            original_gravity_from_brix(12.5, 2.0).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            original_gravity_from_brix(12.5, 0.0).unwrap_err().code(),
            "calc.out_of_range"
        );
    }

    #[test]
    fn a_reading_off_the_end_of_the_scale_is_refused() {
        assert_eq!(
            original_gravity_from_brix(80.0, 1.04).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            original_gravity_from_brix(-1.0, 1.04).unwrap_err().code(),
            "calc.out_of_range"
        );
    }

    #[test]
    fn a_non_finite_input_is_refused() {
        assert_eq!(
            original_gravity_from_brix(f64::NAN, 1.04)
                .unwrap_err()
                .code(),
            "units.not_finite"
        );
        assert_eq!(
            original_gravity_from_brix(12.5, f64::NAN)
                .unwrap_err()
                .code(),
            "units.not_finite"
        );
    }
}
