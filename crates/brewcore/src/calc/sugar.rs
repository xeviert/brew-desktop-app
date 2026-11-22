//! Sugar-wash gravity, and blending.
//!
//! A seltzer base is effectively pure sucrose in water, which makes its gravity a
//! mass fraction rather than an estimate: °P *is* percent extract by mass, and the
//! Plato tables are tabulated against sucrose solutions specifically. So
//!
//! ```text
//! °P = 100 · sugar / (sugar + water)        both masses
//! ```
//!
//! is exact for this base, and everything empirical happens in the one step that
//! follows it — [`super::plato_to_sg`]. Beer's PPG tables are an approximation of a
//! malt extract's yield, which is an approximation of this; there is no reason to
//! borrow them here.
//!
//! Blending works the same way and for the same reason: extract is conserved by
//! *mass*, so mixing two washes is addition, not an interpolation of gravity points.

use super::gravity::{plato_to_sg, sg_to_plato};
use crate::{
    Error, Result,
    units::{Grams, Litres, Sg},
};

/// Water at 20 °C, kg/L.
///
/// Stated rather than taken as 1.000 because it is not: the difference is 0.2 %, and
/// at 20 °P that is 0.04 °P — a quarter of the error band on the Plato fit itself, so
/// small but not nothing. 20 °C is the reference temperature the Plato tables and
/// [`super::hydrometer`]'s default calibration both use.
pub const WATER_DENSITY_KG_PER_L: f64 = 0.99823;

/// The gravity of `sugar` dissolved in `water`.
///
/// Exact for sucrose, to the Plato fit's own **±0.02 °P**. Note the argument is the
/// water added, not the finished volume: dissolving a kilo of sugar in 4 L gives
/// rather more than 4 L of wash, and the mass fraction does not care.
///
/// Refuses a mixture past 44 °P — the top of what [`Sg::PLAUSIBLE`] can express, and
/// well past what any yeast will start in.
pub fn og_from_sugar(sugar: Grams, water: Litres) -> Result<Sg> {
    let water_g = water_mass(water)?;
    if sugar.get() < 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "sugar mass",
            value: sugar.get(),
        });
    }
    let total = sugar.get() + water_g;
    if total <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "water volume",
            value: water.get(),
        });
    }
    plato_to_sg(100.0 * sugar.get() / total)
}

/// The inverse: how much sugar `water` needs to reach `target`.
///
/// Closed form, no bisection — the mass fraction is algebra, and the only fit in the
/// path is [`sg_to_plato`], which is the infallible direction. Inverting
/// [`og_from_sugar`] numerically instead would put [`plato_to_sg`]'s solver in the
/// loop for no gain.
///
/// ```text
/// sugar = water · f / (1 − f),   f = °P/100
/// ```
pub fn sugar_for_og(target: Sg, water: Litres) -> Result<Grams> {
    let water_g = water_mass(water)?;
    let fraction = sg_to_plato(target) / 100.0;
    // Below water there is no sugar to add, and at f ≥ 1 the wash would be sugar
    // with no water in it — the asymptote, not a recipe.
    if !(0.0..1.0).contains(&fraction) {
        return Err(Error::CalcOutOfRange {
            what: "target gravity for a sugar wash",
            value: target.get(),
        });
    }
    Grams::new(water_g * fraction / (1.0 - fraction))
}

/// Blend any number of volumes, returning `(total volume, gravity)`.
///
/// The gravity is **exact**, because it is derived by conserving extract mass and
/// total mass, neither of which depends on the volumes being additive:
///
/// ```text
/// mass    = Σ Vᵢ · SGᵢ · 1000        grams
/// extract = Σ Vᵢ · SGᵢ · 1000 · Pᵢ/100
/// °P      = 100 · extract / mass
/// ```
///
/// The **volume** is the approximation: it is the sum of the parts, and mixing sugar
/// solutions is not strictly volume-additive. The error is under **0.5 %** for the
/// concentrations this app blends, which is smaller than anyone's ability to read a
/// fermenter's sight glass, but it is an assumption and not a result.
///
/// An empty slice is [`Error::CalcOutOfRange`] rather than water: blending nothing is
/// a caller bug, and returning 1.000 would hide it.
pub fn blend(parts: &[(Litres, Sg)]) -> Result<(Litres, Sg)> {
    if parts.is_empty() {
        return Err(Error::CalcOutOfRange {
            what: "number of parts to blend",
            value: 0.0,
        });
    }

    let (mut volume, mut mass, mut extract) = (0.0, 0.0, 0.0);
    for &(v, sg) in parts {
        if v.get() < 0.0 {
            return Err(Error::CalcOutOfRange {
                what: "blend volume",
                value: v.get(),
            });
        }
        let part_mass = v.get() * sg.get() * 1000.0;
        volume += v.get();
        mass += part_mass;
        extract += part_mass * sg_to_plato(sg) / 100.0;
    }
    if mass <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "total blend volume",
            value: volume,
        });
    }
    Ok((Litres::new(volume)?, plato_to_sg(100.0 * extract / mass)?))
}

/// Dilution — [`blend`] with water, kept separate because it is the case anyone
/// actually reaches for and because it returns only the gravity.
pub fn dilute(volume: Litres, gravity: Sg, water: Litres) -> Result<Sg> {
    Ok(blend(&[(volume, gravity), (water, Sg::new(1.0)?)])?.1)
}

