//! Phase 4c. Has the ferment finished? The function Phase 6's alert calls, and the one
//! the batch screen shows — same inputs, same answer.
//!
//! **Two signals, both required.** The slope of a least-squares fit over a trailing
//! window of the *smoothed* gravity, below a threshold, **and** the fitted gravity
//! within a band of the expected FG. Slope alone reads a stall as a finish; the band is
//! what tells them apart. With no expected FG the batch can still finish — it just
//! cannot stall, and says so with `fg_unchecked`.
//!
//! **The fit is what averages jitter.** 4b's median drops a wild tilt but leaves most of
//! the jitter; a line through a day of readings sees a hundred of them. The band is
//! compared against the fitted value at the newest point for the same reason, not
//! against the newest reading.
//!
//! **Hysteresis.** Even so, the default 0.0005 SG/day sits under what a jittery
//! iSpindle's fit resolves, so a finished batch would flicker back to active for hours.
//! Going flat takes the threshold; leaving flat takes [`LEAVE_FLAT_FACTOR`] times it.
//!
//! **Not before attenuation starts.** A lag phase is flat, and a slope-only detector
//! calls it finished. Until the smoothed gravity has fallen [`LAG_DROP`] below the OG
//! (or, with no OG, below the first reading), the answer is
//! [`Insufficient::NotStarted`]. It latches: once started, a ferment does not un-start.
//!
//! **Replay, not a log.** The detector is pure over history, so [`stability_history`]
//! evaluates it at every point and [`evaluate`] is its last entry. Retuning a threshold
//! shows at once where it would have fired on every past batch — which a log of past
//! evaluations, made under the old threshold, cannot.
//!
//! Knows nothing about bindings, gaps or where the expected FG came from:
//! `db::stability` supplies the series and the band.

use super::fit::Sums;
use crate::{
    Error, Result,
    units::{Sg, Timestamp},
};

/// A day: long enough that a fit sees a hundred readings at the iSpindle's usual
/// 15 minutes, short enough that "finished" is not announced days late.
pub const WINDOW_DEFAULT: i64 = 24 * 3_600;
/// A hydrometer can't resolve less than this over a day; neither can the yeast be
/// said to be working.
pub const SLOPE_SG_PER_DAY_DEFAULT: f64 = 0.0005;
/// How far above the expected FG still counts as there.
pub const FG_BAND_DEFAULT: f64 = 0.003;

/// A fit through fewer is noise with a slope.
pub const MIN_POINTS: usize = 6;
/// The window's readings must span at least this fraction of it. A window just after
/// a gap holds a few hours of readings, and a day's threshold does not apply to them.
pub const MIN_COVERAGE: f64 = 0.75;
/// How far below the OG the smoothed gravity must fall before the ferment has started.
/// Two points, a hydrometer's resolution — and 5a's definition of the end of lag.
pub const LAG_DROP: f64 = 0.002;
/// Once flat, the slope must reach this multiple of the threshold to count as active
/// again. A day's fit through 4b's median of a ±0.002 iSpindle at 5 minutes still
/// wanders by 0.00036 SG/day RMS, and for hours at a time: over 32 seeds × 8 flat days
/// it passed 1× the default threshold for up to 13 h, 2× for 4 h, and peaked at 0.0017.
/// A restarted ferment is well above 4×; jitter is not.
pub const LEAVE_FLAT_FACTOR: f64 = 4.0;

const SECS_PER_DAY: f64 = 86_400.0;

/// The three numbers `config::Stability` carries. Every one is an argument, as
/// everywhere else in [`crate::calc`]; nothing here reads the config.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StabilityParams {
    pub window_secs: i64,
    pub slope_sg_per_day: f64,
    pub fg_band: f64,
}

impl Default for StabilityParams {
    fn default() -> Self {
        Self {
            window_secs: WINDOW_DEFAULT,
            slope_sg_per_day: SLOPE_SG_PER_DAY_DEFAULT,
            fg_band: FG_BAND_DEFAULT,
        }
    }
}

