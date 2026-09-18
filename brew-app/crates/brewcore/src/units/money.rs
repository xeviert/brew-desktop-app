//! Money: integer minor units plus an ISO 4217 code.
//!
//! # Why not a float
//!
//! 0.1 + 0.2 is not 0.3 in binary floating point, and a cost-per-batch column is
//! summed over dozens of lots. Minor units (cents, yen, fils) are exact integers,
//! so the arithmetic is exact and the rounding happens once, at display.
//!
//! # Why a currency column and not EUR-only
//!
//! Brewing ingredients get bought abroad. Carrying the code costs one `TEXT`
//! column; adding it later costs a migration over every row of history.
//!
//! # What this deliberately does not do
//!
//! There is no FX conversion. Real multi-currency means stored rates with
//! effective dates and a policy for which rate a two-year-old lot uses — not worth
//! building for one site. So adding EUR to USD is an [`Error::CurrencyMismatch`],
//! not a guess: a mixed-currency sum becomes a loud failure instead of a plausible
//! wrong number.

use crate::{Error, Result};

/// An ISO 4217 alphabetic code.
///
/// A newtype over three bytes rather than an enum of ~180 variants: the app never
/// branches on which currency it is, only on whether two of them match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    pub const EUR: Self = Self(*b"EUR");
    pub const USD: Self = Self(*b"USD");
    pub const GBP: Self = Self(*b"GBP");

    /// Accepts any three ASCII letters, case-insensitively. Validating against the
    /// real ISO list would mean shipping and maintaining that list to catch a typo
    /// the supplier field would show anyway.
    pub fn new(code: &str) -> Result<Self> {
        let code = code.trim();
        let bytes = code.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(|b| b.is_ascii_alphabetic()) {
            return Err(Error::InvalidCurrency(code.to_owned()));
        }
        let mut out = [0u8; 3];
        for (slot, b) in out.iter_mut().zip(bytes) {
            *slot = b.to_ascii_uppercase();
        }
        Ok(Self(out))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("constructors only admit ASCII letters")
    }

    /// Digits after the decimal point. ISO 4217 says most currencies are 2, and the
    /// exceptions are few enough to list; anything unlisted gets 2.
    pub fn exponent(&self) -> u32 {
        match &self.0 {
            b"BIF" | b"CLP" | b"DJF" | b"GNF" | b"ISK" | b"JPY" | b"KMF" | b"KRW" | b"PYG"
            | b"RWF" | b"UGX" | b"VND" | b"VUV" | b"XAF" | b"XOF" | b"XPF" => 0,
            b"BHD" | b"IQD" | b"JOD" | b"KWD" | b"LYD" | b"OMR" | b"TND" => 3,
            _ => 2,
        }
    }

    /// How many minor units are in one major unit.
    fn scale(&self) -> i64 {
        10i64.pow(self.exponent())
    }
}

impl Default for Currency {
    fn default() -> Self {
        Self::EUR
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Currency {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Currency::new(s)
    }
}

impl serde::Serialize for Currency {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Currency {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        let code = String::deserialize(de)?;
        Currency::new(&code).map_err(serde::de::Error::custom)
    }
}

impl rusqlite::ToSql for Currency {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl rusqlite::types::FromSql for Currency {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Currency::new(text).map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

/// An amount, stored as `<name>_minor INTEGER` plus a `currency TEXT` sibling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Money {
    pub minor: i64,
    pub currency: Currency,
}

impl Money {
    pub const fn new(minor: i64, currency: Currency) -> Self {
        Self { minor, currency }
    }

    pub const fn zero(currency: Currency) -> Self {
        Self::new(0, currency)
    }

    /// Parses what a human types: `12,50` and `12.50` are both 1250 in EUR.
    ///
    /// Two separator classes, and the split is by ambiguity rather than by taste:
    ///
    /// - **`,` and `.` are always the decimal point**, never a thousands grouping.
    ///   Either one means a decimal point *somewhere* in the world, so `1,234` in
    ///   a two-decimal currency is an error rather than a silent factor-of-1000
    ///   mistake. Write `1234` if that is what you meant.
    /// - **Whitespace and `_` are always grouping** and are stripped, so `1 234,56`
    ///   is 123456 minor units. No locale writes a decimal space, so there is
    ///   nothing to misread — and a French keyboard is what this brewery types on.
    ///   `char::is_whitespace` covers U+00A0 and U+202F, which is what fr-FR
    ///   formatting and a paste out of a spreadsheet actually produce.
    pub fn parse(input: &str, currency: Currency) -> Result<Self> {
        let cleaned: String = input
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '_')
            .collect();
        let invalid = || Error::InvalidMoney(input.to_owned());

