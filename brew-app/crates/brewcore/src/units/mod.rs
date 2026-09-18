//! The single owner of every unit conversion.
//!
//! **Store canonical, convert at the edges.** Fermentrack hands us Fahrenheit or
//! Celsius per row; gravity gets discussed in SG, Plato or Brix depending on
//! whether the conversation is about seltzer, beer or wine; ingredients are bought
//! in kilos and pounds. None of that variation is allowed past this module.
//!
//! | Quantity | Stored as |
//! |---|---|
//! | Temperature | °C, `REAL` |
//! | Gravity | SG, `REAL` |
//! | Volume | litres, `REAL` |
//! | Mass | grams, `REAL` |
//! | Time | UTC epoch seconds, `INTEGER` |
//! | Money | integer minor units + ISO 4217 code |
//!
//! # Definitional here, empirical in `calc`
//!
//! This module holds conversions that are *exact by definition* — °C↔°F, g↔lb,
//! L↔US gal — which is why its tests can assert near-machine-epsilon tolerances.
//! Conversions that are a polynomial fit of measured data (SG↔Plato, refractometer
//! correction, ABV) live in [`crate::calc`] with a citation and an error band.
//! A conversion that needs a literature reference is not a unit conversion.
//!
//! Display preference is config, never storage.

mod gravity;
mod money;
mod quantity;
mod temperature;
mod time;

pub use gravity::{GravityDisplay, Sg};
pub use money::{Currency, Money};
pub use quantity::{Dimension, Grams, Litres, Quantity, Unit};
pub use temperature::{Celsius, TempDisplay, TempScale};
pub use time::Timestamp;

/// Defines a `f64` newtype that is stored canonically.
///
/// Every one of these gets the same four traits, and it is the `rusqlite` pair
/// that makes the convention stick: db code binds a `Celsius` and never handles a
/// bare `f64` that might be Fahrenheit.
macro_rules! canonical_float {
    ($(#[$meta:meta])* $name:ident, $unit_field:literal, $suffix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(f64);

        impl $name {
            /// Rejects NaN and infinity: a non-finite value that reaches SQLite is
            /// stored as NULL, so it would vanish rather than fail loudly.
            pub fn new(value: f64) -> $crate::Result<Self> {
                if !value.is_finite() {
                    return Err($crate::Error::NotFinite {
                        field: $unit_field,
                        value,
                    });
                }
                Ok(Self(value))
            }

            /// Skips the finite check. For literals and values already validated.
            pub const fn new_unchecked(value: f64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> f64 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}{}", self.0, $suffix)
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                f64::column_result(value).map(Self)
            }
        }
    };
}

pub(crate) use canonical_float;

/// Relative comparison for the round-trip tests. Every conversion here is a
/// multiply or a linear map, so error is proportional to magnitude, not absolute.
#[cfg(test)]
pub(crate) fn close(a: f64, b: f64) -> bool {
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() <= 1e-12 * scale
}
