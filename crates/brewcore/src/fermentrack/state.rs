//! The BrewPi controller state, `0..=9`.
//!
//! BrewPi's vocabulary, not ours, so it lives here rather than in `db/`. The integer
//! is what reaches `reading.value` — this enum exists so Phase 4's chart can label a
//! step change instead of drawing a mystery staircase.
//!
//! The set is from `docs/FERMENTRACK-API.md` §1, which read it out of the source.

/// `state` as written to `_full.csv` (index 8, in every `model_version`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ControllerState {
    Idle = 0,
    StateOff = 1,
    DoorOpen = 2,
    Heating = 3,
    Cooling = 4,
    WaitingToCool = 5,
    WaitingToHeat = 6,
    WaitingForPeakDetect = 7,
    CoolingMinTime = 8,
    HeatingMinTime = 9,
}

impl ControllerState {
    pub const ALL: [ControllerState; 10] = [
        ControllerState::Idle,
        ControllerState::StateOff,
        ControllerState::DoorOpen,
        ControllerState::Heating,
        ControllerState::Cooling,
        ControllerState::WaitingToCool,
        ControllerState::WaitingToHeat,
        ControllerState::WaitingForPeakDetect,
        ControllerState::CoolingMinTime,
        ControllerState::HeatingMinTime,
    ];

    /// `None` for anything outside the documented range. A firmware we have not seen
    /// is a reason to warn and store nothing, never a reason to panic on a row in the
    /// middle of a year of history.
    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }

    pub fn code(self) -> i64 {
        self as u8 as i64
    }

    /// For the chart legend. Not persisted — [`ControllerState::code`] is.
    pub fn label(self) -> &'static str {
        match self {
            ControllerState::Idle => "idle",
            ControllerState::StateOff => "off",
            ControllerState::DoorOpen => "door open",
            ControllerState::Heating => "heating",
            ControllerState::Cooling => "cooling",
            ControllerState::WaitingToCool => "waiting to cool",
            ControllerState::WaitingToHeat => "waiting to heat",
            ControllerState::WaitingForPeakDetect => "waiting for peak detect",
            ControllerState::CoolingMinTime => "cooling (min time)",
            ControllerState::HeatingMinTime => "heating (min time)",
        }
    }
}

impl std::fmt::Display for ControllerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_zero_through_nine_in_order() {
        for (i, state) in ControllerState::ALL.into_iter().enumerate() {
            assert_eq!(state.code(), i as i64);
            assert_eq!(ControllerState::from_code(i as i64), Some(state));
        }
    }

    #[test]
    fn out_of_range_is_none_not_a_panic() {
        assert_eq!(ControllerState::from_code(10), None);
        assert_eq!(ControllerState::from_code(11), None);
        assert_eq!(ControllerState::from_code(-1), None);
        assert_eq!(ControllerState::from_code(i64::MAX), None);
    }

    /// The fixture generator emits 0, 3, 4, 5 and 6 (`crates/fixtures/src/lib.rs`),
    /// and its own test asserts `state <= 9`.
    #[test]
    fn the_states_the_fixtures_emit_are_all_known() {
        for code in [0, 3, 4, 5, 6] {
            assert!(ControllerState::from_code(code).is_some());
        }
    }
}