        let (sign, rest) = match cleaned.strip_prefix('-') {
            Some(rest) => (-1i64, rest),
            None => (1i64, cleaned.strip_prefix('+').unwrap_or(cleaned.as_str())),
        };
        if rest.is_empty() {
            return Err(invalid());
        }

        let (whole, frac) = match rest.rfind([',', '.']) {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, ""),
        };
        // `,50` is a legitimate shorthand, but a separator with digits on neither
        // side is not an amount at all.
        if whole.is_empty() && frac.is_empty() {
            return Err(invalid());
        }
        let whole = if whole.is_empty() { "0" } else { whole };
        if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }

        let exponent = currency.exponent() as usize;
        if frac.len() > exponent {
            return Err(invalid());
        }

        let major: i64 = whole.parse().map_err(|_| invalid())?;
        let minor_part: i64 = if exponent == 0 {
            0
        } else {
            format!("{frac:0<exponent$}")
                .parse()
                .map_err(|_| invalid())?
        };
        let minor = major
            .checked_mul(currency.scale())
            .and_then(|m| m.checked_add(minor_part))
            .and_then(|m| m.checked_mul(sign))
            .ok_or(Error::MoneyOverflow)?;
        Ok(Self::new(minor, currency))
    }

    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.expect_same_currency(other)?;
        let minor = self
            .minor
            .checked_add(other.minor)
            .ok_or(Error::MoneyOverflow)?;
        Ok(Self::new(minor, self.currency))
    }

    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.expect_same_currency(other)?;
        let minor = self
            .minor
            .checked_sub(other.minor)
            .ok_or(Error::MoneyOverflow)?;
        Ok(Self::new(minor, self.currency))
    }

    /// Scaling by a quantity — 2.5 kg at €3.20/kg. Rounds once, at the end.
    pub fn checked_mul_f64(self, factor: f64) -> Result<Self> {
        if !factor.is_finite() {
            return Err(Error::NotFinite {
                field: "money factor",
                value: factor,
            });
        }
        let scaled = (self.minor as f64) * factor;
        let rounded = scaled.round();
        // `i64::MAX as f64` rounds *up* to 2^63, so comparing against it lets
        // exactly 2^63 through, and `as i64` then saturates to `i64::MAX` — an
        // overflow that reports a plausible number instead of an error. Bound on
        // 2^63 itself: `i64::MIN` is exactly -2^63 and legal, `i64::MAX` is not.
        const LIMIT: f64 = 9_223_372_036_854_775_808.0; // 2^63
        if !(-LIMIT..LIMIT).contains(&rounded) {
            return Err(Error::MoneyOverflow);
        }
        Ok(Self::new(rounded as i64, self.currency))
    }

    fn expect_same_currency(self, other: Self) -> Result<()> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(Error::CurrencyMismatch {
                left: self.currency,
                right: other.currency,
            })
        }
    }
}

