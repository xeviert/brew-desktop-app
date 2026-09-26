//! Yeast assimilable nitrogen, and the staggered schedule that delivers it.
//!
//! A malt wort brings its own nitrogen. A sugar wash brings **none** — sucrose is
//! carbon and nothing else — so for the product this app exists for, yeast health is
//! entirely a nutrient-schedule problem, and the schedule is the process variable
//! most likely to explain a batch going wrong. That is why 3b models it as
//! `recipe_item` rows with `offset_hours` rather than a free-text note, and why the
//! arithmetic is here rather than in someone's head.
//!
//! The chain, all of it linear:
//!
//! ```text
//! sugar g/L = °Bx · 10 · SG
//! YAN ppm   = sugar g/L · factor       mg N per gram of sugar
//! YAN mg    = YAN ppm · volume_L
//! product g = YAN mg / mg_yan_per_g
//! ```
//!
//! The factor and the product yield are both arguments. Neither defaults, for the
//! reason [`super::refractometer::WORT_CORRECTION_DEFAULT`] does not: the right value
//! for a seltzer is not the right value for anything else, and a wrong default here
//! is a stuck ferment rather than a rounding error.

use super::gravity::sg_to_brix;
use crate::{
    Error, Result,
    units::{Grams, Litres, Sg},
};

/// mg of nitrogen wanted per gram of sugar, for a base that supplies some of its own.
///
/// The three demand classes are the winemaking convention (Zoecklein et al.,
/// *Wine Analysis and Production*, Chapman & Hall 1995, ch. 6), where a 24 °Bx must
/// at the medium factor works out near 200 ppm YAN — the number every reference
/// quotes for a healthy ferment.
pub const DEMAND_LOW: f64 = 0.75;

/// Medium nitrogen demand. The default a grape must would use.
pub const DEMAND_MEDIUM: f64 = 0.90;

/// High nitrogen demand — **this is the sugar-wash case**, and the reason the module
/// exists. A sucrose base contributes zero YAN, so every milligram the yeast gets is
/// one that was added.
pub const DEMAND_HIGH: f64 = 1.25;

/// mg YAN per gram of diammonium phosphate.
///
/// Stoichiometric, not a datasheet figure: (NH₄)₂HPO₄ is 132.06 g/mol carrying
/// 2 × 14.007 g of nitrogen, so 21.21 % N by mass. The only exact number in this
/// module.
pub const DAP_MG_YAN_PER_G: f64 = 212.0;

/// mg YAN per gram of Fermaid K. Lallemand/Scott Laboratories datasheet: 100 ppm YAN
/// per g/L, which is the same statement.
pub const FERMAID_K_MG_YAN_PER_G: f64 = 100.0;

/// mg YAN per gram of Fermaid O.
///
/// **Contested, and used with the datasheet figure.** Scott Laboratories quotes
/// ~40 mg N/L at a 25 g/hL addition, which is 160 mg per gram; the mead calculators
/// in the TOSNA lineage assume 40 mg per gram, a factor of four lower. The
/// disagreement is real rather than a typo — organic nitrogen is not assayed the same
/// way as ammoniacal nitrogen, and the two figures are measuring different things.
/// The supplier's number is the one implemented, and a caller that wants TOSNA's
/// should pass 40.0 explicitly.
pub const FERMAID_O_MG_YAN_PER_G: f64 = 160.0;

/// The TOSNA schedule: four equal doses, at 24 h, 48 h, 72 h and the ⅓ sugar break.
///
/// Fractions rather than a schedule, because the *timing* belongs to
/// `recipe_item.offset_hours` and this module has no business owning it.
pub const TOSNA_DOSES: [f64; 4] = [0.25, 0.25, 0.25, 0.25];

/// The least sugar, in °Bx, that counts as something to feed yeast on.
const MIN_BRIX: f64 = 0.1;

/// Grams of sugar per litre, from gravity.
///
/// °Bx is percent extract by mass, so `g/L = °Bx · 10 · SG` is definitional once the
/// scale conversion is done — and that conversion is [`sg_to_brix`], the fit.
pub fn sugar_per_litre(gravity: Sg) -> Result<f64> {
    let brix = sg_to_brix(gravity);
    // Not `<= 0.0`: the cubic's residual puts water at ±1e-12 °Bx rather than
    // exactly zero, so a plain sign test lets 1.000 through as a fermentable.
    // A tenth of a °Bx is under half a gravity point — nothing to feed yeast on.
    if brix < MIN_BRIX {
        return Err(Error::CalcOutOfRange {
            what: "gravity for a nitrogen target",
            value: gravity.get(),
        });
    }
    Ok(brix * 10.0 * gravity.get())
}

