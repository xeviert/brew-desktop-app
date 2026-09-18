//! Mass, volume and count.
//!
//! # Canonical column plus entry unit
//!
//! Recipes are written in the units you shop in — 2.5 kg of sugar, a 5 gal carboy,
//! one packet of yeast — but every sum, cost and comparison has to read one unit.
//! So the Phase 3 tables store **both**: a canonical `qty_g` or `qty_l` that all
//! queries use, and `entry_qty` + `entry_unit` for display, so a recipe prints back
//! the way it was typed.
//!
//! A bare `qty` column with a `unit` column alongside it, and no canonical value,
//! is the thing this arrangement exists to prevent: it makes `SELECT SUM(qty)`
//! silently meaningless.

use super::canonical_float;
use crate::{Error, Result};

canonical_float!(
    /// Litres — the canonical volume.
    Litres,
    "volume",
    " L"
);

canonical_float!(
    /// Grams — the canonical mass.
    Grams,
    "mass",
    " g"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dimension {
    Mass,
    Volume,
    Count,
}

impl std::fmt::Display for Dimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Dimension::Mass => "mass",
            Dimension::Volume => "volume",
            Dimension::Count => "count",
        };
        f.write_str(name)
    }
}

/// Units a brewery actually types. Deliberately small: every addition is a new
/// conversion factor someone has to get right.
/// One spelling everywhere: [`Unit::symbol`] is what goes into SQLite *and* into
/// JSON/TOML. A derived `rename_all` would give `gal_us` in a config file and
/// `gal` in a row for the same variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Mg,
    G,
    Kg,
    Oz,
    Lb,
    Ml,
    L,
    Hl,
    GalUs,
    /// Packets, tablets, campden tabs — things that come in whole numbers.
    Each,
}

impl Unit {
    pub const fn dimension(self) -> Dimension {
        match self {
            Unit::Mg | Unit::G | Unit::Kg | Unit::Oz | Unit::Lb => Dimension::Mass,
            Unit::Ml | Unit::L | Unit::Hl | Unit::GalUs => Dimension::Volume,
            Unit::Each => Dimension::Count,
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Unit::Mg => "mg",
            Unit::G => "g",
            Unit::Kg => "kg",
            Unit::Oz => "oz",
            Unit::Lb => "lb",
            Unit::Ml => "mL",
            Unit::L => "L",
            Unit::Hl => "hL",
            Unit::GalUs => "gal",
            Unit::Each => "ea",
        }
    }

    /// How many canonical units one of these is.
    ///
    /// The imperial factors are exact, not approximations: the international pound
    /// is *defined* as 0.453 592 37 kg (1959 agreement) and the US liquid gallon as
    /// 231 in³ = 3.785 411 784 L. That is why the tests can assert them tightly.
    const fn factor(self) -> f64 {
        match self {
            Unit::Mg => 0.001,
            Unit::G => 1.0,
            Unit::Kg => 1000.0,
            Unit::Oz => 453.592_37 / 16.0,
            Unit::Lb => 453.592_37,
            Unit::Ml => 0.001,
            Unit::L => 1.0,
            Unit::Hl => 100.0,
            Unit::GalUs => 3.785_411_784,
            Unit::Each => 1.0,
        }
    }

    fn expect_dimension(self, wanted: Dimension) -> Result<()> {
        if self.dimension() == wanted {
            Ok(())
        } else {
            Err(Error::DimensionMismatch {
                unit: self.symbol(),
                from: self.dimension(),
                to: wanted,
            })
        }
    }
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.symbol())
    }
}

impl std::str::FromStr for Unit {
    type Err = Error;

    /// Lenient on input — `lbs`, `gal_us` and `each` are all things people type —
    /// while [`Unit::symbol`] stays the single spelling on the way out.
    fn from_str(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "mg" => Ok(Unit::Mg),
            "g" => Ok(Unit::G),
            "kg" => Ok(Unit::Kg),
            "oz" => Ok(Unit::Oz),
            "lb" | "lbs" => Ok(Unit::Lb),
            "ml" => Ok(Unit::Ml),
            "l" => Ok(Unit::L),
            "hl" => Ok(Unit::Hl),
            "gal" | "gal_us" => Ok(Unit::GalUs),
            "ea" | "each" => Ok(Unit::Each),
            // The untouched input, not the lowercased one: an error should quote
            // back what was actually typed.
            _ => Err(Error::UnknownUnit(trimmed.to_owned())),
        }
    }
}

impl serde::Serialize for Unit {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(self.symbol())
    }
}

impl<'de> serde::Deserialize<'de> for Unit {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(de)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl rusqlite::ToSql for Unit {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.symbol().into())
    }
}

impl rusqlite::types::FromSql for Unit {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        text.parse()
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

/// A number as it was entered: the pair that goes into `entry_qty` / `entry_unit`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quantity {
    pub value: f64,
    pub unit: Unit,
}

impl Quantity {
    pub fn new(value: f64, unit: Unit) -> Result<Self> {
        if !value.is_finite() {
            return Err(Error::NotFinite {
                field: "quantity",
                value,
            });
        }
        Ok(Self { value, unit })
    }

    pub fn to_grams(self) -> Result<Grams> {
        self.unit.expect_dimension(Dimension::Mass)?;
        Grams::new(self.value * self.unit.factor())
    }

    pub fn to_litres(self) -> Result<Litres> {
        self.unit.expect_dimension(Dimension::Volume)?;
        Litres::new(self.value * self.unit.factor())
    }

    pub fn to_count(self) -> Result<f64> {
        self.unit.expect_dimension(Dimension::Count)?;
        Ok(self.value)
    }

