//! Phase 4b. Smoothing a sensor's gravity: a rolling median over a time window.
//!
//! **A median, not a mean or an EMA.** An iSpindle bumped, or a CO₂ bubble on its
//! hull, reads one wild tilt. A mean smears that tilt across the whole window and an
//! EMA drags it out behind; a median drops it and moves on. The same property keeps a
//! real step — a dilution, a dry-hop — a step: past half a window either side, every
//! value in the window is on one side of it.
//!
//! **Time-based, not count-based.** The iSpindle's interval is a device setting, and
//! changes; "the last eight readings" is two hours one week and forty minutes the next.
//!
//! **What it does not do.** A median of the ~8 readings a 15-minute iSpindle gives in
//! two hours rejects outliers but barely averages jitter: uniform ±0.002 comes out
//! around ±0.0006 RMS. Averaging jitter down is 4c's 24 h least-squares fit, which sees
//! a hundred points. Widening this window to do both would round off the knee of the
//! curve the chart exists to show.
//!
//! **Computed on read, never stored.** Tier 1 stays the raw journal, so a better filter
//! later applies to every past batch for free.
//!
//! Knows nothing about gaps or bindings: a caller passes one contiguous run at a time,
//! since a level can move while the Pi is off and a window must not span that.

use crate::{Error, Result, units::Timestamp};

/// Two hours. Wide enough to hold several readings at the iSpindle's usual 15 minutes,
/// narrow enough that a fast ferment's drop is not rounded off. Like every other
/// default in [`crate::calc`], a caller passes it; nothing here applies it.
pub const SG_WINDOW_DEFAULT: i64 = 2 * 3_600;

#[derive(Debug, Clone, PartialEq)]
pub struct Smoothed {
    /// One per input point, at the same time.
    pub points: Vec<(Timestamp, f64)>,
    /// The point's window ran past the first or last input, so its value is the
    /// median of the side that exists — never extrapolated, and less settled than
    /// its neighbours. The newest points of a live ferment are always partial.
    pub partial: Vec<bool>,
}

/// The median of every point within `window_secs / 2` either side of each point.
///
/// `points` must be strictly ascending by time. Every output lies within the range of
/// the input, since a median is always one of its inputs or the mean of two.
pub fn rolling_median(points: &[(Timestamp, f64)], window_secs: i64) -> Result<Smoothed> {
    if window_secs <= 0 {
        return Err(Error::NotPositive {
            field: "window_secs",
            value: window_secs as f64,
        });
    }
    debug_assert!(
        points.windows(2).all(|w| w[0].0 < w[1].0),
        "not strictly ascending"
    );
    let half = window_secs / 2;
    let (Some(&(first, _)), Some(&(last, _))) = (points.first(), points.last()) else {
        return Ok(Smoothed {
            points: Vec::new(),
            partial: Vec::new(),
        });
    };

    let mut out = Smoothed {
        points: Vec::with_capacity(points.len()),
        partial: Vec::with_capacity(points.len()),
    };
    let mut scratch = Vec::new();
    let (mut lo, mut hi) = (0, 0);
    for &(t, _) in points {
        while points[lo].0.secs_until(t) > half {
            lo += 1;
        }
        while hi < points.len() && t.secs_until(points[hi].0) <= half {
            hi += 1;
        }
        scratch.clear();
        scratch.extend(points[lo..hi].iter().map(|&(_, v)| v));
        out.points.push((t, median(&mut scratch)));
        out.partial
            .push(first.secs_until(t) < half || t.secs_until(last) < half);
    }
    Ok(out)
}

