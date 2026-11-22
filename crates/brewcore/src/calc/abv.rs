//! Alcohol by volume, two ways — and no function called `abv`.
//!
//! There is no single correct ABV formula. The one everybody knows is a linear fit
//! that was made against ordinary beer, and this app's main product is a seltzer
//! finishing near 1.000, which is exactly where it drifts. So both are here, neither
//! is the default, and a caller has to name the one it is showing.
//!
//! That is a UI obligation as much as an API one: a status card reading "10.1 %" and
//! another reading "10.5 %" for the same batch is a bug report, whereas "10.1 %
//! (simple)" is a number with a provenance.
//!
//! Fractions, not percents, like everything else in this crate — `0.052`, not `5.2`.
//! Both published constants are in percent form, so both are scaled here and the docs
//! say so, rather than leaving a factor of 100 to be discovered downstream.

use super::attenuation;
use crate::{Result, units::Sg};

/// The linear approximation, the one on the back of every hydrometer.
///
/// ```text
/// ABV = (OG − FG) × 1.3125          (published as × 131.25, in percent)
/// ```
///
/// Good to about **±0.3 % ABV** for beer under 1.070 OG finishing above 1.008, which
/// is the range it was fitted over. Past that it reads low, and it reads low fastest
/// exactly where a sugar wash lives: see
/// [`the_two_formulas_diverge_on_a_seltzer_and_agree_on_a_beer`].
///
/// The factor is not physics. It is a straight line through a curve, and the curve is
/// [`alternate`].
pub fn simple(og: Sg, fg: Sg) -> Result<f64> {
    let (og, fg) = checked(og, fg)?;
    Ok((og.get() - fg.get()) * 1.3125)
}

/// The high-attenuation formula — Michael Hall's, and the one to show for a seltzer.
///
/// ```text
/// ABV = (76.08·(OG − FG) / (1.775 − OG)) × (FG / 0.794) / 100
/// ```
///
/// Source: Michael L. Hall, "Brew by the Numbers: Add Up What's in Your Beer",
/// *Zymurgy* Summer 1995, pp. 54–59. `0.794` is the density of ethanol near 20 °C, so
/// the trailing term is the weight-to-volume conversion; the leading quotient is the
/// alcohol-by-weight estimate.
///
/// **Error band: ±0.2 % ABV** over OG 1.030–1.120, and it stays usable below 1.000 FG
/// where [`simple`] does not. The `(1.775 − OG)` denominator is what confines it:
/// past OG 1.775 it changes sign, far outside [`Sg::PLAUSIBLE`], so no extra guard is
/// needed beyond the shared one.
pub fn alternate(og: Sg, fg: Sg) -> Result<f64> {
    let (og, fg) = checked(og, fg)?;
    let abw = 76.08 * (og.get() - fg.get()) / (1.775 - og.get());
    Ok(abw * (fg.get() / 0.794) / 100.0)
}

/// Both formulas refuse the same inputs, and they are the ones
/// [`attenuation`] already names: a swapped pair, and an original gravity with
/// nothing dissolved in it.
fn checked(og: Sg, fg: Sg) -> Result<(Sg, Sg)> {
    attenuation::apparent(og, fg)?;
    Ok((og, fg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }

    /// The worked example both sources use: 1.050 → 1.010 is a 5.2–5.3 % beer.
    #[test]
    fn the_textbook_beer_is_a_little_over_five_percent_either_way() {
        let (og, fg) = (sg(1.050), sg(1.010));
        let s = simple(og, fg).unwrap();
        let a = alternate(og, fg).unwrap();
        assert!((s - 0.0525).abs() < 0.0005, "{s}");
        assert!((a - 0.0534).abs() < 0.0010, "{a}");
    }

    /// The reason this module exports two functions instead of one. On a beer they
    /// are within a tenth of a percent; on a seltzer that finishes dry they are four
    /// tenths apart, which is the difference between a label that is right and one
    /// that is not.
    #[test]
    fn the_two_formulas_diverge_on_a_seltzer_and_agree_on_a_beer() {
        let beer = (sg(1.050), sg(1.010));
        let gap = (alternate(beer.0, beer.1).unwrap() - simple(beer.0, beer.1).unwrap()).abs();
        assert!(gap < 0.0015, "beer gap {gap}");

        let seltzer = (sg(1.075), sg(0.998));
        let s = simple(seltzer.0, seltzer.1).unwrap();
        let a = alternate(seltzer.0, seltzer.1).unwrap();
        assert!((s - 0.1011).abs() < 0.0005, "{s}");
        assert!((a - 0.1052).abs() < 0.0010, "{a}");
        assert!(a - s > 0.003, "seltzer gap {}", a - s);
    }

    /// `alternate`'s own consistency check: its leading quotient is an alcohol-by-
    /// weight estimate, so it should track Balling's within the two bands' sum.
    #[test]
    fn the_alternate_formula_agrees_with_ballings_alcohol_by_weight() {
        for (o, f) in [(1.040, 1.008), (1.050, 1.010), (1.075, 0.998)] {
            let hall_abw = 76.08 * (o - f) / (1.775 - o) / 100.0;
            let balling = attenuation::abw(sg(o), sg(f)).unwrap();
            assert!(
                (hall_abw - balling).abs() < 0.004,
                "{o}/{f}: {hall_abw} vs {balling}"
            );
        }
    }

    /// Fractions. A percent slipping out of here would multiply every later number
    /// by 100 and still look like an ABV for a strong beer.
    #[test]
    fn the_result_is_a_fraction_not_a_percent() {
        assert!(simple(sg(1.050), sg(1.010)).unwrap() < 1.0);
        assert!(alternate(sg(1.050), sg(1.010)).unwrap() < 1.0);
    }

    #[test]
    fn both_refuse_a_swapped_pair_and_a_gravity_at_water() {
        assert_eq!(
            simple(sg(1.010), sg(1.050)).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            alternate(sg(1.000), sg(1.000)).unwrap_err().code(),
            "calc.out_of_range"
        );
    }
}