/// The YAN target in ppm (mg/L), from gravity and a demand factor.
pub fn target_ppm(gravity: Sg, factor: f64) -> Result<f64> {
    if !factor.is_finite() {
        return Err(Error::NotFinite {
            field: "nitrogen demand factor",
            value: factor,
        });
    }
    if !VALID_DEMAND.contains(&factor) {
        return Err(Error::CalcOutOfRange {
            what: "nitrogen demand factor",
            value: factor,
        });
    }
    Ok(sugar_per_litre(gravity)? * factor)
}

/// Demand factors outside this are not demand factors. The published classes span
/// 0.75–1.25; the band is widened a little either side for a base nobody has
/// characterised yet.
pub const VALID_DEMAND: std::ops::RangeInclusive<f64> = 0.5..=2.0;

/// Total milligrams of nitrogen for a batch — `ppm × litres`.
pub fn total_mg(target_ppm: f64, volume: Litres) -> Result<f64> {
    if !target_ppm.is_finite() {
        return Err(Error::NotFinite {
            field: "yan target",
            value: target_ppm,
        });
    }
    if target_ppm < 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "yan target",
            value: target_ppm,
        });
    }
    if volume.get() <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "batch volume for a nitrogen target",
            value: volume.get(),
        });
    }
    Ok(target_ppm * volume.get())
}

/// Grams of a nutrient product supplying `yan_mg` of nitrogen.
///
/// No volume argument, and deliberately: `ppm × litres / (mg per g per litre)` cancels
/// the volume out. A signature that took it would imply a dependence that is not
/// there, and would be one more thing to pass wrong.
pub fn product_grams(yan_mg: f64, mg_yan_per_g: f64) -> Result<Grams> {
    if !yan_mg.is_finite() {
        return Err(Error::NotFinite {
            field: "yan",
            value: yan_mg,
        });
    }
    if yan_mg < 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "yan",
            value: yan_mg,
        });
    }
    if !(mg_yan_per_g.is_finite() && mg_yan_per_g > 0.0) {
        return Err(Error::CalcOutOfRange {
            what: "nitrogen yield per gram",
            value: mg_yan_per_g,
        });
    }
    Grams::new(yan_mg / mg_yan_per_g)
}

/// Milligrams of nitrogen that `grams` of a product supplied — [`product_grams`]
/// the other way round, for 5a's accounting of what actually went in.
pub fn supplied_mg(grams: Grams, mg_yan_per_g: f64) -> Result<f64> {
    if grams.get() < 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "nutrient mass",
            value: grams.get(),
        });
    }
    if !(mg_yan_per_g.is_finite() && mg_yan_per_g > 0.0) {
        return Err(Error::CalcOutOfRange {
            what: "nitrogen yield per gram",
            value: mg_yan_per_g,
        });
    }
    Ok(grams.get() * mg_yan_per_g)
}

