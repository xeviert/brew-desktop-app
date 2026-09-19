//! A plausible fermentation, deterministically.
//!
//! Seeded xorshift64* rather than `rand`: one fewer dependency, and identical bytes
//! on every platform, which is what makes the committed goldens a regression guard
//! instead of a source of spurious diffs.

/// xorshift64*, Vigna 2016. Good enough for jitter; not for anything else.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // The state must never be zero, and a caller passing `--seed 0` is likelier
        // than any other single value.
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[-amplitude, amplitude]`.
    pub fn noise(&mut self, amplitude: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64; // [0, 1)
        (unit * 2.0 - 1.0) * amplitude
    }
}

/// Gravity at `hours` after pitch: a lag phase, exponential attenuation, then a
/// terminal plateau. Seltzer numbers — OG 1.045 down to 1.000 — because that is
/// what this brewery actually ferments, and because a terminal gravity of 1.000 is
/// where Phase 3's ABV formulas diverge.
pub fn gravity_at(hours: f64) -> f64 {
    const OG: f64 = 1.045;
    const FG: f64 = 1.000;
    const LAG_HOURS: f64 = 12.0;
    const TAU_HOURS: f64 = 34.0;

    if hours <= LAG_HOURS {
        return OG;
    }
    FG + (OG - FG) * (-(hours - LAG_HOURS) / TAU_HOURS).exp()
}

/// How hard the yeast is working, `0.0..=1.0`. Drives the exotherm, so the beer
/// runs above setpoint exactly while gravity is dropping.
pub fn activity_at(hours: f64) -> f64 {
    let delta = gravity_at(hours) - gravity_at(hours + 1.0);
    (delta / 0.0013).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_sequence() {
        let draw = |seed| {
            let mut rng = Rng::new(seed);
            (0..8).map(|_| rng.noise(1.0)).collect::<Vec<_>>()
        };
        assert_eq!(draw(1), draw(1));
        assert_ne!(draw(1), draw(2));
    }

    #[test]
    fn noise_stays_inside_its_amplitude() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            assert!(rng.noise(0.15).abs() <= 0.15);
        }
    }

    /// A seed of zero must not collapse the generator to a constant.
    #[test]
    fn seed_zero_still_varies() {
        let mut rng = Rng::new(0);
        let first = rng.noise(1.0);
        assert!((1..8).any(|_| rng.noise(1.0) != first));
    }

    #[test]
    fn gravity_lags_then_falls_then_plateaus() {
        assert_eq!(gravity_at(0.0), 1.045);
        assert_eq!(gravity_at(12.0), 1.045);
        assert!(gravity_at(48.0) < 1.030);
        assert!(gravity_at(240.0) < 1.001);
        // Monotonic, and never below the terminal gravity.
        let mut prev = f64::MAX;
        for h in 0..400 {
            let g = gravity_at(h as f64);
            assert!(g <= prev && g >= 1.000, "not monotonic at {h}h");
            prev = g;
        }
    }

    #[test]
    fn activity_peaks_during_attenuation_and_is_zero_at_the_ends() {
        assert_eq!(activity_at(0.0), 0.0);
        assert!(activity_at(14.0) > 0.5);
        assert!(activity_at(300.0) < 0.01);
    }
}
