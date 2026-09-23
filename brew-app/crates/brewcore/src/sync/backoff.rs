//! How long to wait before the next poll.
//!
//! Two independent pieces, both pure so both can be tested without a clock:
//!
//! 1. [`backoff`] — the interval, doubled per consecutive failure, capped. A
//!    Fermentrack that is off for a weekend must not be asked 2 880 times about it.
//! 2. [`Jitter`] — ±[`JITTER_PERCENT`] on every delay, including the healthy one.
//!    Three fermenters added in the same minute would otherwise poll in lockstep
//!    forever, turning a 60 s cadence into a burst of three requests once a minute.
//!
//! No `rand` dependency. A 64-bit xorshift seeded once per source is more than enough
//! to spread three tasks across a minute, and being deterministic given a seed is what
//! makes the bounds assertable.

use std::time::Duration;

/// The longest gap between polls, however long a source has been failing. Fifteen
/// minutes is short enough that a Fermentrack coming back is noticed over a coffee,
/// and long enough that an overnight outage costs ~32 requests rather than ~480.
pub const BACKOFF_CEILING_SECS: u64 = 15 * 60;

/// Spread, as a percentage either side of the delay.
pub const JITTER_PERCENT: u64 = 20;

/// The delay before the next poll, before jitter.
///
/// `failures` is the count of *consecutive* failures, so 0 is the healthy interval.
/// The ceiling never shortens a delay: someone who set an hourly poll interval asked
/// for hourly, and a failing source must not start polling more often than a healthy
/// one.
pub fn backoff(failures: u32, interval_secs: u64) -> Duration {
    let factor = 1u64.checked_shl(failures).unwrap_or(u64::MAX);
    let ceiling = BACKOFF_CEILING_SECS.max(interval_secs);
    Duration::from_secs(interval_secs.saturating_mul(factor).min(ceiling))
}

/// A per-source spread, seeded once and advanced per delay.
#[derive(Debug, Clone)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    pub const fn from_seed(seed: u64) -> Self {
        // A zero state is a xorshift fixed point: it would return 0 forever, and every
        // source would then take exactly the low end of the range.
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// Seeded from the clock and the source id together. The id alone would make two
    /// installations agree; the clock alone would make two sources spawned in the same
    /// microsecond agree.
    pub fn for_source(source_id: i64) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self::from_seed(nanos ^ (source_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64. Not for anything that must be unguessable — only for spreading
        // polls out, which is why a dependency would be the wrong trade.
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// `base`, moved by up to ±[`JITTER_PERCENT`], never below one second.
    pub fn apply(&mut self, base: Duration) -> Duration {
        let secs = base.as_secs();
        let spread = secs * JITTER_PERCENT / 100;
        if spread == 0 {
            return base;
        }
        let offset = self.next_u64() % (2 * spread + 1);
        Duration::from_secs((secs + offset).saturating_sub(spread).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_source_polls_at_the_configured_interval() {
        assert_eq!(backoff(0, 60), Duration::from_secs(60));
        assert_eq!(backoff(0, 5), Duration::from_secs(5));
    }

    #[test]
    fn each_consecutive_failure_doubles_the_delay_up_to_the_ceiling() {
        assert_eq!(backoff(1, 60), Duration::from_secs(120));
        assert_eq!(backoff(2, 60), Duration::from_secs(240));
        assert_eq!(backoff(3, 60), Duration::from_secs(480));
        assert_eq!(backoff(4, 60), Duration::from_secs(BACKOFF_CEILING_SECS));
        // And it stays there rather than overflowing into a short delay.
        assert_eq!(backoff(64, 60), Duration::from_secs(BACKOFF_CEILING_SECS));
        assert_eq!(
            backoff(u32::MAX, 60),
            Duration::from_secs(BACKOFF_CEILING_SECS)
        );
    }

    /// Someone who configured an hourly poll asked for hourly. Clamping a failing
    /// source to the fifteen-minute ceiling would make it poll *more* often than a
    /// healthy one, which is the opposite of backing off.
    #[test]
    fn the_ceiling_never_shortens_an_interval_longer_than_it() {
        let hourly = 3_600;
        assert_eq!(backoff(0, hourly), Duration::from_secs(hourly));
        assert_eq!(backoff(9, hourly), Duration::from_secs(hourly));
    }

    #[test]
    fn jitter_stays_within_twenty_percent_and_never_reaches_zero() {
        let mut jitter = Jitter::from_seed(1);
        for _ in 0..10_000 {
            let delay = jitter.apply(Duration::from_secs(60)).as_secs();
            assert!((48..=72).contains(&delay), "{delay} is outside ±20% of 60");
        }

        // A one-second interval has no room to spread, and must not round down to a
        // busy loop.
        let mut tight = Jitter::from_seed(2);
        for _ in 0..100 {
            assert!(tight.apply(Duration::from_secs(1)).as_secs() >= 1);
        }
    }

    /// The point of jittering at all: two sources started together must not stay
    /// together.
    #[test]
    fn two_seeds_produce_different_delays() {
        let mut a = Jitter::from_seed(1);
        let mut b = Jitter::from_seed(99);
        let base = Duration::from_secs(60);
        let (left, right): (Vec<_>, Vec<_>) =
            (0..20).map(|_| (a.apply(base), b.apply(base))).unzip();
        assert_ne!(left, right);
    }

    /// A zero seed is a xorshift fixed point. Left unhandled, that source would take
    /// the bottom of the range every single poll.
    #[test]
    fn a_zero_seed_does_not_stick_at_one_value() {
        let mut jitter = Jitter::from_seed(0);
        let base = Duration::from_secs(600);
        let first = jitter.apply(base);
        assert!((1..40).any(|_| jitter.apply(base) != first));
    }
}
