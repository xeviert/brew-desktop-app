//! Phase 5a. The numbers one batch is compared on: lag, peak rate, days to terminal,
//! the recorded attenuation and ABV, fermentation temperature and nitrogen added.
//!
//! **A value or a reason, never a zero.** A table that prints 0 for "couldn't compute"
//! poisons every average taken over it, and 5d's rollup is exactly such an average. So
//! every metric is a [`Computed`], and a batch that cannot say is counted out rather
//! than in.
//!
//! **Computed on read.** Nothing here is stored: a changed definition — what counts as
//! the end of lag — then applies to every past batch at once.
//!
//! Pure, like the rest of [`crate::calc`]: `db::analytics` supplies the smoothed series,
//! 4c's history and the rows.

use super::{
    abv, attenuation,
    fit::Sums,
    stability::{Evaluation, LAG_DROP, MIN_COVERAGE, MIN_POINTS, Stability},
    yan,
};
use crate::{
    Error, Result,
    units::{Grams, Litres, Sg, Timestamp},
};

/// Six hours: long enough that a fit through a 15 min iSpindle sees two dozen
/// readings, short enough that the steepest part of a seltzer ferment isn't averaged
/// into the slower hours either side of it.
pub const PEAK_WINDOW_DEFAULT: i64 = 6 * 3_600;

const SECS_PER_DAY: f64 = 86_400.0;

/// A metric, or why there isn't one.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Computed<T> {
    Value(T),
    Missing(Missing),
}

