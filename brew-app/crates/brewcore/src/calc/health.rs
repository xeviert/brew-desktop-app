//! Phase 4f. Why has a device gone quiet? Two cheap guesses, and labelled as guesses.
//!
//! The gravity series alone cannot tell a dead iSpindle from a finished ferment: both
//! are a flat line that stops. What the device last said about itself can: a battery
//! near its cutoff says "battery likely", a healthy battery with a signal that was
//! falling says "signal". Anything else is no guess at all, which is the honest answer.
//!
//! Only ever asked about a device that is already silent — `db::health` decides that
//! from the stream's own interval. Nothing here reads the config.

use serde::{Deserialize, Serialize};

use crate::units::Timestamp;

/// The least [`stale_after_secs`] returns: a 30 s probe that misses a few readings is
/// not news.
pub const STALE_FLOOR_SECS: i64 = 600;

/// The median spacing of `times`, oldest first. `None` under two.
pub fn median_interval(times: &[Timestamp]) -> Option<i64> {
    let mut diffs: Vec<i64> = times.windows(2).map(|w| w[0].secs_until(w[1])).collect();
    if diffs.is_empty() {
        return None;
    }
    diffs.sort_unstable();
    Some(diffs[diffs.len() / 2])
}

/// How long a device may go quiet before it is stale: 3× its median interval, at
/// least [`STALE_FLOOR_SECS`]. Derived, not configured — a 30 s probe and a 15 min
/// iSpindle can't share a threshold, and the interval is a device setting. Phase 4f's
/// card and Phase 6's alert both judge by this.
pub fn stale_after_secs(median_interval: Option<i64>) -> i64 {
    median_interval.map_or(STALE_FLOOR_SECS, |m| (m * 3).max(STALE_FLOOR_SECS))
}

/// An iSpindle's LiPo is nearly flat here; below ~3.5 V it browns out mid-send.
pub const BATTERY_WARN_V_DEFAULT: f64 = 3.6;
/// Below this the ESP8266 drops packets and retries eat the battery.
pub const RSSI_WARN_DBM_DEFAULT: f64 = -80.0;
/// "Near the threshold": a cell that read this close to the warning level at its last
/// send may be under it by the next.
pub const BATTERY_NEAR_V: f64 = 0.1;
/// How far below its own typical signal a device must have fallen to count as falling
/// while still above the warning level — something moved between it and the Pi.
pub const RSSI_DROP_DB: f64 = 10.0;

/// The two numbers `config::Health` carries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HealthParams {
    pub battery_warn_v: f64,
    pub rssi_warn_dbm: f64,
}

impl Default for HealthParams {
    fn default() -> Self {
        Self {
            battery_warn_v: BATTERY_WARN_V_DEFAULT,
            rssi_warn_dbm: RSSI_WARN_DBM_DEFAULT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SilenceGuess {
    Battery,
    Signal,
}

pub fn battery_low(volts: f64, params: &HealthParams) -> bool {
    volts < params.battery_warn_v
}

pub fn rssi_weak(dbm: f64, params: &HealthParams) -> bool {
    dbm < params.rssi_warn_dbm
}

/// Why a silent device might be silent. `rssi` is its recent signal readings, oldest
/// first; the last is the newest. Battery wins: a flat cell also weakens the radio, so
/// a low battery with a weak signal is still the battery.
pub fn guess(battery_v: Option<f64>, rssi: &[f64], params: &HealthParams) -> Option<SilenceGuess> {
    if let Some(v) = battery_v {
        if v <= params.battery_warn_v + BATTERY_NEAR_V {
            return Some(SilenceGuess::Battery);
        }
    }
    let &newest = rssi.last()?;
    if rssi_weak(newest, params) || newest <= median(rssi) - RSSI_DROP_DB {
        return Some(SilenceGuess::Signal);
    }
    None
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: HealthParams = HealthParams {
        battery_warn_v: 3.6,
        rssi_warn_dbm: -80.0,
    };

    #[test]
    fn a_battery_near_the_threshold_is_the_battery() {
        assert_eq!(guess(Some(3.68), &[], &P), Some(SilenceGuess::Battery));
        assert_eq!(guess(Some(3.5), &[-60.0], &P), Some(SilenceGuess::Battery));
    }

    #[test]
    fn a_low_battery_with_a_weak_signal_is_still_the_battery() {
        assert_eq!(
            guess(Some(3.55), &[-60.0, -90.0], &P),
            Some(SilenceGuess::Battery)
        );
    }

    #[test]
    fn a_healthy_battery_and_a_signal_under_the_warning_is_the_signal() {
        assert_eq!(
            guess(Some(4.0), &[-82.0, -83.0], &P),
            Some(SilenceGuess::Signal)
        );
    }

    #[test]
    fn a_healthy_battery_and_a_signal_that_fell_is_the_signal() {
        let rssi = [-60.0, -61.0, -59.0, -60.0, -62.0, -72.0];
        assert_eq!(guess(Some(4.0), &rssi, &P), Some(SilenceGuess::Signal));
    }

    #[test]
    fn a_healthy_battery_and_a_steady_signal_is_no_guess() {
        let rssi = [-70.0, -72.0, -69.0, -71.0, -75.0];
        assert_eq!(guess(Some(4.0), &rssi, &P), None);
    }

    #[test]
    fn nothing_to_go_on_is_no_guess() {
        assert_eq!(guess(None, &[], &P), None);
        assert_eq!(guess(Some(4.1), &[], &P), None);
    }

    #[test]
    fn the_flags_are_strictly_below_the_thresholds() {
        assert!(!battery_low(3.6, &P));
        assert!(battery_low(3.59, &P));
        assert!(!rssi_weak(-80.0, &P));
        assert!(rssi_weak(-80.5, &P));
    }
}
