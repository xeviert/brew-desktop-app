//! Attenuation, real extract and alcohol by weight — Balling's equations.
//!
//! Apparent attenuation is arithmetic on two hydrometer readings and needs no
//! reference. Everything else here does: once yeast has made alcohol, the hydrometer
//! is reading a mixture of sugar and ethanol and no longer reports extract at all.
//! Recovering the *real* extract from an apparent one is an empirical relation fitted
//! by Balling in the 1840s and carried into the ASBC methods unchanged.
//!
//! ```text
//! OE = sg_to_plato(OG)        original extract, °P
//! AE = sg_to_plato(FG)        apparent extract, °P
//! RE = 0.1808·OE + 0.8192·AE  real extract, °P
//! ```
//!
//! Source: K. Balling, *Die Bierbrauerei* (1845), as given in ASBC *Methods of
//! Analysis* (Beer-4) and reproduced in G. Fix, *Principles of Brewing Science*
//! (Brewers Publications, 2nd ed. 1999), §3. The 0.1808/0.8192 split is exact in the
//! sense that everyone quotes the same two numbers; the **±0.1 °P** band on `RE`
//! comes from the fit, which was made on beers of 10–14 °P.
//!
//! Fractions, not percents, for the reason `db::reference` gives: a `78` typed where
//! `0.78` was meant reads as a perfectly plausible band. Unlike the columns there,
//! nothing here is clamped to 0..1 — a sugar wash finishing below 1.000 attenuates
//! *past* 100 %, which is a real result and not an input error.

use super::gravity::sg_to_plato;
use crate::{Error, Result, units::Sg};

/// The smallest `OG − 1` this module will divide by.
///
/// [`Sg::PLAUSIBLE`] starts at 0.900, so a 1.0002 "wort" reaches the divide today and
/// [`apparent`] answers it with a number in the thousands. Two gravity points is a
/// hydrometer's resolution; below it there was no fermentables to attenuate.
const MIN_ORIGINAL_EXCESS: f64 = 0.002;

/// Apparent attenuation, as a fraction.
///
/// ```text
/// ADF = (OG − FG) / (OG − 1)
/// ```
///
/// "Apparent" because both readings are hydrometer readings, and the alcohol in the
/// second one depresses it: the number comes out about a fifth higher than the
/// fraction of sugar actually consumed. That is the convention every recipe, yeast
/// datasheet and [`crate::db::reference::YeastStrainRow::attenuation_band`] uses, so
/// it is the one to compare a batch against.
///
/// **May exceed 1.0.** A seltzer at OG 1.075 finishing at 0.998 returns 1.027, and
/// that is the correct answer: the wash is less dense than water because it is mostly
/// ethanol. See [`real`] for the number that cannot pass 1.0.
pub fn apparent(og: Sg, fg: Sg) -> Result<f64> {
    let (og, fg) = ordered(og, fg)?;
    Ok((og.get() - fg.get()) / (og.get() - 1.0))
}

/// [`apparent`] as a curve: how far each gravity reading along a ferment has come
/// from `og`, for 5b's overlay.
///
/// Refuses only an `og` too close to water. A reading *above* the OG is not refused,
/// unlike [`apparent`]'s swapped pair: during the lag phase sensor jitter puts half
/// the readings a point over, and a slightly negative fraction is what that looks
/// like. Not clamped either way, for the reason [`apparent`] gives.
pub fn apparent_progress(og: Sg) -> Result<impl Fn(f64) -> f64> {
    let og = ordered(og, og)?.0.get();
    Ok(move |sg| (og - sg) / (og - 1.0))
}

/// Real extract in °P — what is actually still dissolved, alcohol discounted.
///
/// ```text
/// RE = 0.1808·OE + 0.8192·AE
/// ```
///
/// Not a fraction and not an SG: real extract is a mass fraction of the beer and °P
/// is its unit. It is an input to [`real`] and [`abw`] far more often than it is a
/// number anyone reads.
pub fn real_extract(og: Sg, fg: Sg) -> Result<f64> {
    let (og, fg) = ordered(og, fg)?;
    Ok(0.1808 * sg_to_plato(og) + 0.8192 * sg_to_plato(fg))
}

