//! Phase 6. Alerts: the rules `brewd` runs over its journals while the laptop is shut.
//!
//! Everything here is pure — no database, no file, no clock. `brewd` owns the
//! [`Buffers`], the [`WatchSet`] and the [`AlertBook`], and persists the last two; the
//! desktop builds the watches from `brewery.db` (`db::alert`) and reads the
//! [`log`] back. The rule engine is the same code on both sides of that wire.
//!
//! **Why watches.** `brewd` has no database: it knows device names and raw columns,
//! not batches, OGs, pitches or expected FGs. The desktop pushes what each active
//! batch needs to be judged — its feeds, its OG, its band — and `brewd` evaluates that
//! on every burst it journals.
//!
//! **One alert per rule × batch while the condition holds.** A rule says raise, clear
//! or can't tell ([`Verdict`]); the space between raise and clear is the hysteresis,
//! so a reading hovering at an edge doesn't flap, and [`AlertBook::step`] never raises
//! twice. Every transition is a [`log::LogRecord`], which is the history: "log every
//! evaluation" is 4c's replay for the detector and this log for what fired.

pub mod book;
pub mod buffer;
pub mod eval;
pub mod log;
pub mod watch;

pub use book::{Alert, AlertBook};
pub use buffer::{Buffers, DeviceBuffer};
pub use eval::{Condition, Verdict, evaluate};
pub use log::{LogEvent, LogRecord};
pub use watch::{Feed, GravityFeed, WATCH_VERSION, Watch, WatchSet};

use serde::{Deserialize, Serialize};

/// What an alert is about. A closed set: the desktop branches on it, and the log
/// stores it by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// 4c's `Stable`: flat, and at the expected FG or with none to check.
    Finished,
    /// 4c's `Stalled`: flat above the expected FG. Its own alert, never folded into
    /// finished.
    Stalled,
    TempHigh,
    TempLow,
    /// A device bound to the batch stopped sending. Keyed per device.
    Silent,
    /// The ⅓ sugar break, from the OG and expected FG. Latched.
    SugarBreak,
    /// The batch's own target gravity. Latched.
    TargetSg,
}

impl Rule {
    /// A notification's title, after the batch code.
    pub fn title(self) -> &'static str {
        match self {
            Rule::Finished => "fermentation finished",
            Rule::Stalled => "fermentation stalled",
            Rule::TempHigh => "too warm",
            Rule::TempLow => "too cold",
            Rule::Silent => "device silent",
            Rule::SugarBreak => "⅓ sugar break",
            Rule::TargetSg => "target gravity reached",
        }
    }
}