/// `12.50 EUR` — for logs and errors. The UI formats with `Intl.NumberFormat`,
/// which already knows symbol placement and digit count per locale.
impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let exponent = self.currency.exponent() as usize;
        if exponent == 0 {
            return write!(f, "{} {}", self.minor, self.currency);
        }
        let scale = self.currency.scale().unsigned_abs();
        let sign = if self.minor < 0 { "-" } else { "" };
        let abs = self.minor.unsigned_abs();
        let major = abs / scale;
        let frac = abs % scale;
        let currency = self.currency;
        write!(f, "{sign}{major}.{frac:0>exponent$} {currency}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_decimal_separators_parse() {
        let eur = Currency::EUR;
        assert_eq!(Money::parse("12,50", eur).unwrap().minor, 1250);
        assert_eq!(Money::parse("12.50", eur).unwrap().minor, 1250);
        assert_eq!(Money::parse("12", eur).unwrap().minor, 1200);
        assert_eq!(Money::parse("12,5", eur).unwrap().minor, 1250);
        assert_eq!(Money::parse(",50", eur).unwrap().minor, 50);
        assert_eq!(Money::parse("-3,07", eur).unwrap().minor, -307);
        assert_eq!(Money::parse(" 1 234,56 ", eur).unwrap().minor, 123_456);
    }

    /// Whitespace groups, `,`/`.` do not. The NBSP cases are what fr-FR
    /// `Intl.NumberFormat` and a spreadsheet paste hand over.
    #[test]
    fn whitespace_groups_but_dots_and_commas_never_do() {
        let eur = Currency::EUR;
        assert_eq!(Money::parse("1\u{a0}234,56", eur).unwrap().minor, 123_456);
        assert_eq!(Money::parse("1\u{202f}234,56", eur).unwrap().minor, 123_456);
        assert_eq!(
            Money::parse("1_234_567,89", eur).unwrap().minor,
            123_456_789
        );
        // Same number, written with the separators that could mean either thing.
        assert!(Money::parse("1.234,56", eur).is_err());
        assert!(Money::parse("1,234.56", eur).is_err());
    }

    #[test]
    fn exponents_follow_the_currency() {
        let jpy = Currency::new("jpy").unwrap();
        let bhd = Currency::new("BHD").unwrap();
        assert_eq!(jpy.exponent(), 0);
        assert_eq!(bhd.exponent(), 3);

        assert_eq!(Money::parse("1200", jpy).unwrap().minor, 1200);
        assert_eq!(Money::parse("1,234", bhd).unwrap().minor, 1234);
        assert_eq!(Money::new(1200, jpy).to_string(), "1200 JPY");
        assert_eq!(Money::new(1234, bhd).to_string(), "1.234 BHD");
        assert_eq!(Money::new(-5, Currency::EUR).to_string(), "-0.05 EUR");
        assert_eq!(Money::new(1250, Currency::EUR).to_string(), "12.50 EUR");
    }

    /// Too many decimals for the currency is a typo, not something to round away.
    #[test]
    fn junk_and_over_precision_are_rejected() {
        let eur = Currency::EUR;
        assert!(Money::parse("1,234", eur).is_err());
        assert!(Money::parse("12.505", eur).is_err());
        assert!(Money::parse("", eur).is_err());
        assert!(Money::parse("abc", eur).is_err());
        assert!(Money::parse("1.2.3", eur).is_err());
        // A separator with digits on neither side is not zero.
        assert!(Money::parse(".", eur).is_err());
        assert!(Money::parse(",", eur).is_err());
        assert!(Money::parse("-,", eur).is_err());
        assert_eq!(
            Money::parse("12.505", eur).unwrap_err().code(),
            "money.invalid"
        );
    }

    #[test]
    fn mixed_currencies_error_instead_of_summing() {
        let a = Money::new(1000, Currency::EUR);
        let b = Money::new(1000, Currency::USD);
        let err = a.checked_add(b).unwrap_err();
        assert_eq!(err.code(), "money.currency_mismatch");
        assert!(a.checked_sub(b).is_err());
        assert_eq!(a.checked_add(a).unwrap().minor, 2000);
        assert_eq!(Money::zero(Currency::EUR).checked_add(a).unwrap(), a);
    }

    #[test]
    fn overflow_is_reported_not_wrapped() {
        let big = Money::new(i64::MAX, Currency::EUR);
        assert_eq!(
            big.checked_add(Money::new(1, Currency::EUR))
                .unwrap_err()
                .code(),
            "money.overflow"
        );
        assert!(Money::parse("92233720368547758.08", Currency::EUR).is_err());
    }

    #[test]
    fn scaling_rounds_once() {
        let per_kg = Money::new(320, Currency::EUR);
        assert_eq!(per_kg.checked_mul_f64(2.5).unwrap().minor, 800);
        assert_eq!(per_kg.checked_mul_f64(0.3333).unwrap().minor, 107);
        assert!(per_kg.checked_mul_f64(f64::NAN).is_err());
    }

    /// 2^62 × 2 lands on exactly 2^63, one past `i64::MAX`. A `>` bound would let
    /// it through and let the cast saturate.
    #[test]
    fn scaling_past_the_top_is_an_error_not_a_saturated_cast() {
        let half = Money::new(1i64 << 62, Currency::EUR);
        assert_eq!(
            half.checked_mul_f64(2.0).unwrap_err().code(),
            "money.overflow"
        );
        // The range is asymmetric because `i64` is: -2^63 is `i64::MIN` exactly.
        assert_eq!(half.checked_mul_f64(-2.0).unwrap().minor, i64::MIN);
        assert!(half.checked_mul_f64(-2.5).is_err());
    }

    #[test]
    fn currency_codes_are_validated_and_normalised() {
        assert_eq!(Currency::new("eur").unwrap(), Currency::EUR);
        assert_eq!(Currency::new(" gbp ").unwrap(), Currency::GBP);
        assert_eq!(
            Currency::new("euro").unwrap_err().code(),
            "money.invalid_currency"
        );
        assert!(Currency::new("E1R").is_err());
        assert!(Currency::new("").is_err());
        assert_eq!(Currency::default(), Currency::EUR);
    }
}