/// Real attenuation — the real degree of fermentation, as a fraction.
///
/// ```text
/// RDF = (OE − RE) / OE
/// ```
///
/// The fraction of the original extract genuinely fermented, so unlike [`apparent`]
/// it cannot exceed 1.0. Roughly `0.82 × ADF` for ordinary beer, which is the
/// sanity check [`real_attenuation_is_about_four_fifths_of_apparent`] pins.
pub fn real(og: Sg, fg: Sg) -> Result<f64> {
    let oe = sg_to_plato(ordered(og, fg)?.0);
    if oe.abs() < f64::EPSILON {
        return Err(Error::CalcOutOfRange {
            what: "original extract for attenuation",
            value: oe,
        });
    }
    Ok((oe - real_extract(og, fg)?) / oe)
}

/// Alcohol by weight, as a fraction.
///
/// ```text
/// ABW = (OE − RE) / (2.0665 − 0.010665·OE) / 100
/// ```
///
/// The trailing `/100` is this crate's, not Balling's: the relation is published in
/// percent by weight, and everything here is a fraction.
///
/// Balling's relation, the same source as [`real_extract`]. **Fitted on beers of
/// 10–14 °P**; a 20 °P sugar wash is outside that and the result should be labelled
/// an estimate. Refused past [`MAX_ORIGINAL_EXTRACT`] rather than extrapolated,
/// because the denominator reaches zero at 193 °P and is already misbehaving long
/// before that.
///
/// ABV is deliberately not derived here — see [`super::abv`], which offers two
/// formulas precisely because no single one is right across this app's range.
/// The conversion, for anyone comparing numbers, is `ABV ≈ ABW × FG / 0.794`.
pub fn abw(og: Sg, fg: Sg) -> Result<f64> {
    let (og, fg) = ordered(og, fg)?;
    let oe = sg_to_plato(og);
    if oe > MAX_ORIGINAL_EXTRACT {
        return Err(Error::CalcOutOfRange {
            what: "original extract for alcohol by weight",
            value: oe,
        });
    }
    Ok((oe - real_extract(og, fg)?) / (2.0665 - 0.010665 * oe) / 100.0)
}

/// The top of the range Balling's ABW relation is usable over, in °P. Around 30 °P
/// is a barleywine or a strong wash; past it the fit is guesswork.
pub const MAX_ORIGINAL_EXTRACT: f64 = 30.0;