impl<T> Computed<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            Self::Missing(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Missing {
    /// No t=0 to measure from.
    NoPitch,
    NoOg,
    NoFg,
    /// `calc` refused the pair: swapped, or an OG within two points of water.
    Implausible,
    /// No gravity bound to the batch at all.
    NoSensorGravity,
    /// Gravity, but only hand-taken: a dozen points can't place a lag or a peak.
    ManualOnly,
    /// The gravity never fell [`LAG_DROP`] below the OG.
    NotStarted,
    /// 4c never called it `Stable`. Still running, stalled, or packaged early — this
    /// can't tell which, and doesn't guess.
    NeverStable,
    /// No window full enough to fit.
    TooFewPoints,
    NoTemp,
    NoVolume,
    /// No priced lot among the additions.
    NoCost,
    /// Priced in more than one currency, which has no total.
    MixedCurrency,
    /// A nutrient addition with no nitrogen yield on its ingredient, or not weighed
    /// in grams. Never a partial sum: an understated total reads as underfeeding.
    NoNutrientYield,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Lag {
    /// The first smoothed reading [`LAG_DROP`] below the OG.
    pub at: Timestamp,
    pub secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct PeakRate {
    /// The steepest fall, as a positive number.
    pub sg_per_day: f64,
    /// The middle of the window it was fitted over: a least-squares slope is the
    /// curve's slope there.
    pub at: Timestamp,
    pub secs_from_pitch: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Terminal {
    /// 4c's `since` at its first `Stable`.
    pub at: Timestamp,
    pub secs_from_pitch: i64,
    pub fg_unchecked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Spread {
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub n: usize,
}

/// Pitch to the first smoothed reading [`LAG_DROP`] below the OG — 4c's end of lag.
pub fn lag(points: &[(Timestamp, f64)], pitch: Timestamp, og: Sg) -> Computed<Lag> {
    let below = og.get() - LAG_DROP;
    points
        .iter()
        .find(|&&(t, v)| t >= pitch && v <= below)
        .map_or(Computed::Missing(Missing::NotStarted), |&(at, _)| {
            Computed::Value(Lag {
                at,
                secs: pitch.secs_until(at),
            })
        })
}

/// The steepest least-squares fall over any `window_secs` of the smoothed series.
///
/// A window counts only when full enough by 4c's rule — [`MIN_POINTS`] and
/// [`MIN_COVERAGE`] — or a fit across a gap would report a cliff that is only the two
/// ends of it.
pub fn peak_rate(
    points: &[(Timestamp, f64)],
    pitch: Timestamp,
    window_secs: i64,
) -> Result<Computed<PeakRate>> {
    if window_secs <= 0 {
        return Err(Error::NotPositive {
            field: "window_secs",
            value: window_secs as f64,
        });
    }
    let Some(&(origin, _)) = points.first() else {
        return Ok(Computed::Missing(Missing::TooFewPoints));
    };
    let min_span = (window_secs as f64 * MIN_COVERAGE).ceil() as i64;
    let x = |t: Timestamp| origin.secs_until(t) as f64 / SECS_PER_DAY;

    let mut sums = Sums::default();
    let mut lo = 0;
    let mut best: Option<(f64, Timestamp)> = None;
    for (i, &(t, value)) in points.iter().enumerate() {
        sums.add(x(t), value - 1.0, 1.0);
        while points[lo].0.secs_until(t) >= window_secs {
            sums.add(x(points[lo].0), points[lo].1 - 1.0, -1.0);
            lo += 1;
        }
        let start = points[lo].0;
        if i + 1 - lo < MIN_POINTS || start.secs_until(t) < min_span {
            continue;
        }
        let fall = -sums.fit(x(t)).0;
        if best.is_none_or(|(b, _)| fall > b) {
            let mid = start.saturating_add_secs(start.secs_until(t) / 2);
            best = Some((fall, mid));
        }
    }
    Ok(match best {
        None => Computed::Missing(Missing::TooFewPoints),
        // A flat line fits a slope of ±1e-15, not zero.
        Some((fall, _)) if fall < 1e-6 => Computed::Missing(Missing::NotStarted),
        Some((sg_per_day, at)) => Computed::Value(PeakRate {
            sg_per_day,
            at,
            secs_from_pitch: pitch.secs_until(at),
        }),
    })
}

/// The `since` of 4c's first `Stable`. A `Stalled` plateau is not a terminal.
pub fn terminal(history: &[Evaluation], pitch: Timestamp) -> Computed<Terminal> {
    history
        .iter()
        .find_map(|e| match e.state {
            Stability::Stable {
                since,
                fg_unchecked,
            } => Some(Terminal {
                at: since,
                secs_from_pitch: pitch.secs_until(since),
                fg_unchecked,
            }),
            _ => None,
        })
        .map_or(Computed::Missing(Missing::NeverStable), Computed::Value)
}

/// Apparent attenuation and Hall's ABV from the *recorded* OG and FG, so finished
/// batches compare on the numbers written down rather than a live reading.
pub fn recorded(og: Option<Sg>, fg: Option<Sg>) -> (Computed<f64>, Computed<f64>) {
    let (og, fg) = match (og, fg) {
        (None, _) => {
            return (
                Computed::Missing(Missing::NoOg),
                Computed::Missing(Missing::NoOg),
            );
        }
        (_, None) => {
            return (
                Computed::Missing(Missing::NoFg),
                Computed::Missing(Missing::NoFg),
            );
        }
        (Some(og), Some(fg)) => (og, fg),
    };
    let or_implausible =
        |r: Result<f64>| r.map_or(Computed::Missing(Missing::Implausible), Computed::Value);
    (
        or_implausible(attenuation::apparent(og, fg)),
        or_implausible(abv::alternate(og, fg)),
    )
}

/// Median, min and max. The median so a cold crash at the end doesn't drag a
/// ferment's temperature down; the range so a controller that swung still shows.
pub fn spread(values: &[f64]) -> Option<Spread> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    };
    Some(Spread {
        median,
        min: sorted[0],
        max: sorted[n - 1],
        n,
    })
}

/// Nitrogen added, mg/L, from each nutrient addition's grams and its product's mg of
/// YAN per gram.
///
/// No additions is a real zero — a batch nobody fed. One addition that can't be
/// counted makes the whole total [`Missing::NoNutrientYield`].
pub fn yan_added(
    lines: &[(Option<Grams>, Option<f64>)],
    volume: Option<Litres>,
) -> Result<Computed<f64>> {
    if lines.is_empty() {
        return Ok(Computed::Value(0.0));
    }
    let mut mg = 0.0;
    for &line in lines {
        let (Some(grams), Some(yield_)) = line else {
            return Ok(Computed::Missing(Missing::NoNutrientYield));
        };
        mg += yan::supplied_mg(grams, yield_)?;
    }
    Ok(match volume {
        Some(v) if v.get() > 0.0 => Computed::Value(mg / v.get()),
        _ => Computed::Missing(Missing::NoVolume),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::{
        smooth::{SG_WINDOW_DEFAULT, rolling_median},
        stability::{ExpectedFg, StabilityParams, stability_history},
        yan::DAP_MG_YAN_PER_G,
    };
    use brewfixture::curve::gravity_at;

    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;
    const OG: f64 = 1.045;
    const PITCH: Timestamp = Timestamp::from_secs(0);

    fn og() -> Sg {
        Sg::new_unchecked(OG)
    }

    /// `f(hours)` every `interval` over `[0, end)`, through 4b's median, as
    /// `db::analytics` gets it.
    fn smoothed(end: i64, interval: i64, f: impl Fn(f64) -> f64) -> Vec<(Timestamp, f64)> {
        let raw: Vec<_> = (0..end)
            .step_by(interval as usize)
            .map(|t| (Timestamp::from_secs(t), f(t as f64 / HOUR as f64)))
            .collect();
        rolling_median(&raw, SG_WINDOW_DEFAULT).unwrap().points
    }

    fn hours(secs: i64) -> f64 {
        secs as f64 / HOUR as f64
    }

    fn value<T: Copy + std::fmt::Debug>(c: Computed<T>) -> T {
        match c {
            Computed::Value(v) => v,
            Computed::Missing(m) => panic!("missing: {m:?}"),
        }
    }

    /// The fixture is flat for 12 h, then `1.000 + 0.045·exp(−(h − 12)/34)`: it
    /// crosses 1.043 at `12 + 34·ln(45/43)` ≈ 13.55 h.
    #[test]
    fn lag_is_where_the_curve_first_falls_two_points() {
        let points = smoothed(12 * DAY, 15 * MIN, gravity_at);
        let got = value(lag(&points, PITCH, og()));
        let want = 12.0 + 34.0 * (45.0_f64 / 43.0).ln();
        assert!(
            (hours(got.secs) - want).abs() <= 0.25,
            "{got:?}, want {want} h"
        );
        assert_eq!(got.at, Timestamp::from_secs(got.secs));
    }

    #[test]
    fn a_curve_that_never_falls_has_not_started() {
        let points = smoothed(3 * DAY, 15 * MIN, |_| OG);
        assert_eq!(
            lag(&points, PITCH, og()),
            Computed::Missing(Missing::NotStarted)
        );
        assert_eq!(
            peak_rate(&points, PITCH, PEAK_WINDOW_DEFAULT).unwrap(),
            Computed::Missing(Missing::NotStarted)
        );
    }

    /// The steepest 6 h is the first 6 h after lag, [12, 18]: a least-squares slope of
    /// an exponential is its slope mid-window, `0.045/34 · e^(−3/34)` per hour.
    #[test]
    fn peak_rate_is_the_first_six_hours_after_lag() {
        let points = smoothed(12 * DAY, 15 * MIN, gravity_at);
        let got = value(peak_rate(&points, PITCH, PEAK_WINDOW_DEFAULT).unwrap());
        let want = 0.045 / 34.0 * 24.0 * (-3.0_f64 / 34.0).exp();
        assert!(
            (got.sg_per_day - want).abs() / want < 0.03,
            "{got:?}, want {want}"
        );
        assert!((hours(got.secs_from_pitch) - 15.0).abs() <= 0.5, "{got:?}");
    }

    /// The Pi was off for the whole ferment: flat at OG, two days of nothing, flat at
    /// FG. A window across the gap holds a few readings either side of the drop and
    /// would fit a cliff; it isn't full enough to count, so no peak is reported.
    #[test]
    fn a_window_across_a_gap_is_not_a_peak() {
        let points: Vec<_> = (0..6 * DAY)
            .step_by(15 * MIN as usize)
            .filter(|t| !(2 * DAY..4 * DAY).contains(t))
            .map(|t| {
                (
                    Timestamp::from_secs(t),
                    if t < 2 * DAY { OG } else { 1.000 },
                )
            })
            .collect();
        assert_eq!(
            peak_rate(&points, PITCH, PEAK_WINDOW_DEFAULT).unwrap(),
            Computed::Missing(Missing::NotStarted)
        );

        let sparse: Vec<_> = smoothed(12 * DAY, 15 * MIN, gravity_at)
            .into_iter()
            .step_by(40)
            .collect();
        assert_eq!(
            peak_rate(&sparse, PITCH, PEAK_WINDOW_DEFAULT).unwrap(),
            Computed::Missing(Missing::TooFewPoints)
        );
    }

    #[test]
    fn terminal_is_the_first_stable_since() {
        let points = smoothed(12 * DAY, 15 * MIN, gravity_at);
        let target = ExpectedFg::resolve(Some(Sg::new_unchecked(1.000)), None, None);
        let history = stability_history(
            &points,
            Some(og()),
            target.as_ref(),
            &StabilityParams::default(),
        )
        .unwrap();
        let first = history
            .iter()
            .find_map(|e| match e.state {
                Stability::Stable { since, .. } => Some(since),
                _ => None,
            })
            .unwrap();
        let got = value(terminal(&history, PITCH));
        assert_eq!(got.at, first);
        assert_eq!(got.secs_from_pitch, first.as_secs());
        assert!(!got.fg_unchecked);
    }

    #[test]
    fn a_stall_is_never_stable() {
        let points = smoothed(12 * DAY, 15 * MIN, |h| {
            1.010 + (gravity_at(h) - 1.000) * (OG - 1.010) / (OG - 1.000)
        });
        let target = ExpectedFg::resolve(Some(Sg::new_unchecked(1.000)), None, None);
        let history = stability_history(
            &points,
            Some(og()),
            target.as_ref(),
            &StabilityParams::default(),
        )
        .unwrap();
        assert_eq!(
            terminal(&history, PITCH),
            Computed::Missing(Missing::NeverStable)
        );
    }

    #[test]
    fn recorded_uses_the_alternate_formula_and_refuses_a_swapped_pair() {
        let fg = Sg::new_unchecked(1.000);
        let (att, abv_alt) = recorded(Some(og()), Some(fg));
        assert_eq!(
            att,
            Computed::Value(attenuation::apparent(og(), fg).unwrap())
        );
        assert_eq!(abv_alt, Computed::Value(abv::alternate(og(), fg).unwrap()));

        let (att, abv_alt) = recorded(Some(fg), Some(og()));
        assert_eq!(att, Computed::Missing(Missing::Implausible));
        assert_eq!(abv_alt, Computed::Missing(Missing::Implausible));

        assert_eq!(recorded(None, Some(fg)).0, Computed::Missing(Missing::NoOg));
        assert_eq!(
            recorded(Some(og()), None).1,
            Computed::Missing(Missing::NoFg)
        );
    }

    /// Eight days at 18 °C, then two of cold crash at 2 °C: the median stays 18.
    #[test]
    fn the_median_temp_ignores_a_cold_crash() {
        let values: Vec<f64> = (0..10 * 24)
            .map(|h| if h < 8 * 24 { 18.0 } else { 2.0 })
            .collect();
        let got = spread(&values).unwrap();
        assert_eq!(got.median, 18.0);
        assert_eq!((got.min, got.max, got.n), (2.0, 18.0, 240));

        assert_eq!(spread(&[1.0, 3.0]).unwrap().median, 2.0);
        assert_eq!(spread(&[]), None);
    }

    #[test]
    fn yan_is_every_addition_or_nothing() {
        let g = |v| Some(Grams::new(v).unwrap());
        let l = |v| Some(Litres::new(v).unwrap());
        let dap = Some(DAP_MG_YAN_PER_G);

        let got = yan_added(&[(g(12.0), dap)], l(20.0)).unwrap();
        assert!((value(got) - 127.2).abs() < 1e-9, "{got:?}");

        let got = yan_added(&[(g(6.0), dap), (g(6.0), dap)], l(20.0)).unwrap();
        assert!((value(got) - 127.2).abs() < 1e-9, "{got:?}");

        assert_eq!(
            yan_added(&[(g(12.0), dap), (g(5.0), None)], l(20.0)).unwrap(),
            Computed::Missing(Missing::NoNutrientYield)
        );
        assert_eq!(
            yan_added(&[(None, dap)], l(20.0)).unwrap(),
            Computed::Missing(Missing::NoNutrientYield)
        );
        assert_eq!(
            yan_added(&[(g(12.0), dap)], None).unwrap(),
            Computed::Missing(Missing::NoVolume)
        );
        assert_eq!(yan_added(&[], None).unwrap(), Computed::Value(0.0));
    }

    #[test]
    fn missing_serialises_as_its_reason() {
        let json = serde_json::to_value(Computed::<f64>::Missing(Missing::NoOg)).unwrap();
        assert_eq!(json, serde_json::json!({ "missing": "no_og" }));
        let json = serde_json::to_value(Computed::Value(1.5)).unwrap();
        assert_eq!(json, serde_json::json!({ "value": 1.5 }));
    }
}
