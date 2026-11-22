//! What the desktop tells `brewd` about each batch it should watch.
//!
//! Built from `brewery.db` on every sync (`db::alert::build_watches`) and pushed whole,
//! so `brewd` never holds a stale half of it: a pitch, a packaging or a changed band
//! reaches the Pi within one poll. Feeds name a journal device and column, never a
//! stream id — `brewd` has no database to look one up in.

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    calc::{
        health::HealthParams,
        stability::{ExpectedFg, StabilityParams},
    },
    config::IspindelUnit,
    units::{Sg, Timestamp},
};

/// The `"v"` a [`WatchSet`] carries. A `brewd` and a desktop that disagree on what a
/// watch means refuse each other rather than judge half of one.
pub const WATCH_VERSION: u64 = 2;

/// One journal column, from the moment it was bound to the batch. Readings before
/// `since` are the vessel's previous batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub device: String,
    pub column: String,
    pub since: Timestamp,
}

/// A gravity column and what unit the device reports it in — only the desktop's
/// config knows, and a watch is only built once it does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GravityFeed {
    pub device: String,
    pub column: String,
    pub since: Timestamp,
    pub unit: IspindelUnit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    pub batch_id: i64,
    /// For the notification. The desktop shows its own.
    pub code: String,
    pub gravity: Option<GravityFeed>,
    pub temp: Option<Feed>,
    /// Every journal device bound to the batch, for [`super::Rule::Silent`].
    pub devices: Vec<String>,
    pub og: Option<Sg>,
    pub expected_fg: Option<ExpectedFg>,
    pub target_sg: Option<Sg>,
    pub temp_min_c: Option<f64>,
    pub temp_max_c: Option<f64>,
    /// When the band began, if it comes from a schedule step: a newly ticked step's
    /// band isn't judged until the beer gets there.
    pub band_since: Option<Timestamp>,
    pub stability: StabilityParams,
    pub health: HealthParams,
}

impl Watch {
    /// Whether a row from `device` can change any of this watch's verdicts.
    pub fn reads(&self, device: &str) -> bool {
        self.devices.iter().any(|d| d == device)
            || self.gravity.as_ref().is_some_and(|f| f.device == device)
            || self.temp.as_ref().is_some_and(|f| f.device == device)
    }
}

/// The whole set, as `PUT /watches` carries it and `brewd` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchSet {
    pub v: u64,
    pub watches: Vec<Watch>,
}

/// Nothing watched, at this build's version — what a `brewd` with no state serves.
impl Default for WatchSet {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl WatchSet {
    pub fn new(watches: Vec<Watch>) -> Self {
        Self {
            v: WATCH_VERSION,
            watches,
        }
    }

    /// The version is checked before any field is read, so a newer set is reported as
    /// newer rather than as whichever of its fields fails first.
    pub fn from_json(json: &[u8]) -> Result<Self> {
        let invalid = |e: serde_json::Error| Error::InvalidWatch {
            reason: e.to_string(),
        };
        let value: serde_json::Value = serde_json::from_slice(json).map_err(invalid)?;
        let found = value.get("v");
        if found.and_then(serde_json::Value::as_u64) != Some(WATCH_VERSION) {
            return Err(Error::WatchVersion {
                found: found.map_or_else(|| "none".into(), |v| v.to_string()),
            });
        }
        let set: Self = serde_json::from_value(value).map_err(invalid)?;
        set.check()?;
        Ok(set)
    }

    fn check(&self) -> Result<()> {
        let invalid = |reason: String| Err(Error::InvalidWatch { reason });
        let mut ids = std::collections::HashSet::new();
        for w in &self.watches {
            if !ids.insert(w.batch_id) {
                return invalid(format!("batch {} is watched twice", w.batch_id));
            }
            if let (Some(lo), Some(hi)) = (w.temp_min_c, w.temp_max_c) {
                if lo > hi {
                    return invalid(format!("batch {}: temp band is inverted", w.batch_id));
                }
            }
            let bounds = [w.temp_min_c, w.temp_max_c];
            if bounds.iter().flatten().any(|t| !t.is_finite()) {
                return invalid(format!("batch {}: temp band is not finite", w.batch_id));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn watch(batch_id: i64) -> Watch {
        Watch {
            batch_id,
            code: format!("SELTZER-{batch_id:03}"),
            gravity: Some(GravityFeed {
                device: "ispindel".into(),
                column: "gravity".into(),
                since: Timestamp::from_secs(0),
                unit: IspindelUnit::Sg,
            }),
            temp: Some(Feed {
                device: "esp32".into(),
                column: "vessel_temp".into(),
                since: Timestamp::from_secs(0),
            }),
            devices: vec!["ispindel".into(), "esp32".into()],
            og: Some(Sg::new_unchecked(1.040)),
            expected_fg: None,
            target_sg: None,
            temp_min_c: Some(16.0),
            temp_max_c: Some(20.0),
            band_since: None,
            stability: StabilityParams::default(),
            health: HealthParams::default(),
        }
    }

    #[test]
    fn the_empty_default_carries_the_version() {
        assert_eq!(WatchSet::default().v, WATCH_VERSION);
    }

    #[test]
    fn a_set_round_trips_with_its_version() {
        let set = WatchSet::new(vec![watch(1), watch(2)]);
        let json = serde_json::to_vec(&set).unwrap();
        assert_eq!(WatchSet::from_json(&json).unwrap(), set);
    }

    #[test]
    fn another_version_is_refused_before_its_fields_are_read() {
        for v in [WATCH_VERSION - 1, WATCH_VERSION + 1] {
            let json = format!(r#"{{"v":{v},"watches":[{{"nonsense":1}}]}}"#);
            let err = WatchSet::from_json(json.as_bytes()).unwrap_err();
            assert_eq!(err.code(), "alert.watch_version");
        }
        let err = WatchSet::from_json(br#"{"watches":[]}"#).unwrap_err();
        assert_eq!(err.code(), "alert.watch_version");
    }

    #[test]
    fn an_unknown_field_is_refused_not_dropped() {
        let mut value = serde_json::to_value(WatchSet::new(vec![watch(1)])).unwrap();
        value["watches"][0]["temp_rate"] = 1.into();
        let err = WatchSet::from_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(err.code(), "alert.invalid_watch");
    }

    #[test]
    fn a_batch_twice_or_an_inverted_band_is_refused() {
        let twice = WatchSet::new(vec![watch(1), watch(1)]);
        let err = WatchSet::from_json(&serde_json::to_vec(&twice).unwrap()).unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");

        let mut inverted = watch(1);
        inverted.temp_min_c = Some(22.0);
        let set = WatchSet::new(vec![inverted]);
        let err = WatchSet::from_json(&serde_json::to_vec(&set).unwrap()).unwrap_err();
        assert!(err.to_string().contains("inverted"), "{err}");
    }
}
