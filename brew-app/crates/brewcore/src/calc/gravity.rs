//! SG ↔ °Plato ↔ °Brix.
//!
//! All three describe the same dissolved sugar, and none of the conversions between
//! them is exact: Plato and Brix are both tabulated against sucrose solutions, and
//! everything below is a cubic regression on those tables. That is why they live
//! here and not in [`crate::units`] — see that module's doc.
//!
//! Plato and Brix are *different* fits, not aliases. Plato tables are generated
//! against density and Brix tables against refractive index; over the brewing range
//! the two agree to under a tenth of a degree, which is why the literature is casual
//! about the distinction and why this module is not.

use crate::{
    Error, Result,
    units::{GravityDisplay, Sg},
};

/// Apparent extract in °Plato, from apparent specific gravity at 20 °C/20 °C.
///
/// ```text
/// °P = 135.997·S³ − 630.272·S² + 1111.14·S − 616.868
/// ```
///
/// A cubic regression on the Plato tables in J. de Clerck, *A Textbook of Brewing*
/// Vol. 1 (Chapman & Hall, 1957), as maintained by the ASBC. Agrees with de Clerck's
/// own closed form `SG = 1 + P/(258.6 − 227.1·P/258.2)` to **±0.02 °P** over
/// 1.000–1.120, which [`the_cubic_agrees_with_the_de_clerck_closed_form_to_two_hundredths_of_a_plato`]
/// pins.
///
/// Infallible: the polynomial is defined everywhere and [`Sg`] is already finite.
pub fn sg_to_plato(sg: Sg) -> f64 {
    let s = sg.get();
    135.997 * s * s * s - 630.272 * s * s + 1111.14 * s - 616.868
}

/// °Brix from specific gravity.
///
/// ```text
/// °Bx = 143.254·S³ − 648.670·S² + 1125.805·S − 620.389
/// ```
///
/// The fit used by the winemaking calculators (Vinolab, VinoCalc), which tabulate
/// against refractive index rather than density. Over 1.000–1.100 it tracks
/// [`sg_to_plato`] to under 0.1 °; the divergence grows past that, well outside
/// anything this app stores.
pub fn sg_to_brix(sg: Sg) -> f64 {
    let s = sg.get();
    143.254 * s * s * s - 648.670 * s * s + 1125.805 * s - 620.389
}

/// `sg` in the `ui.gravity_display` scale, unchanged for SG.
pub fn sg_to_display(sg: Sg, display: GravityDisplay) -> f64 {
    match display {
        GravityDisplay::Sg => sg.get(),
        GravityDisplay::Plato => sg_to_plato(sg),
        GravityDisplay::Brix => sg_to_brix(sg),
    }
}

/// The inverse of [`sg_to_display`]: a gravity typed in the `ui.gravity_display`
/// scale, as SG. An SG typed outside [`Sg::PLAUSIBLE`] comes back as typed — the
/// caller's plausibility check names it; the two fits refuse it themselves.
pub fn display_to_sg(value: f64, display: GravityDisplay) -> Result<Sg> {
    match display {
        GravityDisplay::Sg => Sg::new(value),
        GravityDisplay::Plato => plato_to_sg(value),
        GravityDisplay::Brix => brix_to_sg(value),
    }
}

/// The inverse of [`sg_to_plato`], by bisection over [`Sg::PLAUSIBLE`].
///
/// A numeric inverse rather than de Clerck's closed form so that both directions are
/// the *same* fit and round-trip to the solver's tolerance. Mixing the two would put
/// a silent 0.02 °P step in the middle of every SG → °P → SG path.
///
/// `plato` outside what the plausible SG band can express is
/// [`Error::CalcOutOfRange`], never an extrapolation.
pub fn plato_to_sg(plato: f64) -> Result<Sg> {
    invert(sg_to_plato, plato, "plato")
}

/// The inverse of [`sg_to_brix`], by bisection. Same reasoning as [`plato_to_sg`].
pub fn brix_to_sg(brix: f64) -> Result<Sg> {
    invert(sg_to_brix, brix, "brix")
}

