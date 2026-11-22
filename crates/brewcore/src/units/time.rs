//! Time.
//!
//! One representation, everywhere: UTC epoch seconds in an `INTEGER` column.
//! Text timestamps sort correctly only if every writer agrees on the format and
//! the offset, and they cost a parse on every comparison. Integers do neither.
//!
//! Seconds, not milliseconds: Fermentrack logs at minute resolution, and seconds
//! keep the numbers small enough to read in a `sqlite3` shell.
//!
//! No `chrono` here — formatting a timestamp is the UI's job (`src/lib/units.ts`),
//! and parsing Fermentrack's `%Y/%m/%d %H:%M:%SZ` is Phase 2's, so the dependency
//! arrives with the parser that needs it.

use std::time::{SystemTime, UNIX_EPOCH};

/// A point in time, as UTC epoch seconds.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const UNIX_EPOCH: Self = Self(0);

    pub const fn from_secs(secs: i64) -> Self {
        Self(secs)
    }

    pub const fn as_secs(self) -> i64 {
        self.0
    }

    pub fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => Self(d.as_secs() as i64),
            // Clock set before 1970. Not worth an error path.
            Err(e) => Self(-(e.duration().as_secs() as i64)),
        }
    }

    /// Seconds from `self` to `other`, negative if `other` is earlier.
    pub const fn secs_until(self, other: Self) -> i64 {
        other.0 - self.0
    }

    pub const fn saturating_add_secs(self, secs: i64) -> Self {
        Self(self.0.saturating_add(secs))
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl rusqlite::ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.0.into())
    }
}

impl rusqlite::types::FromSql for Timestamp {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_secs() {
        for secs in [i64::MIN, -1, 0, 1, 1_758_153_600, i64::MAX] {
            assert_eq!(Timestamp::from_secs(secs).as_secs(), secs);
        }
    }

    #[test]
    fn orders_chronologically() {
        let earlier = Timestamp::from_secs(1_000);
        let later = Timestamp::from_secs(2_000);
        assert!(earlier < later);
        assert_eq!(earlier.secs_until(later), 1_000);
        assert_eq!(later.secs_until(earlier), -1_000);
    }

    #[test]
    fn now_is_after_2020() {
        assert!(Timestamp::now().as_secs() > 1_577_836_800);
    }

    #[test]
    fn adding_saturates_rather_than_panics() {
        assert_eq!(
            Timestamp::from_secs(i64::MAX)
                .saturating_add_secs(1)
                .as_secs(),
            i64::MAX
        );
    }
}