    /// Back to the entry unit, for redisplay.
    pub fn from_grams(g: Grams, unit: Unit) -> Result<Self> {
        unit.expect_dimension(Dimension::Mass)?;
        Quantity::new(g.get() / unit.factor(), unit)
    }

    pub fn from_litres(l: Litres, unit: Unit) -> Result<Self> {
        unit.expect_dimension(Dimension::Volume)?;
        Quantity::new(l.get() / unit.factor(), unit)
    }
}

impl std::fmt::Display for Quantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.value, self.unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::close;

    /// Exact definitions, so the tolerance is machine-epsilon rather than "close
    /// enough for brewing".
    #[test]
    fn imperial_factors_are_the_defined_ones() {
        assert!(close(
            Quantity::new(1.0, Unit::Lb)
                .unwrap()
                .to_grams()
                .unwrap()
                .get(),
            453.59237
        ));
        assert!(close(
            Quantity::new(16.0, Unit::Oz)
                .unwrap()
                .to_grams()
                .unwrap()
                .get(),
            453.59237
        ));
        assert!(close(
            Quantity::new(1.0, Unit::GalUs)
                .unwrap()
                .to_litres()
                .unwrap()
                .get(),
            3.785411784
        ));
    }

    #[test]
    fn metric_factors() {
        assert!(close(
            Quantity::new(2.5, Unit::Kg)
                .unwrap()
                .to_grams()
                .unwrap()
                .get(),
            2500.0
        ));
        assert!(close(
            Quantity::new(500.0, Unit::Ml)
                .unwrap()
                .to_litres()
                .unwrap()
                .get(),
            0.5
        ));
        assert!(close(
            Quantity::new(1.0, Unit::Hl)
                .unwrap()
                .to_litres()
                .unwrap()
                .get(),
            100.0
        ));
    }

    #[test]
    fn mass_round_trips() {
        for unit in [Unit::Mg, Unit::G, Unit::Kg, Unit::Oz, Unit::Lb] {
            for value in [0.0, 1.0, 2.5, 453.59237, 1e6] {
                let q = Quantity::new(value, unit).unwrap();
                let back = Quantity::from_grams(q.to_grams().unwrap(), unit).unwrap();
                assert!(close(back.value, value), "{value} {unit} did not survive");
            }
        }
    }

    #[test]
    fn volume_round_trips() {
        for unit in [Unit::Ml, Unit::L, Unit::Hl, Unit::GalUs] {
            for value in [0.0, 1.0, 5.0, 23.5, 1e6] {
                let q = Quantity::new(value, unit).unwrap();
                let back = Quantity::from_litres(q.to_litres().unwrap(), unit).unwrap();
                assert!(close(back.value, value), "{value} {unit} did not survive");
            }
        }
    }

    #[test]
    fn crossing_dimensions_is_an_error_not_a_multiply() {
        let two_kilos = Quantity::new(2.0, Unit::Kg).unwrap();
        let err = two_kilos.to_litres().unwrap_err();
        assert_eq!(err.code(), "units.dimension_mismatch");

        assert!(Quantity::new(5.0, Unit::L).unwrap().to_grams().is_err());
        assert!(Quantity::new(1.0, Unit::Each).unwrap().to_grams().is_err());
    }

    #[test]
    fn count_stays_a_count() {
        assert_eq!(
            Quantity::new(2.0, Unit::Each).unwrap().to_count().unwrap(),
            2.0
        );
    }

    #[test]
    fn symbols_round_trip_through_parsing() {
        for unit in [
            Unit::Mg,
            Unit::G,
            Unit::Kg,
            Unit::Oz,
            Unit::Lb,
            Unit::Ml,
            Unit::L,
            Unit::Hl,
            Unit::GalUs,
            Unit::Each,
        ] {
            assert_eq!(unit.symbol().parse::<Unit>().unwrap(), unit);
        }
        assert!("furlong".parse::<Unit>().is_err());
    }

    /// One spelling, or a config file and a table row disagree about `gal`.
    #[test]
    fn serde_and_sqlite_agree_on_the_spelling() {
        use rusqlite::{ToSql, types::ToSqlOutput, types::Value, types::ValueRef};

        for unit in [Unit::Ml, Unit::Hl, Unit::GalUs, Unit::Each, Unit::L] {
            let json = serde_json::to_string(&unit).unwrap();
            assert_eq!(json, format!("\"{}\"", unit.symbol()));
            assert_eq!(serde_json::from_str::<Unit>(&json).unwrap(), unit);

            let sql = match unit.to_sql().unwrap() {
                ToSqlOutput::Borrowed(ValueRef::Text(b)) => String::from_utf8(b.to_vec()).unwrap(),
                ToSqlOutput::Owned(Value::Text(s)) => s,
                other => panic!("{unit} bound as {other:?}"),
            };
            assert_eq!(sql, unit.symbol());
        }
    }

    /// Lenient in, canonical out — and the error quotes what was typed.
    #[test]
    fn aliases_parse_but_errors_are_verbatim() {
        assert_eq!("lbs".parse::<Unit>().unwrap(), Unit::Lb);
        assert_eq!("gal_us".parse::<Unit>().unwrap(), Unit::GalUs);
        assert_eq!(" EACH ".parse::<Unit>().unwrap(), Unit::Each);
        let err = "PoUnDs".parse::<Unit>().unwrap_err();
        assert_eq!(err.to_string(), "unknown unit 'PoUnDs'");
    }

    #[test]
    fn non_finite_is_rejected() {
        assert!(Quantity::new(f64::NAN, Unit::Kg).is_err());
    }
}
