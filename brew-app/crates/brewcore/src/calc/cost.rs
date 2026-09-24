//! Rolling up what a batch cost.
//!
//! The one module here that is not a fit of anything — it is exact integer money
//! arithmetic, and it lives in `calc` rather than `db` so that the folding can be
//! tested without a database. `db::batch::batch_cost` does the join and hands the
//! lines over.
//!
//! The awkward part is the per-unit price, and it is awkward for the reason
//! `db::reference::LotRow::cost_per_canonical_unit` documents: €12.50 for 5 kg of
//! sucrose is 0.25 minor units per gram, and any integer type rounds that to zero.
//! So the lines carry an `f64` of minor units, the fold accumulates in `f64`, and the
//! rounding to a real [`Money`] happens **once**, at the end — the same rule
//! [`Money::checked_mul_f64`] follows for a single line.

use crate::{
    Error, Result,
    units::{Currency, Litres, Money},
};

/// One `batch_ingredient` row priced against its lot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostLine {
    /// Minor units per gram / litre / unit — what
    /// `LotRow::cost_per_canonical_unit` returns.
    pub per_unit_minor: f64,
    pub currency: Currency,
    /// The canonical quantity used, in the dimension `per_unit_minor` is priced in.
    pub qty: f64,
}

/// What a batch cost, and how much of it is actually known.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct CostTotal {
    /// `None` when no line carried a price at all — distinct from `Some(zero)`,
    /// which means the lots were free.
    pub total: Option<Money>,
    pub lines_costed: usize,
    /// Additions whose lot has no price on it. Reported rather than swallowed: a
    /// total that quietly omits three of a batch's eight lots is worse than no total,
    /// because it looks like one.
    pub lines_without_cost: usize,
}

/// Fold priced lines into a total.
///
/// `lines_without_cost` is the caller's to supply — it knows which rows it could not
/// price, and this function only ever sees the ones it could.
///
/// Mixing currencies is [`Error::CurrencyMismatch`], never a coercion.
/// `units::money` sets that policy and this is the first place in the app that could
/// have been tempted to break it: a brewery that buys yeast in USD and sugar in EUR
/// has no cost per batch until someone decides on a rate, and inventing one here
/// would produce a plausible wrong number rather than a loud failure.
pub fn total(
    lines: impl IntoIterator<Item = CostLine>,
    lines_without_cost: usize,
) -> Result<CostTotal> {
    let mut currency: Option<Currency> = None;
    let mut minor = 0.0f64;
    let mut lines_costed = 0usize;

    for line in lines {
        for (field, value) in [("unit cost", line.per_unit_minor), ("quantity", line.qty)] {
            if !value.is_finite() {
                return Err(Error::NotFinite { field, value });
            }
        }
        match currency {
            None => currency = Some(line.currency),
            Some(seen) if seen != line.currency => {
                return Err(Error::CurrencyMismatch {
                    left: seen,
                    right: line.currency,
                });
            }
            Some(_) => {}
        }
        minor += line.per_unit_minor * line.qty;
        lines_costed += 1;
    }

    let total = match currency {
        // Round once, here. `checked_mul_f64` on a one-minor-unit base is the
        // crate's only bounds-checked f64 → minor conversion, so it is reused rather
        // than re-implementing its 2^63 guard a second way.
        Some(c) => Some(Money::new(1, c).checked_mul_f64(minor)?),
        None => None,
    };
    Ok(CostTotal {
        total,
        lines_costed,
        lines_without_cost,
    })
}

/// Cost per litre. Rounds to whole minor units, so it is a display figure: at 20 L a
/// single cent of rounding is 20 cents of implied total, which is why
/// [`CostTotal::total`] is the number to sum and this one is the number to print.
pub fn per_litre(total: Money, volume: Litres) -> Result<Money> {
    if volume.get() <= 0.0 {
        return Err(Error::CalcOutOfRange {
            what: "batch volume for a cost per litre",
            value: volume.get(),
        });
    }
    total.checked_mul_f64(1.0 / volume.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(per_unit_minor: f64, qty: f64) -> CostLine {
        CostLine {
            per_unit_minor,
            currency: Currency::EUR,
            qty,
        }
    }

    /// The worked example the whole `f64` detour exists for: €12.50 for 5 kg of
    /// sucrose is 0.25 minor units per gram. 4 kg of it is €10.00 — and would be
    /// €0.00 if the per-unit price had been rounded to an integer first.
    #[test]
    fn a_sub_cent_unit_price_still_totals_correctly() {
        let per_gram = 1250.0 / 5000.0;
        let out = total([line(per_gram, 4000.0)], 0).unwrap();
        assert_eq!(out.total.unwrap(), Money::new(1000, Currency::EUR));
        assert_eq!(out.lines_costed, 1);
    }

    /// Rounding once at the end, not per line. Three lines of 0.333… minor units
    /// are 1 minor unit together and 0 apiece.
    #[test]
    fn rounding_happens_once_at_the_end() {
        let out = total([line(0.334, 1.0), line(0.333, 1.0), line(0.333, 1.0)], 0).unwrap();
        assert_eq!(out.total.unwrap().minor, 1);
    }

    #[test]
    fn a_realistic_batch_adds_up() {
        let out = total(
            [
                line(1250.0 / 5000.0, 4200.0), // sugar
                line(480.0 / 11.5, 11.5),      // a yeast packet
                line(900.0 / 500.0, 25.0),     // nutrient
            ],
            0,
        )
        .unwrap();
        assert_eq!(
            out.total.unwrap(),
            Money::new(1050 + 480 + 45, Currency::EUR)
        );
        assert_eq!(out.lines_without_cost, 0);
    }

    #[test]
    fn mixed_currencies_fail_loudly_rather_than_summing() {
        let usd = CostLine {
            per_unit_minor: 1.0,
            currency: Currency::USD,
            qty: 100.0,
        };
        let err = total([line(1.0, 100.0), usd], 0).unwrap_err();
        assert_eq!(err.code(), "money.currency_mismatch");
    }

    /// No priced lines is `None`, not zero: a batch whose lots have no prices on
    /// them did not cost nothing.
    #[test]
    fn no_priced_lines_is_none_rather_than_zero() {
        let out = total(std::iter::empty(), 3).unwrap();
        assert!(out.total.is_none());
        assert_eq!(out.lines_costed, 0);
        assert_eq!(out.lines_without_cost, 3);
        // A genuinely free lot is `Some(zero)`, and the two must not collapse.
        let free = total([line(0.0, 100.0)], 0).unwrap();
        assert_eq!(free.total, Some(Money::zero(Currency::EUR)));
    }

    #[test]
    fn cost_per_litre_divides_and_refuses_an_empty_fermenter() {
        let t = Money::new(1575, Currency::EUR);
        assert_eq!(
            per_litre(t, Litres::new(20.0).unwrap()).unwrap(),
            Money::new(79, Currency::EUR) // 78.75, rounded
        );
        assert_eq!(
            per_litre(t, Litres::new(0.0).unwrap()).unwrap_err().code(),
            "calc.out_of_range"
        );
    }

    #[test]
    fn a_non_finite_line_is_refused() {
        assert_eq!(
            total([line(f64::NAN, 1.0)], 0).unwrap_err().code(),
            "units.not_finite"
        );
        assert_eq!(
            total([line(1.0, f64::INFINITY)], 0).unwrap_err().code(),
            "units.not_finite"
        );
    }
}
