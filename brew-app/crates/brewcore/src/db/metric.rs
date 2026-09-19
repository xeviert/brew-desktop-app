//! The closed vocabulary of `stream.metric`.
//!
//! `0001_tier1.sql` deliberately leaves `metric` without a `CHECK`: SQLite cannot
//! widen one without rebuilding the table, and Phase 3 adds manual metrics (`ph`,
//! `ta`) to this set. This enum is the enforcement instead, at the only write path.
//!
//! It lives in `db/` rather than in `fermentrack/` because the vocabulary is ours,
//! not Fermentrack's: Phase 3's manual measurements and v2's MQTT streams write the
//! same `metric` strings without going anywhere near a Fermentrack module. The
//! *mapping* from a CSV column to a `Metric` is `fermentrack`'s, and lives in
//! `fermentrack::csv_log`.

/// One kind of time series. The string form is persisted, so it is part of the
/// schema: changing one is a migration, not a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Metric {
    /// The fermenting liquid. Named `temp_c` and not `beer_temp_c` — see
    /// docs/BUILDOUT.md "Scope": nothing in our schema is named "beer".
    TempC,
    /// What the controller was asked to hold the liquid at.
    SetpointC,
    FridgeTempC,
    FridgeSetpointC,
    RoomTempC,
    /// Specific gravity. Never Plato or Brix.
    Sg,
    /// The gravity sensor's own thermometer, which is not the fermenter probe.
    GravityTempC,
    /// The BrewPi controller state, stored as its integer. See
    /// [`crate::fermentrack::ControllerState`].
    State,
}

impl Metric {
    pub const ALL: [Metric; 8] = [
        Metric::TempC,
        Metric::SetpointC,
        Metric::FridgeTempC,
        Metric::FridgeSetpointC,
        Metric::RoomTempC,
        Metric::Sg,
        Metric::GravityTempC,
        Metric::State,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Metric::TempC => "temp_c",
            Metric::SetpointC => "setpoint_c",
            Metric::FridgeTempC => "fridge_temp_c",
            Metric::FridgeSetpointC => "fridge_setpoint_c",
            Metric::RoomTempC => "room_temp_c",
            Metric::Sg => "sg",
            Metric::GravityTempC => "gravity_temp_c",
            Metric::State => "state",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

impl std::fmt::Display for Metric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::ToSql for Metric {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

/// An unrecognised `metric` string is a data error, not a reason to panic: it means
/// an older or newer build wrote this row.
impl rusqlite::types::FromSql for Metric {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Metric::parse(text).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(Box::new(crate::Error::UnknownMetric(
                text.to_owned(),
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_its_string_form() {
        for metric in Metric::ALL {
            assert_eq!(Metric::parse(metric.as_str()), Some(metric));
        }
        assert_eq!(Metric::parse("beer_temp"), None);
        assert_eq!(Metric::parse(""), None);
    }

    /// These strings are in the database. Pinning them here is what makes changing
    /// one a deliberate act with a migration attached, rather than a rename that
    /// orphans every existing `stream` row.
    #[test]
    fn the_stored_strings_are_pinned() {
        assert_eq!(
            Metric::ALL.map(Metric::as_str),
            [
                "temp_c",
                "setpoint_c",
                "fridge_temp_c",
                "fridge_setpoint_c",
                "room_temp_c",
                "sg",
                "gravity_temp_c",
                "state",
            ]
        );
    }

    /// docs/BUILDOUT.md "Scope": no table, column or value in our own code is named
    /// "beer". `tests/schema_conventions.rs` enforces it for the schema; this is the
    /// same rule for the vocabulary the schema stores.
    #[test]
    fn no_metric_is_named_beer() {
        for metric in Metric::ALL {
            assert!(!metric.as_str().contains("beer"), "{metric}");
        }
    }

    #[test]
    fn all_is_complete_and_unique() {
        let mut names: Vec<&str> = Metric::ALL.iter().map(|m| m.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two metrics share a string");
    }
}