/// Litres of water as grams, refusing a negative volume before it becomes a negative
/// mass fraction that [`plato_to_sg`] would then refuse for the wrong reason.
fn water_mass(water: Litres) -> Result<f64> {
    if water.get() < 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "water volume",
            value: water.get(),
        });
    }
    Ok(water.get() * 1000.0 * WATER_DENSITY_KG_PER_L)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }
    fn g(v: f64) -> Grams {
        Grams::new(v).unwrap()
    }
    fn l(v: f64) -> Litres {
        Litres::new(v).unwrap()
    }

    /// The worked example: 1 kg of sucrose in 4 L of water. By mass that is
    /// 1000/(1000 + 3993) = 20.03 %, so 20.03 °P, and the Plato tables put 20 °P at
    /// SG 1.0832.
    #[test]
    fn a_kilo_of_sugar_in_four_litres_is_twenty_plato() {
        let og = og_from_sugar(g(1000.0), l(4.0)).unwrap();
        assert!((sg_to_plato(og) - 20.03).abs() < 0.05, "{og}");
        assert!((og.get() - 1.0834).abs() < 0.0010, "{og}");
    }

    #[test]
    fn sugar_and_gravity_round_trip() {
        for target in [1.020, 1.040, 1.075, 1.100] {
            let sugar = sugar_for_og(sg(target), l(20.0)).unwrap();
            let back = og_from_sugar(sugar, l(20.0)).unwrap();
            assert!((back.get() - target).abs() < 1e-9, "{target} -> {back}");
        }
    }

    /// Water is not 1.000 kg/L, and pretending it is would shift a 20 °P wash by
    /// 0.04 °P. Small, but the doc claims the correction is applied, so it is pinned.
    #[test]
    fn water_density_is_applied_rather_than_assumed_to_be_one() {
        let with = og_from_sugar(g(1000.0), l(4.0)).unwrap();
        let naive = plato_to_sg(100.0 * 1000.0 / (1000.0 + 4000.0)).unwrap();
        assert!(with.get() > naive.get());
        assert!((sg_to_plato(with) - 100.0 * 1000.0 / 5000.0).abs() < 0.1);
    }

    /// Equal volumes of 1.060 and water land on 1.030 — the answer the naive
    /// gravity-point average also gives, which is why blending looks easy right up
    /// until the volumes differ.
    #[test]
    fn equal_volumes_of_wort_and_water_halve_the_gravity() {
        let (v, s) = blend(&[(l(10.0), sg(1.060)), (l(10.0), sg(1.000))]).unwrap();
        assert!((v.get() - 20.0).abs() < 1e-12);
        assert!((s.get() - 1.030).abs() < 0.0015, "{s}");
    }

    /// Extract mass is what the function conserves, so it is what the test asserts —
    /// not the gravity, which is only the shape that mass takes.
    #[test]
    fn blending_conserves_extract_mass() {
        let parts = [
            (l(12.0), sg(1.075)),
            (l(3.0), sg(1.010)),
            (l(5.0), sg(1.000)),
        ];
        let before: f64 = parts
            .iter()
            .map(|&(v, s)| v.get() * s.get() * 1000.0 * sg_to_plato(s) / 100.0)
            .sum();
        let (v, s) = blend(&parts).unwrap();
        let after = v.get() * s.get() * 1000.0 * sg_to_plato(s) / 100.0;
        // Within 0.5 %, the volume-additivity band the doc states.
        assert!(
            (after - before).abs() / before < 0.005,
            "{before} -> {after}"
        );
    }

    #[test]
    fn dilution_is_blending_with_water() {
        let by_dilute = dilute(l(10.0), sg(1.060), l(10.0)).unwrap();
        let by_blend = blend(&[(l(10.0), sg(1.060)), (l(10.0), sg(1.000))])
            .unwrap()
            .1;
        assert_eq!(by_dilute, by_blend);
    }

    /// A single part is itself, to the round-trip tolerance. If this drifted, the
    /// mass bookkeeping would be wrong and every multi-part blend with it.
    #[test]
    fn a_blend_of_one_part_is_that_part() {
        let (v, s) = blend(&[(l(23.0), sg(1.048))]).unwrap();
        assert!((v.get() - 23.0).abs() < 1e-12);
        assert!((s.get() - 1.048).abs() < 1e-9, "{s}");
    }

    #[test]
    fn nonsense_inputs_are_refused() {
        assert_eq!(blend(&[]).unwrap_err().code(), "calc.out_of_range");
        assert_eq!(
            blend(&[(l(0.0), sg(1.050))]).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            og_from_sugar(g(-1.0), l(4.0)).unwrap_err().code(),
            "calc.out_of_range"
        );
        // Below water there is no sugar to add.
        assert_eq!(
            sugar_for_og(sg(0.995), l(20.0)).unwrap_err().code(),
            "calc.out_of_range"
        );
        // 5 kg of sugar in 100 mL is off the top of the Plato band, not a wash.
        assert_eq!(
            og_from_sugar(g(5000.0), l(0.1)).unwrap_err().code(),
            "calc.out_of_range"
        );
    }
}