impl StabilityParams {
    fn check(&self) -> Result<()> {
        if self.window_secs <= 0 {
            return Err(Error::NotPositive {
                field: "window_secs",
                value: self.window_secs as f64,
            });
        }
        for (field, value) in [
            ("slope_sg_per_day", self.slope_sg_per_day),
            ("fg_band", self.fg_band),
        ] {
            if !value.is_finite() {
                return Err(Error::NotFinite { field, value });
            }
            if value <= 0.0 {
                return Err(Error::NotPositive { field, value });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FgSource {
    /// `recipe_version.target_fg`.
    Recipe,
    /// `batch.og` and the yeast strain's attenuation band.
    Strain,
}

/// Where the ferment should end, as a band. A recipe target is a band of one.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExpectedFg {
    pub lo: Sg,
    pub hi: Sg,
    pub source: FgSource,
}

impl ExpectedFg {
    /// The recipe's target, else the OG attenuated by the strain's `(min, max)`
    /// apparent attenuation, else nothing.
    ///
    /// Only `hi` decides a stall: a batch that finishes drier than expected has still
    /// finished. `lo` is there for the card to show the band.
    pub fn resolve(
        target_fg: Option<Sg>,
        og: Option<Sg>,
        attenuation: Option<(f64, f64)>,
    ) -> Option<Self> {
        if let Some(fg) = target_fg {
            return Some(Self {
                lo: fg,
                hi: fg,
                source: FgSource::Recipe,
            });
        }
        let (og, (a, b)) = og.zip(attenuation)?;
        let at = |attenuation: f64| Sg::new_unchecked(og.get() - (og.get() - 1.0) * attenuation);
        Some(Self {
            lo: at(a.max(b)),
            hi: at(a.min(b)),
            source: FgSource::Strain,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Insufficient {
    /// No sensor gravity at all.
    Empty,
    /// Fewer than [`MIN_POINTS`] in the window.
    TooFewPoints,
    /// The window's readings span less than [`MIN_COVERAGE`] of it.
    UnderFilled,
    /// Still in the lag phase — see the module doc.
    NotStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Stability {
    Active,
    /// Flat and at the expected FG, or flat with none to check against.
    Stable {
        since: Timestamp,
        fg_unchecked: bool,
    },
    /// Flat, but above the expected FG's band.
    Stalled {
        since: Timestamp,
    },
    Insufficient {
        reason: Insufficient,
    },
}

/// The detector's answer at one reading, and what it was computed from.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Evaluation {
    pub at: Timestamp,
    pub state: Stability,
    /// `None` when the window was too sparse to fit.
    pub slope_sg_per_day: Option<f64>,
    /// The fit's value at `at`.
    pub fitted_sg: Option<f64>,
}

/// The detector evaluated at every point of `points`, each over the window trailing it.
///
/// `points` is a smoothed gravity series, strictly ascending, gaps dropped. `since` is
/// the first flat evaluation after the last active one, found by this walk — not the
/// time anyone happened to ask. An insufficient window in between does not reset it:
/// only the slope crossing [`LEAVE_FLAT_FACTOR`] times the threshold does.
pub fn stability_history(
    points: &[(Timestamp, f64)],
    og: Option<Sg>,
    expected: Option<&ExpectedFg>,
    params: &StabilityParams,
) -> Result<Vec<Evaluation>> {
    params.check()?;
    debug_assert!(
        points.windows(2).all(|w| w[0].0 < w[1].0),
        "not strictly ascending"
    );
    let Some(&(origin, first)) = points.first() else {
        return Ok(Vec::new());
    };

    let started_below = og.map_or(first, Sg::get) - LAG_DROP;
    let min_span = (params.window_secs as f64 * MIN_COVERAGE).ceil() as i64;
    // Days from the first point, and gravity less one, so the sums stay small.
    let x = |t: Timestamp| origin.secs_until(t) as f64 / SECS_PER_DAY;

    let mut out = Vec::with_capacity(points.len());
    let mut sums = Sums::default();
    let mut lo = 0;
    let mut started = false;
    let mut flat_since = None;
    for (i, &(t, value)) in points.iter().enumerate() {
        sums.add(x(t), value - 1.0, 1.0);
        while points[lo].0.secs_until(t) >= params.window_secs {
            sums.add(x(points[lo].0), points[lo].1 - 1.0, -1.0);
            lo += 1;
        }
        started |= value <= started_below;

        let insufficient = |reason| Evaluation {
            at: t,
            state: Stability::Insufficient { reason },
            slope_sg_per_day: None,
            fitted_sg: None,
        };
        if i + 1 - lo < MIN_POINTS {
            out.push(insufficient(Insufficient::TooFewPoints));
            continue;
        }
        if points[lo].0.secs_until(t) < min_span {
            out.push(insufficient(Insufficient::UnderFilled));
            continue;
        }
        let (slope, fitted) = sums.fit(x(t));
        let fitted = fitted + 1.0;

        let state = if !started {
            Stability::Insufficient {
                reason: Insufficient::NotStarted,
            }
        } else if slope.abs()
            >= params.slope_sg_per_day * flat_since.map_or(1.0, |_| LEAVE_FLAT_FACTOR)
        {
            flat_since = None;
            Stability::Active
        } else {
            let since = *flat_since.get_or_insert(t);
            match expected {
                None => Stability::Stable {
                    since,
                    fg_unchecked: true,
                },
                Some(e) if fitted <= e.hi.get() + params.fg_band => Stability::Stable {
                    since,
                    fg_unchecked: false,
                },
                Some(_) => Stability::Stalled { since },
            }
        };
        out.push(Evaluation {
            at: t,
            state,
            slope_sg_per_day: Some(slope),
            fitted_sg: Some(fitted),
        });
    }
    Ok(out)
}

/// The detector at the newest point: [`stability_history`]'s last entry, since `since`
/// needs the walk anyway.
pub fn evaluate(
    points: &[(Timestamp, f64)],
    og: Option<Sg>,
    expected: Option<&ExpectedFg>,
    params: &StabilityParams,
) -> Result<Stability> {
    Ok(stability_history(points, og, expected, params)?
        .last()
        .map_or(
            Stability::Insufficient {
                reason: Insufficient::Empty,
            },
            |e| e.state,
        ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlatState {
    Stable,
    Stalled,
}

/// A run of consecutive evaluations in one flat state, from the first's `at` to the
/// last's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FlatSpan {
    pub start: Timestamp,
    pub end: Timestamp,
    pub state: FlatState,
}

/// [`stability_history`] collapsed into the spans a chart shades. A history is one
/// entry per raw reading; the spans are a handful.
///
/// An insufficient evaluation ends a span even though it doesn't reset `since`: the
/// shading shows where the detector could say so, not where it inferred it.
pub fn flat_spans(history: &[Evaluation]) -> Vec<FlatSpan> {
    let mut out: Vec<FlatSpan> = Vec::new();
    let mut open = false;
    for e in history {
        let state = match e.state {
            Stability::Stable { .. } => FlatState::Stable,
            Stability::Stalled { .. } => FlatState::Stalled,
            _ => {
                open = false;
                continue;
            }
        };
        match out.last_mut() {
            Some(last) if open && last.state == state => last.end = e.at,
            _ => out.push(FlatSpan {
                start: e.at,
                end: e.at,
                state,
            }),
        }
        open = true;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::smooth::{SG_WINDOW_DEFAULT, rolling_median};
    use brewfixture::curve::{Rng, gravity_at};

    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;
    const OG: f64 = 1.045;

    fn params() -> StabilityParams {
        StabilityParams::default()
    }

    fn og() -> Option<Sg> {
        Some(Sg::new_unchecked(OG))
    }

    fn target(fg: f64) -> ExpectedFg {
        ExpectedFg::resolve(Some(Sg::new_unchecked(fg)), None, None).unwrap()
    }

    /// `f(hours)` every `interval` seconds over `[0, end)`, plus uniform jitter, through
    /// 4b's median — the pipeline `db::stability` runs.
    fn smoothed(
        end: i64,
        interval: i64,
        jitter: f64,
        seed: u64,
        f: impl Fn(f64) -> f64,
    ) -> Vec<(Timestamp, f64)> {
        let mut rng = Rng::new(seed);
        let raw: Vec<_> = (0..end)
            .step_by(interval as usize)
            .map(|t| {
                let noise = if jitter > 0.0 { rng.noise(jitter) } else { 0.0 };
                (Timestamp::from_secs(t), f(t as f64 / HOUR as f64) + noise)
            })
            .collect();
        rolling_median(&raw, SG_WINDOW_DEFAULT).unwrap().points
    }

    fn first(history: &[Evaluation], pred: impl Fn(&Stability) -> bool) -> Option<&Evaluation> {
        history.iter().find(|e| pred(&e.state))
    }

    fn hours(t: Timestamp) -> f64 {
        t.as_secs() as f64 / HOUR as f64
    }

    /// The fixture curve is `FG + (OG − FG)·exp(−(h − 12)/34)`. A day's least-squares
    /// slope of an exponential is its slope at the window's middle, to within
    /// `(w/2τ)²/10` ≈ 1 % here — so the fit crosses the threshold where the true slope
    /// half a window back does, give or take half an hour.
    fn crossing_hour(lag: f64, tau: f64, drop: f64, window_hours: f64) -> f64 {
        let peak_per_day = drop / tau * 24.0;
        lag + window_hours / 2.0 + tau * (peak_per_day / SLOPE_SG_PER_DAY_DEFAULT).ln()
    }

    #[test]
    fn lag_then_active_then_plateau_reads_stable_at_the_right_hour() {
        let points = smoothed(12 * DAY, 15 * MIN, 0.0, 0, gravity_at);
        let history = stability_history(&points, og(), Some(&target(1.000)), &params()).unwrap();

        let active = first(&history, |s| *s == Stability::Active).expect("never active");
        let stable =
            first(&history, |s| matches!(s, Stability::Stable { .. })).expect("never stable");
        assert!(active.at < stable.at);

        let want = crossing_hour(12.0, 34.0, 0.045, 24.0);
        let got = hours(stable.at);
        assert!(
            (got - want).abs() <= 1.0,
            "stable at {got} h, want {want} h"
        );
        assert_eq!(
            stable.state,
            Stability::Stable {
                since: stable.at,
                fg_unchecked: false
            }
        );
        // And it stays there.
        let after = history.iter().skip_while(|e| e.at < stable.at);
        assert!(
            after.clone().all(|e| e.state == stable.state),
            "flipped back"
        );
        assert_eq!(
            evaluate(&points, og(), Some(&target(1.000)), &params()).unwrap(),
            stable.state
        );
    }

    #[test]
    fn a_plateau_ten_points_above_the_expected_fg_is_stalled() {
        let points = smoothed(12 * DAY, 15 * MIN, 0.0, 0, |h| {
            1.010 + (gravity_at(h) - 1.000) * (OG - 1.010) / (OG - 1.000)
        });
        let got = evaluate(&points, og(), Some(&target(1.000)), &params()).unwrap();
        assert!(matches!(got, Stability::Stalled { .. }), "{got:?}");

        // The same curve with nothing to compare against has simply finished.
        let got = evaluate(&points, og(), None, &params()).unwrap();
        assert!(
            matches!(
                got,
                Stability::Stable {
                    fg_unchecked: true,
                    ..
                }
            ),
            "{got:?}"
        );
    }

    /// Within the band is finished, and so is below it: a drier finish than the recipe
    /// expected is a result, not a stall.
    #[test]
    fn the_band_is_one_sided() {
        let points = smoothed(12 * DAY, 15 * MIN, 0.0, 0, gravity_at);
        for fg in [1.000, 0.998, 1.010, 0.990] {
            let got = evaluate(&points, og(), Some(&target(fg)), &params()).unwrap();
            let within = 1.000 <= fg + FG_BAND_DEFAULT;
            assert_eq!(
                matches!(got, Stability::Stable { .. }),
                within,
                "target {fg}: {got:?}"
            );
        }
    }

    /// The jitter 4b could not average out, through a whole ferment and a week of
    /// plateau. 4b's convention: ±0.002 at 5 minutes. At 15 minutes the fit wanders
    /// past 4× the default threshold too — see [`LEAVE_FLAT_FACTOR`].
    #[test]
    fn a_jittered_plateau_never_flips_to_active() {
        for seed in 1..=8 {
            let points = smoothed(14 * DAY, 5 * MIN, 0.002, seed, gravity_at);
            let history =
                stability_history(&points, og(), Some(&target(1.000)), &params()).unwrap();
            let stable = first(&history, |s| matches!(s, Stability::Stable { .. }))
                .unwrap_or_else(|| panic!("seed {seed}: never stable"));
            assert!(
                hours(stable.at) > 120.0,
                "seed {seed}: stable at {}",
                stable.at
            );

            let after = history.iter().skip_while(|e| e.at < stable.at);
            let active = after.filter(|e| e.state == Stability::Active).count();
            assert_eq!(
                active, 0,
                "seed {seed}: active {active} times after going flat"
            );
            assert_eq!(history.last().unwrap().state, stable.state, "seed {seed}");
        }
    }

    /// The case a slope-only detector gets wrong: two flat days before the yeast wakes
    /// up, with no expected FG to fall back on.
    #[test]
    fn the_lag_phase_is_not_stable() {
        let lag = 48.0;
        let points = smoothed(12 * DAY, 15 * MIN, 0.0, 0, |h| gravity_at(h - lag + 12.0));

        for og in [og(), None] {
            let history = stability_history(&points, og, None, &params()).unwrap();
            let lagging = history.iter().take_while(|e| hours(e.at) < lag);
            assert!(
                lagging
                    .clone()
                    .all(|e| !matches!(e.state, Stability::Stable { .. })),
                "og {og:?}"
            );
            assert!(lagging.clone().any(|e| e.state
                == Stability::Insufficient {
                    reason: Insufficient::NotStarted
                }));
            assert!(first(&history, |s| *s == Stability::Active).is_some());
            assert!(matches!(
                history.last().unwrap().state,
                Stability::Stable { .. }
            ));
        }
    }

    /// Asked at any hour after the crossing, the answer's `since` is the crossing —
    /// the time the ferment went flat, not the time anyone looked.
    #[test]
    fn since_is_the_crossing_however_late_it_is_asked() {
        let points = smoothed(12 * DAY, 15 * MIN, 0.0, 0, gravity_at);
        let expected = target(1.000);
        let history = stability_history(&points, og(), Some(&expected), &params()).unwrap();
        let crossing = first(&history, |s| matches!(s, Stability::Stable { .. }))
            .unwrap()
            .at;

        for end in [
            crossing.as_secs() + 1,
            9 * DAY,
            10 * DAY + 7 * HOUR,
            12 * DAY,
        ] {
            let live: Vec<_> = points
                .iter()
                .copied()
                .filter(|p| p.0.as_secs() < end)
                .collect();
            assert_eq!(
                evaluate(&live, og(), Some(&expected), &params()).unwrap(),
                Stability::Stable {
                    since: crossing,
                    fg_unchecked: false
                },
                "asked at {end}"
            );
        }
    }

    #[test]
    fn a_window_under_six_points_is_insufficient() {
        let points: Vec<_> = (0..5)
            .map(|i| (Timestamp::from_secs(i * 5 * HOUR), 1.000))
            .collect();
        let got = evaluate(&points, og(), None, &params()).unwrap();
        assert_eq!(
            got,
            Stability::Insufficient {
                reason: Insufficient::TooFewPoints
            }
        );
    }

    /// Readings resume after two days off: the window holds only what came since, and a
    /// day's threshold does not apply to a few hours of them.
    #[test]
    fn a_window_refilling_after_a_gap_is_insufficient_until_three_quarters_full() {
        let before = smoothed(8 * DAY, 15 * MIN, 0.0, 0, gravity_at);
        let resume = 10 * DAY;
        let points: Vec<_> = before
            .iter()
            .copied()
            .chain(
                (0..DAY)
                    .step_by(15 * MIN as usize)
                    .map(|t| (Timestamp::from_secs(resume + t), 1.000)),
            )
            .collect();
        let history = stability_history(&points, og(), None, &params()).unwrap();

        let stable_before = history
            .iter()
            .rfind(|e| e.at.as_secs() < resume)
            .unwrap()
            .state;
        assert!(matches!(stable_before, Stability::Stable { .. }));
        for e in history.iter().filter(|e| e.at.as_secs() >= resume) {
            let filled = e.at.as_secs() - resume >= DAY * 3 / 4;
            if filled {
                // Still flat, so `since` survived the gap.
                assert_eq!(e.state, stable_before, "at {}", e.at);
            } else {
                assert_ne!(e.state, Stability::Active);
                assert!(
                    matches!(e.state, Stability::Insufficient { .. }),
                    "at {}: {:?}",
                    e.at,
                    e.state
                );
            }
        }
    }

    /// A rising gravity is not a flat one: sugar went in.
    #[test]
    fn a_rising_gravity_is_active() {
        let points = smoothed(12 * DAY, 15 * MIN, 0.0, 0, |h| {
            gravity_at(h) + if h > 240.0 { (h - 240.0) * 0.0005 } else { 0.0 }
        });
        let got = evaluate(&points, og(), None, &params()).unwrap();
        assert_eq!(got, Stability::Active);
    }

    /// Sixty days at 30 s, far from the origin, against a fit done from scratch.
    #[test]
    fn the_sliding_fit_matches_a_direct_one() {
        let mut rng = Rng::new(9);
        let points: Vec<_> = (0..60 * DAY)
            .step_by(30)
            .map(|t| {
                let h = t as f64 / HOUR as f64;
                (Timestamp::from_secs(t), gravity_at(h) + rng.noise(0.001))
            })
            .collect();
        let history = stability_history(&points, og(), None, &params()).unwrap();

        for i in (DAY as usize / 30..points.len()).step_by(997) {
            let (t, _) = points[i];
            let window: Vec<_> = points[..=i]
                .iter()
                .filter(|p| p.0.secs_until(t) < WINDOW_DEFAULT)
                .map(|&(p, v)| (p.as_secs() as f64 / SECS_PER_DAY, v))
                .collect();
            let n = window.len() as f64;
            let mx = window.iter().map(|p| p.0).sum::<f64>() / n;
            let my = window.iter().map(|p| p.1).sum::<f64>() / n;
            let sxy: f64 = window.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
            let sxx: f64 = window.iter().map(|p| (p.0 - mx).powi(2)).sum();
            let slope = sxy / sxx;
            let fitted = my + slope * (t.as_secs() as f64 / SECS_PER_DAY - mx);

            let got = history[i];
            assert!(
                (got.slope_sg_per_day.unwrap() - slope).abs() < 1e-9,
                "at {i}"
            );
            assert!((got.fitted_sg.unwrap() - fitted).abs() < 1e-9, "at {i}");
        }
    }

    #[test]
    fn empty_is_insufficient() {
        assert!(
            stability_history(&[], og(), None, &params())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            evaluate(&[], og(), None, &params()).unwrap(),
            Stability::Insufficient {
                reason: Insufficient::Empty
            }
        );
    }

    #[test]
    fn flat_spans_collapse_runs_and_break_on_anything_else() {
        let t = |h: i64| Timestamp::from_secs(h * 3_600);
        let since = t(1);
        let eval = |h: i64, state: Stability| Evaluation {
            at: t(h),
            state,
            slope_sg_per_day: None,
            fitted_sg: None,
        };
        let stable = Stability::Stable {
            since,
            fg_unchecked: false,
        };
        let stalled = Stability::Stalled { since };
        let gap = Stability::Insufficient {
            reason: Insufficient::UnderFilled,
        };
        let history = [
            eval(0, Stability::Active),
            eval(1, stalled),
            eval(2, stalled),
            eval(3, stable),
            eval(4, stable),
            eval(5, gap),
            eval(6, stable),
            eval(7, Stability::Active),
            eval(8, stable),
        ];
        let span = |start, end, state| FlatSpan {
            start: t(start),
            end: t(end),
            state,
        };
        assert_eq!(
            flat_spans(&history),
            vec![
                span(1, 2, FlatState::Stalled),
                span(3, 4, FlatState::Stable),
                span(6, 6, FlatState::Stable),
                span(8, 8, FlatState::Stable),
            ]
        );
        assert!(flat_spans(&[]).is_empty());
    }

    #[test]
    fn the_recipe_target_wins_over_the_strain() {
        let og = Some(Sg::new_unchecked(1.050));
        let band = Some((0.80, 0.90));

        let recipe = ExpectedFg::resolve(Some(Sg::new_unchecked(1.002)), og, band).unwrap();
        assert_eq!(recipe.source, FgSource::Recipe);
        assert_eq!((recipe.lo.get(), recipe.hi.get()), (1.002, 1.002));

        let strain = ExpectedFg::resolve(None, og, band).unwrap();
        assert_eq!(strain.source, FgSource::Strain);
        assert!((strain.lo.get() - 1.005).abs() < 1e-12, "{strain:?}");
        assert!((strain.hi.get() - 1.010).abs() < 1e-12, "{strain:?}");

        assert_eq!(ExpectedFg::resolve(None, None, band), None);
        assert_eq!(ExpectedFg::resolve(None, og, None), None);
    }

    #[test]
    fn bad_params_are_refused() {
        let cases = [
            (
                StabilityParams {
                    window_secs: 0,
                    ..params()
                },
                "units.not_positive",
            ),
            (
                StabilityParams {
                    slope_sg_per_day: -0.001,
                    ..params()
                },
                "units.not_positive",
            ),
            (
                StabilityParams {
                    fg_band: f64::NAN,
                    ..params()
                },
                "units.not_finite",
            ),
        ];
        for (bad, code) in cases {
            let err = stability_history(&[], og(), None, &bad).unwrap_err();
            assert_eq!(err.code(), code, "{bad:?}");
        }
    }
}
