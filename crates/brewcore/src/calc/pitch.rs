//! Pitch rate — how many cells, and how many grams that is.
//!
//! ```text
//! cells = rate · 10⁶ · volume_mL · °P
//! grams = cells / (cells_per_gram · viability)
//! ```
//!
//! The first line is a definition once the rate is chosen; all the empiricism is in
//! the rate itself, and in `cells_per_gram`, which is a manufacturer's spec rather
//! than a measurement anyone here can make.
//!
//! Nothing in this module reads the database. `yeast_strain` has no cells-per-gram,
//! viability or pack-size column and is not getting one in Phase 3 — the numbers are
//! arguments with documented defaults, and a caller decides which default applies.
//! `YeastForm`'s doc comment says this decides the pitch-rate maths, and it does, at
//! the call site: a `calc` function matching on a `db` enum would be the first thing
//! here to import from the layer above it.

use super::gravity::sg_to_plato;
use crate::{
    Error, Result,
    units::{Grams, Litres, Sg},
};

/// Million cells per mL per °P for a clean ale ferment.
///
/// The figures in Chris White and Jamil Zainasheff, *Yeast: The Practical Guide to
/// Beer Fermentation* (Brewers Publications, 2010), ch. 7. Half this is a deliberate
/// under-pitch for ester production; double is a lager.
pub const RATE_ALE: f64 = 0.75;

/// Million cells per mL per °P for a lager. Same source; the cold ferment needs
/// roughly twice the ale count.
pub const RATE_LAGER: f64 = 1.5;

/// Viable cells per gram of active dry yeast.
///
/// The floor both major dry-yeast houses guarantee on the packet — Fermentis and
/// Lallemand each specify ≥ 6 × 10⁹ CFU/g at manufacture. Fresh yeast usually assays
/// higher; using the spec floor over-pitches slightly, which is the safe direction.
pub const DRY_CELLS_PER_GRAM: f64 = 6.0e9;

/// The band a viability fraction has to sit in. Open at the bottom: a zero-viability
/// pitch needs infinite yeast, which is a division by zero dressed up as an answer.
pub const VALID_VIABILITY: std::ops::Range<f64> = 0.0..1.000_001;

/// Rates outside this are not pitch rates. 0.35 is a heavy under-pitch, 2.0 a
/// double lager pitch.
pub const VALID_RATE: std::ops::RangeInclusive<f64> = 0.1..=3.0;

/// The cell count a batch wants, from its volume and starting gravity.
///
/// `rate` is in **million cells per mL per °P** — the unit every source quotes and
/// nobody writes down. The return is plain cells, so 20 L of 12 °P at [`RATE_ALE`] is
/// 1.8 × 10¹¹.
pub fn cells_needed(volume: Litres, gravity: Sg, rate: f64) -> Result<f64> {
    if volume.get() <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "batch volume for a pitch rate",
            value: volume.get(),
        });
    }
    if !rate.is_finite() {
        return Err(Error::NotFinite {
            field: "pitch rate",
            value: rate,
        });
    }
    if !VALID_RATE.contains(&rate) {
        return Err(Error::CalcOutOfRange {
            what: "pitch rate",
            value: rate,
        });
    }
    let plato = sg_to_plato(gravity);
    if plato <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "original gravity for a pitch rate",
            value: gravity.get(),
        });
    }
    Ok(rate * 1.0e6 * volume.get() * 1000.0 * plato)
}