/// Bisection over [`Sg::PLAUSIBLE`].
///
/// Both cubics are strictly increasing on the reals — their derivatives
/// `407.991·S² − 1260.544·S + 1111.14` and `429.762·S² − 1297.34·S + 1125.805` each
/// have a negative discriminant, so neither has a real root and neither cubic can
/// turn — which is what makes a single bracketed root guaranteed.
/// `the_polynomials_are_monotone_across_the_plausible_band` pins the property the
/// argument rests on.
///
/// Bisection and not Newton: it needs no derivative to keep in sync with the
/// coefficients, cannot diverge, and 100 halvings of a 0.3-wide bracket is far past
/// `f64`'s resolution. At this call volume the cost is not measurable.
fn invert(f: fn(Sg) -> f64, target: f64, what: &'static str) -> Result<Sg> {
    if !target.is_finite() {
        return Err(Error::NotFinite {
            field: what,
            value: target,
        });
    }

    let (mut lo, mut hi) = (*Sg::PLAUSIBLE.start(), *Sg::PLAUSIBLE.end());
    let (lo_val, hi_val) = (f(Sg::new_unchecked(lo)), f(Sg::new_unchecked(hi)));
    if target < lo_val || target > hi_val {
        return Err(Error::CalcOutOfRange {
            what,
            value: target,
        });
    }

    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if f(Sg::new_unchecked(mid)) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Sg::new(0.5 * (lo + hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// de Clerck's closed form `SG = 1 + P/(258.6 − 227.1·P/258.2)`, solved for P.
    /// Kept only as the independent cross-check for [`sg_to_plato`].
    fn de_clerck_plato(sg: f64) -> f64 {
        let d = sg - 1.0;
        258.6 * d / (1.0 + (227.1 / 258.2) * d)
    }

    /// The worked example every brewing text uses: a 1.048 wort is a shade under
    /// 12 °P. de Clerck's tables give 11.91.
    #[test]
    fn a_ten_forty_eight_wort_is_just_under_twelve_plato() {
        let p = sg_to_plato(Sg::new(1.048).unwrap());
        assert!((p - 11.91).abs() < 0.05, "{p}");
    }

    /// A regression has a residual at every point, including this one: both cubics
    /// read about −0.003 °P for water rather than exactly zero. That is a thousand
    /// times smaller than a hydrometer can be read to, and forcing it to zero would
    /// mean refitting the published coefficients.
    #[test]
    fn pure_water_reads_zero_to_a_hundredth_of_a_plato() {
        let p = sg_to_plato(Sg::new(1.000).unwrap());
        assert!(p.abs() < 0.01, "{p}");
        let b = sg_to_brix(Sg::new(1.000).unwrap());
        assert!(b.abs() < 0.01, "{b}");
    }

    #[test]
    fn plato_and_sg_round_trip_to_the_solvers_tolerance() {
        for sg in [1.000, 1.010, 1.0374, 1.048, 1.090, 1.120] {
            let back = plato_to_sg(sg_to_plato(Sg::new(sg).unwrap())).unwrap();
            assert!((back.get() - sg).abs() < 1e-9, "{sg} -> {back}");
        }
    }

    #[test]
    fn brix_and_sg_round_trip_to_the_solvers_tolerance() {
        for sg in [1.000, 1.010, 1.0374, 1.048, 1.090, 1.120] {
            let back = brix_to_sg(sg_to_brix(Sg::new(sg).unwrap())).unwrap();
            assert!((back.get() - sg).abs() < 1e-9, "{sg} -> {back}");
        }
    }

    #[test]
    fn a_display_gravity_round_trips_in_every_scale() {
        for display in [
            GravityDisplay::Sg,
            GravityDisplay::Plato,
            GravityDisplay::Brix,
        ] {
            let sg = Sg::new(1.040).unwrap();
            let back = display_to_sg(sg_to_display(sg, display), display).unwrap();
            assert!((back.get() - 1.040).abs() < 1e-9, "{display:?} -> {back}");
        }
        // SG is passed through for the caller to judge; the fits refuse on their own.
        assert_eq!(
            display_to_sg(1040.0, GravityDisplay::Sg).unwrap().get(),
            1040.0
        );
        assert!(display_to_sg(400.0, GravityDisplay::Plato).is_err());
    }

    /// The error band in [`sg_to_plato`]'s doc, asserted rather than asserted-to.
    #[test]
    fn the_cubic_agrees_with_the_de_clerck_closed_form_to_two_hundredths_of_a_plato() {
        let mut sg = 1.000;
        while sg <= 1.120 {
            let drift = (sg_to_plato(Sg::new(sg).unwrap()) - de_clerck_plato(sg)).abs();
            assert!(drift < 0.02, "at SG {sg}: {drift} °P");
            sg += 0.002;
        }
    }

    #[test]
    fn brix_and_plato_agree_to_a_tenth_over_the_brewing_range() {
        let mut sg = 1.000;
        while sg <= 1.100 {
            let s = Sg::new(sg).unwrap();
            let gap = (sg_to_brix(s) - sg_to_plato(s)).abs();
            assert!(gap < 0.1, "at SG {sg}: {gap} °");
            sg += 0.002;
        }
    }

    /// [`invert`]'s precondition. If either cubic ever turned inside this band, the
    /// bisection could bracket the wrong root and the round-trip tests would still
    /// pass.
    #[test]
    fn the_polynomials_are_monotone_across_the_plausible_band() {
        let mut sg = *Sg::PLAUSIBLE.start();
        let (mut last_p, mut last_b) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        while sg <= *Sg::PLAUSIBLE.end() {
            let s = Sg::new(sg).unwrap();
            let (p, b) = (sg_to_plato(s), sg_to_brix(s));
            assert!(p > last_p, "plato turned at SG {sg}");
            assert!(b > last_b, "brix turned at SG {sg}");
            last_p = p;
            last_b = b;
            sg += 0.001;
        }
    }

    #[test]
    fn a_gravity_outside_the_plausible_band_is_refused_rather_than_extrapolated() {
        // Roughly 44 °P is the top of the band; 60 is off the end of it.
        let err = plato_to_sg(60.0).unwrap_err();
        assert_eq!(err.code(), "calc.out_of_range");
        assert_eq!(plato_to_sg(-40.0).unwrap_err().code(), "calc.out_of_range");
        assert_eq!(brix_to_sg(60.0).unwrap_err().code(), "calc.out_of_range");
    }

    #[test]
    fn a_non_finite_input_is_refused() {
        assert_eq!(
            plato_to_sg(f64::NAN).unwrap_err().code(),
            "units.not_finite"
        );
        assert_eq!(
            brix_to_sg(f64::INFINITY).unwrap_err().code(),
            "units.not_finite"
        );
    }
}