/// Reorders `values`. Even counts take the mean of the two middles.
fn median(values: &mut [f64]) -> f64 {
    let len = values.len();
    let (below, &mut upper, _) = values.select_nth_unstable_by(len / 2, f64::total_cmp);
    if len % 2 == 1 {
        return upper;
    }
    let lower = below
        .iter()
        .copied()
        .max_by(f64::total_cmp)
        .expect("an even, non-empty slice has a lower half");
    (lower + upper) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use brewfixture::curve::Rng;

    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;
    const W: i64 = SG_WINDOW_DEFAULT;

    fn line(end: i64, interval: i64, mut f: impl FnMut(i64) -> f64) -> Vec<(Timestamp, f64)> {
        (0..end)
            .step_by(interval as usize)
            .map(|t| (Timestamp::from_secs(t), f(t)))
            .collect()
    }

    fn interior(s: &Smoothed) -> impl Iterator<Item = f64> + '_ {
        s.points
            .iter()
            .zip(&s.partial)
            .filter(|(_, partial)| !**partial)
            .map(|(&(_, v), _)| v)
    }

    /// Outlier rejection, not jitter averaging — see the module doc. At 5 minutes a
    /// 2 h window holds 25 readings; at 15 minutes the RMS is nearer 0.0006.
    #[test]
    fn jitter_on_a_flat_line_comes_out_near_flat() {
        let mut rng = Rng::new(4);
        let raw = line(4 * DAY, 5 * MIN, |_| 1.010 + rng.noise(0.002));
        let got = rolling_median(&raw, W).unwrap();

        let dev: Vec<f64> = interior(&got).map(|v| v - 1.010).collect();
        let rms = (dev.iter().map(|d| d * d).sum::<f64>() / dev.len() as f64).sqrt();
        let worst = dev.iter().fold(0.0_f64, |m, d| m.max(d.abs()));
        assert!(rms <= 0.0005, "rms {rms}");
        assert!(worst <= 0.0015, "worst {worst}");

        let lo = raw.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        let hi = raw.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        assert!(got.points.iter().all(|&(_, v)| (lo..=hi).contains(&v)));
    }

    #[test]
    fn a_single_outlier_is_removed() {
        let raw = line(
            DAY,
            15 * MIN,
            |t| if t == 10 * HOUR { 1.030 } else { 1.010 },
        );
        let got = rolling_median(&raw, W).unwrap();
        assert!(got.points.iter().all(|&(_, v)| v == 1.010), "{got:?}");
    }

    #[test]
    fn two_adjacent_outliers_are_removed() {
        let raw = line(DAY, 15 * MIN, |t| match t {
            t if t == 10 * HOUR => 1.030,
            t if t == 10 * HOUR + 15 * MIN => 0.990,
            _ => 1.010,
        });
        let got = rolling_median(&raw, W).unwrap();
        assert!(interior(&got).all(|v| v == 1.010), "{got:?}");
    }

    /// A dilution: the level really moves, and must not be ramped across the window.
    #[test]
    fn a_real_step_survives_after_half_a_window() {
        let step = 10 * HOUR;
        let raw = line(DAY, 15 * MIN, |t| if t < step { 1.020 } else { 1.010 });
        let got = rolling_median(&raw, W).unwrap();
        for &(t, v) in &got.points {
            let t = t.as_secs();
            if t + W / 2 < step {
                assert_eq!(v, 1.020, "at {t}");
            } else if t - W / 2 >= step {
                assert_eq!(v, 1.010, "at {t}");
            }
        }
    }

    #[test]
    fn edges_are_marked_partial_and_not_extrapolated() {
        let raw = line(DAY, 15 * MIN, |t| 1.040 - t as f64 * 1e-7);
        let got = rolling_median(&raw, W).unwrap();
        for (&(t, _), &partial) in got.points.iter().zip(&got.partial) {
            let t = t.as_secs();
            let near_an_end = t < W / 2 || t > raw.last().unwrap().0.as_secs() - W / 2;
            assert_eq!(partial, near_an_end, "at {t}");
        }
        // On a falling line the first point's median is of what follows it, so it
        // reads lower than the reading itself. Extrapolating would have matched it.
        assert!(got.points[0].1 < raw[0].1);
        assert!(got.points[0].1 >= raw[4].1);
        assert!(got.points.last().unwrap().1 > raw.last().unwrap().1);
    }

    #[test]
    fn a_single_point_is_itself_and_partial() {
        let raw = [(Timestamp::from_secs(0), 1.012)];
        let got = rolling_median(&raw, W).unwrap();
        assert_eq!(got.points, raw);
        assert_eq!(got.partial, [true]);
    }

    #[test]
    fn empty_is_empty() {
        let got = rolling_median(&[], W).unwrap();
        assert!(got.points.is_empty() && got.partial.is_empty());
    }

    #[test]
    fn a_non_positive_window_is_refused() {
        for window in [0, -1] {
            let err = rolling_median(&[], window).unwrap_err();
            assert_eq!(err.code(), "units.not_positive");
        }
    }

    #[test]
    fn an_even_window_takes_the_mean_of_the_middles() {
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
    }
}