/// Grams of dry yeast for a cell count, given the packet's spec and its viability.
///
/// `viability` is a fraction — `1.0` for a fresh packet, `0.8` for one a year past
/// its date at the usual ~20 % per year decline. Nothing defaults it: a caller that
/// does not know the packet's age should pass `1.0` and say so on screen, rather than
/// inherit a guess from here.
pub fn dry_yeast_grams(cells: f64, cells_per_gram: f64, viability: f64) -> Result<Grams> {
    for (field, value) in [("cell count", cells), ("viability", viability)] {
        if !value.is_finite() {
            return Err(Error::NotFinite { field, value });
        }
    }
    if cells < 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "cell count",
            value: cells,
        });
    }
    if !VALID_VIABILITY.contains(&viability) || viability <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "viability",
            value: viability,
        });
    }
    if !(cells_per_gram.is_finite() && cells_per_gram > 0.0) {
        return Err(Error::CalcOutOfRange {
            what: "cells per gram",
            value: cells_per_gram,
        });
    }
    Grams::new(cells / (cells_per_gram * viability))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(v: f64) -> Litres {
        Litres::new(v).unwrap()
    }
    fn sg(v: f64) -> Sg {
        Sg::new(v).unwrap()
    }

    /// The worked example from *Yeast*, ch. 7: 20 L of 12 °P ale wort at 0.75
    /// million cells/mL/°P is 180 billion cells, which is 30 g of dry yeast at spec
    /// density — about two and a half 11.5 g packets.
    #[test]
    fn twenty_litres_of_twelve_plato_ale_wants_a_hundred_and_eighty_billion_cells() {
        // 1.048 is 11.91 °P, so pin the definition on the °P rather than the SG.
        let cells = RATE_ALE * 1.0e6 * 20_000.0 * 12.0;
        assert!((cells - 1.8e11).abs() < 1.0);
        let grams = dry_yeast_grams(cells, DRY_CELLS_PER_GRAM, 1.0).unwrap();
        assert!((grams.get() - 30.0).abs() < 0.01, "{grams}");
    }

    /// The same batch through the public path, where the gravity does the °P
    /// conversion. 1.048 is 11.91 °P, so this lands just under the round figure.
    #[test]
    fn the_gravity_path_agrees_with_the_plato_figure() {
        let cells = cells_needed(l(20.0), sg(1.048), RATE_ALE).unwrap();
        assert!((cells - 1.787e11).abs() < 2.0e8, "{cells}");
        let grams = dry_yeast_grams(cells, DRY_CELLS_PER_GRAM, 1.0).unwrap();
        assert!((grams.get() - 29.8).abs() < 0.1, "{grams}");
    }

    /// A lager wants twice an ale's cells, which is the whole content of the two
    /// constants and worth pinning so a future edit cannot quietly swap them.
    #[test]
    fn a_lager_pitch_is_twice_an_ale_pitch() {
        let ale = cells_needed(l(20.0), sg(1.048), RATE_ALE).unwrap();
        let lager = cells_needed(l(20.0), sg(1.048), RATE_LAGER).unwrap();
        assert!((lager / ale - 2.0).abs() < 1e-12);
    }

    /// Stale yeast needs proportionally more of it.
    #[test]
    fn half_viable_yeast_takes_twice_the_grams() {
        let full = dry_yeast_grams(1.8e11, DRY_CELLS_PER_GRAM, 1.0).unwrap();
        let half = dry_yeast_grams(1.8e11, DRY_CELLS_PER_GRAM, 0.5).unwrap();
        assert!((half.get() / full.get() - 2.0).abs() < 1e-12);
    }

    /// Zero viability is a division by zero wearing an answer's clothes.
    #[test]
    fn a_dead_packet_is_refused_rather_than_returning_infinity() {
        assert_eq!(
            dry_yeast_grams(1.8e11, DRY_CELLS_PER_GRAM, 0.0)
                .unwrap_err()
                .code(),
            "calc.out_of_range"
        );
        assert_eq!(
            dry_yeast_grams(1.8e11, 0.0, 1.0).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            dry_yeast_grams(1.8e11, DRY_CELLS_PER_GRAM, 1.5)
                .unwrap_err()
                .code(),
            "calc.out_of_range"
        );
    }

    #[test]
    fn a_rate_or_volume_outside_the_band_is_refused() {
        assert_eq!(
            cells_needed(l(20.0), sg(1.048), 12.0).unwrap_err().code(),
            "calc.out_of_range"
        );
        assert_eq!(
            cells_needed(l(0.0), sg(1.048), RATE_ALE)
                .unwrap_err()
                .code(),
            "calc.out_of_range"
        );
        // Water has nothing to ferment, so it has no pitch rate.
        assert_eq!(
            cells_needed(l(20.0), sg(1.000), RATE_ALE)
                .unwrap_err()
                .code(),
            "calc.out_of_range"
        );
    }
}