/// Every refusal these formulas share, in one place so each of them makes the same
/// four checks in the same order.
///
/// A swapped pair is the mistake worth catching loudest: `(1.010, 1.050)` returns a
/// confident negative attenuation, and negative is exactly what a stalled ferment
/// nearly looks like.
fn ordered(og: Sg, fg: Sg) -> Result<(Sg, Sg)> {
    if og.get() - 1.0 < MIN_ORIGINAL_EXCESS {
        return Err(Error::CalcOutOfRange {
            what: "original gravity for attenuation",
            value: og.get(),
        });
    }
    if fg.get() > og.get() {
        return Err(Error::CalcOutOfRange {
            what: "final gravity, which is above the original",
            value: fg.get(),
        });
    }
    Ok((og, fg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }

    /// The worked example: a 1.050 wort finishing at 1.010. Every brewing text puts
    /// this at 80 % apparent, ~65 % real, ~4.2 % ABW, with a real extract near
    /// 4.3 °P. The intermediate values are `OE = 12.39 °P`, `AE = 2.56 °P`.
    #[test]
    fn the_textbook_beer_attenuates_eighty_percent_apparent_and_sixty_five_real() {
        let (og, fg) = (sg(1.050), sg(1.010));
        assert!((apparent(og, fg).unwrap() - 0.800).abs() < 0.001);
        assert!((real_extract(og, fg).unwrap() - 4.34).abs() < 0.05);
        assert!((real(og, fg).unwrap() - 0.650).abs() < 0.005);
        assert!((abw(og, fg).unwrap() - 0.0416).abs() < 0.0005);
    }

    /// The relation the whole apparent/real distinction rests on. If `real` ever
    /// drifted away from four-fifths of `apparent` across the band, one of the two
    /// would be reading the wrong scale.
    #[test]
    fn real_attenuation_is_about_four_fifths_of_apparent() {
        for (o, f) in [
            (1.040, 1.008),
            (1.050, 1.012),
            (1.060, 1.014),
            (1.070, 1.018),
        ] {
            let ratio = real(sg(o), sg(f)).unwrap() / apparent(sg(o), sg(f)).unwrap();
            assert!((0.78..0.86).contains(&ratio), "{o}/{f}: {ratio}");
        }
    }

    /// A sugar wash finishes below water, and its apparent attenuation is genuinely
    /// over 100 %. Clamping this to 1.0 would hide the one result that tells you the
    /// wash went dry.
    #[test]
    fn a_seltzer_finishing_under_one_point_zero_attenuates_past_one_hundred_percent() {
        let a = apparent(sg(1.075), sg(0.998)).unwrap();
        assert!((a - 1.027).abs() < 0.002, "{a}");
        // Real attenuation is the one that cannot: it is still short of everything
        // fermenting, because real extract never goes negative here.
        let r = real(sg(1.075), sg(0.998)).unwrap();
        assert!(r < 1.0 && r > 0.80, "{r}");
    }

    #[test]
    fn progress_is_apparent_attenuation_along_the_curve() {
        let f = apparent_progress(sg(1.050)).unwrap();
        assert!((f(1.010) - 0.800).abs() < 1e-9);
        assert!((f(1.050)).abs() < 1e-9);
        // Lag-phase jitter above the OG, and a seltzer finishing under water.
        assert!(f(1.051) < 0.0);
        assert!(apparent_progress(sg(1.075)).unwrap()(0.998) > 1.0);
        assert_eq!(
            apparent_progress(sg(1.001)).err().unwrap().code(),
            "calc.out_of_range"
        );
    }

    #[test]
    fn a_swapped_pair_is_refused_rather_than_read_as_a_stall() {
        let err = apparent(sg(1.010), sg(1.050)).unwrap_err();
        assert_eq!(err.code(), "calc.out_of_range");
        assert_eq!(real(sg(1.010), sg(1.050)).unwrap_err().code(), err.code());
        assert_eq!(abw(sg(1.010), sg(1.050)).unwrap_err().code(), err.code());
    }

    /// Water in, water out: `(OG − 1)` is not a denominator at 1.000.
    #[test]
    fn an_original_gravity_at_water_is_refused_rather_than_divided_by() {
        assert_eq!(
            apparent(sg(1.000), sg(1.000)).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            apparent(sg(1.0015), sg(1.000)).unwrap_err().code(),
            "calc.out_of_range"
        );
        // Two gravity points is the floor, and it is admitted.
        assert!(apparent(sg(1.002), sg(1.000)).is_ok());
    }

    /// Balling's ABW denominator hits zero at 193 °P. Long before that the fit is
    /// fiction, so the refusal sits at the edge of the documented range.
    #[test]
    fn an_extract_past_the_fitted_range_is_refused_rather_than_extrapolated() {
        // 1.140 is about 32 °P, just over the cap.
        let err = abw(sg(1.140), sg(1.010)).unwrap_err();
        assert_eq!(err.code(), "calc.out_of_range");
        // Attenuation itself has no such limit — it is arithmetic, not a fit.
        assert!(apparent(sg(1.140), sg(1.010)).is_ok());
    }

    /// A batch that has not moved is 0 % attenuated, not an error.
    #[test]
    fn an_unfermented_batch_reads_zero_rather_than_failing() {
        assert_eq!(apparent(sg(1.050), sg(1.050)).unwrap(), 0.0);
        assert!(real(sg(1.050), sg(1.050)).unwrap().abs() < 1e-12);
        assert!(abw(sg(1.050), sg(1.050)).unwrap().abs() < 1e-12);
    }
}
