//! The `db_enum!` macro: a closed vocabulary stored as its string.
//!
//! Lifted out of `reference.rs` in Phase 3b, where the macro's own note about
//! "3b/3c will want more" came true. Same promotion the `units` module does with
//! `canonical_float!`: define it once at module scope, `pub(crate) use` it, import
//! it from the submodules that invoke it.

/// A closed vocabulary stored as its string.
///
/// The same six impls `SourceKind` writes out by hand in
/// [`tier1`](crate::db::tier1) — a macro here because Phase 3a needs three of them
/// and 3b/3c will want more. `as_str` is the single place that decides what goes in
/// the database *and* what goes over the wire to the UI; a derived
/// `serde(rename_all)` would eventually disagree with it.
macro_rules! db_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $error:ident, $(($variant:ident, $text:literal)),+ $(,)?
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
                ser.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
                let text = String::deserialize(de)?;
                Self::parse(&text)
                    .ok_or_else(|| serde::de::Error::custom(Error::$error(text.clone())))
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(Box::new(Error::$error(text.to_owned())))
                })
            }
        }
    };
}

pub(crate) use db_enum;