/// Split a total across fractional doses.
///
/// Takes the fractions rather than a dose count so an uneven schedule — a larger
/// first addition, a token last one — is expressible. They must sum to 1.0 within
/// 1e-6: a set summing to 0.9 would silently under-feed by a tenth, which looks
/// exactly like a nitrogen-limited ferment and would be diagnosed as one.
///
/// Pair the output with `recipe_item.offset_hours`; see [`TOSNA_DOSES`].
pub fn stagger(total: f64, doses: &[f64]) -> Result<Vec<f64>> {
    if !total.is_finite() {
        return Err(Error::NotFinite {
            field: "yan",
            value: total,
        });
    }
    if doses.is_empty() {
        return Err(Error::CalcOutOfRange {
            what: "number of nutrient doses",
            value: 0.0,
        });
    }
    let mut sum = 0.0;
    for &d in doses {
        if !d.is_finite() {
            return Err(Error::NotFinite {
                field: "nutrient dose fraction",
                value: d,
            });
        }
        if d < 0.0 {
            return Err(Error::CalcOutOfRange {
                what: "nutrient dose fraction",
                value: d,
            });
        }
        sum += d;
    }
    if (sum - 1.0).abs() > 1e-6 {
        return Err(Error::CalcOutOfRange {
            what: "nutrient dose fractions, which must sum to one",
            value: sum,
        });
    }
    Ok(doses.iter().map(|d| total * d).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }
    fn l(v: f64) -> Litres {
        Litres::new(v).unwrap()
    }

    /// The reconciliation the demand constants rest on: a 24 °Bx must at
    /// [`DEMAND_MEDIUM`] should land near the 200–250 ppm every winemaking reference
    /// quotes. If it did not, the factors would be in the wrong unit.
    #[test]
    fn a_twenty_four_brix_must_at_medium_demand_wants_about_two_hundred_ppm() {
        // 1.1010 is ~24 °Bx.
        let must = sg(1.1010);
        let brix = sg_to_brix(must);
        assert!((brix - 24.0).abs() < 0.2, "{brix}");
        let ppm = target_ppm(must, DEMAND_MEDIUM).unwrap();
        assert!((200.0..250.0).contains(&ppm), "{ppm}");
    }

    /// The seltzer case, end to end. A 1.075 wash is ~18.2 °Bx, so ~196 g/L of
    /// sugar; at high demand that is ~245 ppm YAN, and 20 L of it is ~4.9 g of N —
    /// about 23 g of DAP, or four TOSNA doses of ~5.8 g.
    #[test]
    fn a_twenty_litre_seltzer_wash_wants_about_twenty_three_grams_of_dap() {
        let wash = sg(1.075);
        let per_litre = sugar_per_litre(wash).unwrap();
        assert!((per_litre - 195.6).abs() < 2.0, "{per_litre}");

        let ppm = target_ppm(wash, DEMAND_HIGH).unwrap();
        assert!((ppm - 244.5).abs() < 3.0, "{ppm}");

        let mg = total_mg(ppm, l(20.0)).unwrap();
        assert!((mg - 4890.0).abs() < 60.0, "{mg}");

        let dap = product_grams(mg, DAP_MG_YAN_PER_G).unwrap();
        assert!((dap.get() - 23.1).abs() < 0.3, "{dap}");

        let doses = stagger(dap.get(), &TOSNA_DOSES).unwrap();
        assert_eq!(doses.len(), 4);
        assert!((doses[0] - 5.77).abs() < 0.1, "{:?}", doses);
        assert!((doses.iter().sum::<f64>() - dap.get()).abs() < 1e-9);
    }

    /// DAP is stoichiometric and therefore the only product figure that can be
    /// checked rather than cited: 132.06 g/mol carrying 28.014 g of N.
    #[test]
    fn the_dap_yield_is_its_nitrogen_mass_fraction() {
        let from_chemistry = 1000.0 * (2.0 * 14.007) / 132.06;
        assert!(
            (DAP_MG_YAN_PER_G - from_chemistry).abs() < 1.0,
            "{from_chemistry}"
        );
    }

    /// The same nitrogen from different products is different masses of product —
    /// which is the whole reason the yield is an argument and not a constant.
    #[test]
    fn the_same_nitrogen_takes_more_of_a_weaker_product() {
        let dap = product_grams(5000.0, DAP_MG_YAN_PER_G).unwrap();
        let k = product_grams(5000.0, FERMAID_K_MG_YAN_PER_G).unwrap();
        let tosna_o = product_grams(5000.0, 40.0).unwrap();
        assert!(k.get() > dap.get());
        assert!(tosna_o.get() > k.get());
        assert!((k.get() / dap.get() - 2.12).abs() < 0.01);
    }

    #[test]
    fn supplied_is_product_grams_inverted() {
        let dap = product_grams(4890.0, DAP_MG_YAN_PER_G).unwrap();
        let back = supplied_mg(dap, DAP_MG_YAN_PER_G).unwrap();
        assert!((back - 4890.0).abs() < 1e-9, "{back}");
        let g = Grams::new(10.0).unwrap();
        assert_eq!(supplied_mg(g, 0.0).unwrap_err().code(), "calc.out_of_range");
        assert_eq!(
            supplied_mg(Grams::new(-1.0).unwrap(), DAP_MG_YAN_PER_G)
                .unwrap_err()
                .code(),
            "calc.out_of_range"
        );
    }

    /// An uneven schedule is legal; one that does not add up is not.
    #[test]
    fn dose_fractions_must_sum_to_one() {
        let uneven = stagger(100.0, &[0.4, 0.3, 0.2, 0.1]).unwrap();
        assert_eq!(uneven, vec![40.0, 30.0, 20.0, 10.0]);
        assert_eq!(
            stagger(100.0, &[0.25, 0.25, 0.25]).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(stagger(100.0, &[]).unwrap_err().code(), "calc.out_of_range");
        assert_eq!(
            stagger(100.0, &[1.5, -0.5]).unwrap_err().code(),
            "calc.out_of_range"
        );
    }

    #[test]
    fn nonsense_inputs_are_refused() {
        assert_eq!(
            target_ppm(sg(1.000), DEMAND_HIGH).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            target_ppm(sg(1.075), 12.0).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            total_mg(200.0, l(0.0)).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            product_grams(5000.0, 0.0).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            target_ppm(sg(1.075), f64::NAN).unwrap_err().code(),
            "units.not_finite"
        );
    }
}
