//! The rules: one [`Watch`] and the buffers in, a verdict per rule out.
//!
//! Each rule answers raise, clear, or can't tell. Can't tell holds whatever the
//! [`super::AlertBook`] already has — a window too sparse to fit says nothing about a
//! ferment that was finished an hour ago. The gap between a rule's raise and clear
//! conditions is its hysteresis.

use crate::{
    brewd::dialect::column_metric,
    calc::{
        gravity::plato_to_sg,
        health::{self, SilenceGuess},
        smooth::{SG_WINDOW_DEFAULT, smooth_runs},
        stability::{Stability, stability_history},
    },
    config::IspindelUnit,
    db::Metric,
    units::Timestamp,
};

use super::{Buffers, Rule, Watch, buffer::DeviceBuffer};

/// How long a temperature must stay outside its band before it raises, so one spike —
/// a probe lifted out of the thermowell, the fridge door — doesn't.
pub const TEMP_CONFIRM_SECS: i64 = 600;
/// How far back inside the band a temperature must come to clear, so a reading
/// hovering at the edge raises once rather than at every crossing.
pub const TEMP_CLEAR_MARGIN_C: f64 = 0.3;
/// The least quiet a device must be before [`Rule::Silent`] raises, over 4f's stale
/// threshold. A 5 min iSpindle on weak Wi-Fi drops a send or two most hours; the
/// Home card still says so at 15 min, but a push is for a device that sank or died.
pub const SILENT_ALERT_FLOOR_SECS: i64 = 3600;
/// How long a band from a newly started schedule step waits for the beer to get into
/// it before judging anyway. A diacetyl rest free-rises over a day; a crash takes one.
pub const TEMP_ARM_SECS: i64 = DAY;

const DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Raise {
        detail: String,
    },
    Clear,
    /// Not enough to say either way. The alert, if any, stays as it is.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub rule: Rule,
    /// The device, for [`Rule::Silent`]; `None` for every other rule.
    pub device: Option<String>,
    pub verdict: Verdict,
}

/// Every rule the watch enables. A rule the watch has no inputs for is left out, which
/// [`super::AlertBook::step`] reads as "no longer applies" and clears.
pub fn evaluate(watch: &Watch, buffers: &Buffers, now: Timestamp) -> Vec<Condition> {
    let mut out = Vec::new();
    gravity(watch, buffers, &mut out);
    temperature(watch, buffers, &mut out);
    silence(watch, buffers, now, &mut out);
    out
}

fn push(out: &mut Vec<Condition>, rule: Rule, verdict: Verdict) {
    out.push(Condition {
        rule,
        device: None,
        verdict,
    });
}

fn gravity(watch: &Watch, buffers: &Buffers, out: &mut Vec<Condition>) {
    let Some(feed) = &watch.gravity else { return };
    let raw: Vec<(Timestamp, f64)> = buffers
        .get(&feed.device)
        .map(|b| b.series(&feed.column, feed.since))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(t, v)| Some((t, to_sg(v, feed.unit)?)))
        .collect();
    let smoothed = smooth(&raw);

    let newest = stability_history(
        &smoothed,
        watch.og,
        watch.expected_fg.as_ref(),
        &watch.stability,
    )
    .ok()
    .and_then(|h| h.last().copied());
    let (finished, stalled) = match newest {
        None => (Verdict::Unknown, Verdict::Unknown),
        Some(e) => {
            let sg = e
                .fitted_sg
                .map_or_else(String::new, |sg| format!("SG {sg:.3}, "));
            let flat = |since: Timestamp| format!("{sg}flat for {}", span(since.secs_until(e.at)));
            match e.state {
                Stability::Active => (Verdict::Clear, Verdict::Clear),
                Stability::Stable {
                    since,
                    fg_unchecked,
                } => {
                    let unchecked = if fg_unchecked {
                        " — no expected FG to check it against"
                    } else {
                        ""
                    };
                    let detail = format!("{}{unchecked}", flat(since));
                    (Verdict::Raise { detail }, Verdict::Clear)
                }
                Stability::Stalled { since } => {
                    let above = watch.expected_fg.map_or_else(String::new, |fg| {
                        format!(", above the expected {:.3}", fg.hi.get())
                    });
                    let detail = format!("{}{above}", flat(since));
                    (Verdict::Clear, Verdict::Raise { detail })
                }
                Stability::Insufficient { .. } => (Verdict::Unknown, Verdict::Unknown),
            }
        }
    };
    push(out, Rule::Finished, finished);
    push(out, Rule::Stalled, stalled);

    // Latched: once through, a later wobble back above isn't news.
    let latest = smoothed.last().map(|&(_, v)| v);
    let reached = |mark: f64, what: &str| match latest {
        Some(v) if v <= mark => Verdict::Raise {
            detail: format!("SG {v:.3}, {what} {mark:.3}"),
        },
        _ => Verdict::Unknown,
    };
    if let (Some(og), Some(fg)) = (watch.og, watch.expected_fg) {
        let (og, fg) = (og.get(), (fg.lo.get() + fg.hi.get()) / 2.0);
        if fg < og {
            push(
                out,
                Rule::SugarBreak,
                reached(og - (og - fg) / 3.0, "⅓ break at"),
            );
        }
    }
    if let Some(target) = watch.target_sg {
        push(out, Rule::TargetSg, reached(target.get(), "target"));
    }
}

fn to_sg(value: f64, unit: IspindelUnit) -> Option<f64> {
    match unit {
        IspindelUnit::Sg => Some(value),
        IspindelUnit::Plato => plato_to_sg(value).ok().map(|sg| sg.get()),
    }
}

/// 4b's smoothing, each run between gaps on its own, by the gap rule `db::series`
/// uses: a step over 3× the median interval.
fn smooth(raw: &[(Timestamp, f64)]) -> Vec<(Timestamp, f64)> {
    let times: Vec<Timestamp> = raw.iter().map(|&(t, _)| t).collect();
    let gap = health::median_interval(&times).map_or(i64::MAX, |m| m.saturating_mul(3));
    smooth_runs(raw, gap, SG_WINDOW_DEFAULT)
        .map(|runs| runs.into_iter().flat_map(|(_, s)| s.points).collect())
        .unwrap_or_default()
}

fn temperature(watch: &Watch, buffers: &Buffers, out: &mut Vec<Condition>) {
    let Some(feed) = &watch.temp else { return };
    let mut points = buffers
        .get(&feed.device)
        .map(|b| b.series(&feed.column, feed.since))
        .unwrap_or_default();
    let inside = |v: f64| {
        watch.temp_min_c.is_none_or(|lo| v >= lo) && watch.temp_max_c.is_none_or(|hi| v <= hi)
    };
    if let Some(since) = watch.band_since {
        let deadline = since.saturating_add_secs(TEMP_ARM_SECS);
        let armed = points
            .iter()
            .find(|&&(t, v)| t >= since && (inside(v) || t >= deadline))
            .map(|&(t, _)| t);
        // Not there yet: whatever the old band raised no longer applies.
        let Some(armed) = armed else {
            for (rule, end) in [
                (Rule::TempHigh, watch.temp_max_c),
                (Rule::TempLow, watch.temp_min_c),
            ] {
                if end.is_some() {
                    push(out, rule, Verdict::Clear);
                }
            }
            push(out, Rule::TempRate, Verdict::Clear);
            return;
        };
        points.retain(|&(t, _)| t >= armed);
    }
    if let Some(hi) = watch.temp_max_c {
        let verdict = band(
            &points,
            |v| v > hi,
            |v| v <= hi - TEMP_CLEAR_MARGIN_C,
            "above",
            hi,
        );
        push(out, Rule::TempHigh, verdict);
    }
    if let Some(lo) = watch.temp_min_c {
        let verdict = band(
            &points,
            |v| v < lo,
            |v| v >= lo + TEMP_CLEAR_MARGIN_C,
            "below",
            lo,
        );
        push(out, Rule::TempLow, verdict);
    }
    push(out, Rule::TempRate, rate(watch, &points, inside));
}

/// Raised when the temperature moved more than the set change within the set window
/// while staying in the band. Over a rolling median of [`TEMP_CONFIRM_SECS`], so a
/// lifted probe doesn't count, and only since the reading came into the band, so a
/// ramp into a new one doesn't either. Out of the band is the band rules' to say.
fn rate(watch: &Watch, points: &[(Timestamp, f64)], inside: impl Fn(f64) -> bool) -> Verdict {
    let Some(&(newest, _)) = points.last() else {
        return Verdict::Unknown;
    };
    let p = &watch.params;
    let from = newest.saturating_add_secs(-p.temp_rate_window_secs);
    let raw = &points
        [points.partition_point(|&(t, _)| t < from.saturating_add_secs(-TEMP_CONFIRM_SECS))..];
    let smoothed: Vec<(Timestamp, f64)> = raw
        .iter()
        .enumerate()
        .filter(|&(_, &(t, _))| t >= from)
        .map(|(i, &(t, _))| {
            let lo = raw.partition_point(|&(u, _)| u < t.saturating_add_secs(-TEMP_CONFIRM_SECS));
            (t, median(raw[lo..=i].iter().map(|&(_, v)| v).collect()))
        })
        .collect();
    let in_band = smoothed
        .iter()
        .rposition(|&(_, v)| !inside(v))
        .map_or(&smoothed[..], |i| &smoothed[i + 1..]);
    let by_value = |a: &&(Timestamp, f64), b: &&(Timestamp, f64)| a.1.total_cmp(&b.1);
    let (Some(lo), Some(hi)) = (
        in_band.iter().min_by(by_value),
        in_band.iter().max_by(by_value),
    ) else {
        return Verdict::Unknown;
    };
    let delta = hi.1 - lo.1;
    if delta > p.temp_rate_delta_c {
        let (a, b, word) = if lo.0 < hi.0 {
            (lo, hi, "rose")
        } else {
            (hi, lo, "fell")
        };
        Verdict::Raise {
            detail: format!(
                "{word} {delta:.1} °C in {} ({:.1} → {:.1} °C)",
                span(a.0.secs_until(b.0)),
                a.1,
                b.1
            ),
        }
    } else if delta <= p.temp_rate_delta_c - TEMP_CLEAR_MARGIN_C {
        Verdict::Clear
    } else {
        Verdict::Unknown
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// Raised once the trailing run of readings outside the band spans
/// [`TEMP_CONFIRM_SECS`]; cleared once the newest is back inside by the margin.
fn band(
    points: &[(Timestamp, f64)],
    outside: impl Fn(f64) -> bool,
    back: impl Fn(f64) -> bool,
    word: &str,
    limit: f64,
) -> Verdict {
    let Some(&(newest, v)) = points.last() else {
        return Verdict::Unknown;
    };
    if outside(v) {
        let start = points
            .iter()
            .rev()
            .take_while(|&&(_, v)| outside(v))
            .last()
            .map_or(newest, |&(t, _)| t);
        let held = start.secs_until(newest);
        if held >= TEMP_CONFIRM_SECS {
            return Verdict::Raise {
                detail: format!("{v:.1} °C, {word} {limit:.1} °C for {}", span(held)),
            };
        }
        return Verdict::Unknown;
    }
    if back(v) {
        Verdict::Clear
    } else {
        Verdict::Unknown
    }
}

fn silence(watch: &Watch, buffers: &Buffers, now: Timestamp, out: &mut Vec<Condition>) {
    for device in &watch.devices {
        let Some(buffer) = buffers.get(device) else {
            continue;
        };
        let times = buffer.times();
        let Some(&newest) = times.last() else {
            continue;
        };
        let day = &times[times.partition_point(|&t| t < newest.saturating_add_secs(-DAY))..];
        let after =
            health::stale_after_secs(health::median_interval(day)).max(SILENT_ALERT_FLOOR_SECS);
        let quiet = newest.secs_until(now);
        let verdict = if quiet > after {
            let guess = match guess(buffer, newest, &watch.health) {
                Some(SilenceGuess::Battery) => " — battery likely",
                Some(SilenceGuess::Signal) => " — signal likely",
                None => "",
            };
            Verdict::Raise {
                detail: format!("no reading for {}{guess}", span(quiet)),
            }
        } else {
            Verdict::Clear
        };
        out.push(Condition {
            rule: Rule::Silent,
            device: Some(device.clone()),
            verdict,
        });
    }
}

/// 4f's guess, from the last battery and the week's signal the device sent.
fn guess(
    buffer: &DeviceBuffer,
    newest: Timestamp,
    params: &health::HealthParams,
) -> Option<SilenceGuess> {
    let week = newest.saturating_add_secs(-7 * DAY);
    let column = |metric| {
        buffer
            .columns()
            .iter()
            .find(|c| column_metric(c) == Some(metric))
    };
    let battery =
        column(Metric::BatteryV).and_then(|c| buffer.series(c, week).last().map(|&(_, v)| v));
    let rssi: Vec<f64> = column(Metric::RssiDbm)
        .map(|c| buffer.series(c, week).into_iter().map(|(_, v)| v).collect())
        .unwrap_or_default();
    health::guess(battery, &rssi, params)
}

/// "45 min", "18 h", "3 d 4 h": as precise as a phone notification needs.
pub fn span(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    match (mins / 1_440, mins / 60 % 24, mins % 60) {
        (0, 0, m) => format!("{m} min"),
        (0, h, _) => format!("{h} h"),
        (d, 0, _) => format!("{d} d"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::watch::tests::watch;
    use crate::calc::stability::{ExpectedFg, FgSource};
    use crate::units::Sg;
    use brewfixture::curve::Rng;

    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    fn buffers(
        device: &str,
        column: &str,
        points: impl IntoIterator<Item = (i64, f64)>,
    ) -> Buffers {
        let mut b = DeviceBuffer::new(vec![column.into()]);
        for (t, v) in points {
            b.push(at(t), &[Some(v.to_string())]);
        }
        Buffers::from([(device.to_owned(), b)])
    }

    fn verdict(conds: &[Condition], rule: Rule) -> &Verdict {
        &conds.iter().find(|c| c.rule == rule).unwrap().verdict
    }

    fn raised(v: &Verdict) -> bool {
        matches!(v, Verdict::Raise { .. })
    }

    /// 1.040 for a 12 h lag, down to 1.000 over three days, then flat — every 15 min
    /// with ±0.001 of jitter.
    fn ferment(end: i64, seed: u64) -> Vec<(i64, f64)> {
        let mut rng = Rng::new(seed);
        (0..end)
            .step_by(15 * MIN as usize)
            .map(|t| {
                let sg = match t {
                    t if t < 12 * HOUR => 1.040,
                    t if t < 84 * HOUR => {
                        1.040 - 0.040 * (t - 12 * HOUR) as f64 / (72 * HOUR) as f64
                    }
                    _ => 1.000,
                };
                (t, sg + rng.noise(0.001))
            })
            .collect()
    }

    fn sg_watch(expected: Option<f64>) -> Watch {
        let mut w = watch(1);
        w.temp = None;
        w.devices.clear();
        w.expected_fg = expected.map(|fg| ExpectedFg {
            lo: Sg::new_unchecked(fg),
            hi: Sg::new_unchecked(fg),
            source: FgSource::Recipe,
        });
        w
    }

    #[test]
    fn a_ferment_finishes_on_the_plateau_and_not_before() {
        let w = sg_watch(Some(1.000));
        let mid = buffers("ispindel", "gravity", ferment(48 * HOUR, 1));
        let conds = evaluate(&w, &mid, at(48 * HOUR));
        assert_eq!(verdict(&conds, Rule::Finished), &Verdict::Clear);

        let done = buffers("ispindel", "gravity", ferment(6 * 24 * HOUR, 1));
        let conds = evaluate(&w, &done, at(6 * 24 * HOUR));
        assert!(raised(verdict(&conds, Rule::Finished)), "{conds:?}");
        assert_eq!(verdict(&conds, Rule::Stalled), &Verdict::Clear);
    }

    #[test]
    fn a_plateau_above_the_expected_fg_is_stalled_not_finished() {
        let w = sg_watch(Some(0.990));
        let b = buffers("ispindel", "gravity", ferment(6 * 24 * HOUR, 2));
        let conds = evaluate(&w, &b, at(6 * 24 * HOUR));
        let Verdict::Raise { detail } = verdict(&conds, Rule::Stalled) else {
            panic!("{conds:?}");
        };
        assert!(detail.contains("above the expected 0.990"), "{detail}");
        assert_eq!(verdict(&conds, Rule::Finished), &Verdict::Clear);
    }

    #[test]
    fn the_lag_phase_says_nothing() {
        let w = sg_watch(Some(1.000));
        let b = buffers("ispindel", "gravity", ferment(11 * HOUR, 3));
        let conds = evaluate(&w, &b, at(11 * HOUR));
        assert_eq!(verdict(&conds, Rule::Finished), &Verdict::Unknown);
        assert_eq!(verdict(&conds, Rule::SugarBreak), &Verdict::Unknown);
    }

    #[test]
    fn the_sugar_break_and_target_latch_on_the_smoothed_gravity() {
        let mut w = sg_watch(Some(1.000));
        w.target_sg = Some(Sg::new_unchecked(1.010));
        // ⅓ of 1.040 → 1.000 is 1.0267, reached at 12 h + 24 h.
        let before = buffers("ispindel", "gravity", ferment(34 * HOUR, 4));
        let conds = evaluate(&w, &before, at(34 * HOUR));
        assert_eq!(verdict(&conds, Rule::SugarBreak), &Verdict::Unknown);

        let after = buffers("ispindel", "gravity", ferment(40 * HOUR, 4));
        let conds = evaluate(&w, &after, at(40 * HOUR));
        assert!(raised(verdict(&conds, Rule::SugarBreak)), "{conds:?}");
        assert_eq!(verdict(&conds, Rule::TargetSg), &Verdict::Unknown);

        let late = buffers("ispindel", "gravity", ferment(80 * HOUR, 4));
        let conds = evaluate(&w, &late, at(80 * HOUR));
        assert!(raised(verdict(&conds, Rule::TargetSg)), "{conds:?}");
    }

    #[test]
    fn one_tilt_spike_does_not_reach_the_sugar_break() {
        let w = sg_watch(Some(1.000));
        let mut points = ferment(20 * HOUR, 5);
        let last = points.len() - 3;
        points[last].1 = 1.010;
        let b = buffers("ispindel", "gravity", points);
        let conds = evaluate(&w, &b, at(20 * HOUR));
        assert_eq!(verdict(&conds, Rule::SugarBreak), &Verdict::Unknown);
    }

    #[test]
    fn plato_is_converted_before_judging() {
        let mut w = sg_watch(Some(1.000));
        w.gravity.as_mut().unwrap().unit = IspindelUnit::Plato;
        let plato = ferment(40 * HOUR, 6)
            .into_iter()
            .map(|(t, sg)| (t, crate::calc::sg_to_plato(Sg::new_unchecked(sg))));
        let b = buffers("ispindel", "gravity", plato);
        let conds = evaluate(&w, &b, at(40 * HOUR));
        assert!(raised(verdict(&conds, Rule::SugarBreak)), "{conds:?}");
    }

    fn temp_watch() -> Watch {
        let mut w = watch(1);
        w.gravity = None;
        w.devices.clear();
        w
    }

    fn temps(values: impl IntoIterator<Item = f64>) -> Buffers {
        buffers(
            "esp32",
            "vessel_temp",
            values
                .into_iter()
                .enumerate()
                .map(|(i, v)| (i as i64 * 30, v)),
        )
    }

    #[test]
    fn a_temperature_spike_does_not_raise() {
        let mut values = vec![18.0; 40];
        values.extend([25.0; 5]);
        let conds = evaluate(&temp_watch(), &temps(values), at(45 * 30));
        assert_eq!(verdict(&conds, Rule::TempHigh), &Verdict::Unknown);
        assert_eq!(verdict(&conds, Rule::TempLow), &Verdict::Clear);
    }

    #[test]
    fn ten_minutes_out_of_band_raises_and_the_margin_clears() {
        let mut values = vec![18.0; 10];
        values.extend([20.5; 21]);
        let conds = evaluate(&temp_watch(), &temps(values.clone()), at(31 * 30));
        let Verdict::Raise { detail } = verdict(&conds, Rule::TempHigh) else {
            panic!("{conds:?}");
        };
        assert_eq!(detail, "20.5 °C, above 20.0 °C for 10 min");

        values.push(19.9);
        let conds = evaluate(&temp_watch(), &temps(values.clone()), at(32 * 30));
        assert_eq!(
            verdict(&conds, Rule::TempHigh),
            &Verdict::Unknown,
            "inside the margin"
        );
        values.push(19.6);
        let conds = evaluate(&temp_watch(), &temps(values), at(33 * 30));
        assert_eq!(verdict(&conds, Rule::TempHigh), &Verdict::Clear);
    }

    #[test]
    fn a_new_steps_band_waits_for_the_beer_to_get_there() {
        // A diacetyl rest from 10 °C: 16–18, ticked at 0, free-rising half a degree an hour.
        let mut w = temp_watch();
        (w.temp_min_c, w.temp_max_c, w.band_since) = (Some(16.0), Some(18.0), Some(at(0)));
        let rising = |hours: i64| {
            buffers(
                "esp32",
                "vessel_temp",
                (0..=hours * 4).map(|i| (i * 15 * MIN, 10.0 + 0.5 * (i as f64 / 4.0))),
            )
        };
        let conds = evaluate(&w, &rising(11), at(11 * HOUR));
        assert_eq!(
            verdict(&conds, Rule::TempLow),
            &Verdict::Clear,
            "on its way"
        );

        // In at 12 h, then it falls back out and stays out.
        let mut points: Vec<_> = (0..=48)
            .map(|i| (i * 15 * MIN, 10.0 + 0.5 * (i as f64 / 4.0)))
            .collect();
        points.extend((1..=4).map(|i| (12 * HOUR + i * 15 * MIN, 15.0)));
        let conds = evaluate(&w, &buffers("esp32", "vessel_temp", points), at(13 * HOUR));
        assert!(raised(verdict(&conds, Rule::TempLow)), "{conds:?}");
    }

    #[test]
    fn a_band_never_reached_is_judged_after_a_day() {
        let mut w = temp_watch();
        (w.temp_min_c, w.temp_max_c, w.band_since) = (Some(0.0), Some(2.0), Some(at(0)));
        let stuck = |end: i64| {
            buffers(
                "esp32",
                "vessel_temp",
                (0..=end / (15 * MIN)).map(|i| (i * 15 * MIN, 8.0)),
            )
        };
        let conds = evaluate(&w, &stuck(23 * HOUR), at(23 * HOUR));
        assert_eq!(verdict(&conds, Rule::TempHigh), &Verdict::Clear);
        let conds = evaluate(&w, &stuck(25 * HOUR), at(25 * HOUR));
        assert!(raised(verdict(&conds, Rule::TempHigh)), "{conds:?}");
    }

    #[test]
    fn no_band_is_no_band_rule_but_still_a_rate() {
        let mut w = temp_watch();
        w.temp_min_c = None;
        w.temp_max_c = None;
        let conds = evaluate(&w, &temps([30.0; 50]), at(50 * 30));
        let rules: Vec<Rule> = conds.iter().map(|c| c.rule).collect();
        assert_eq!(rules, [Rule::TempRate]);
        assert_eq!(conds[0].verdict, Verdict::Clear);
    }

    /// `hold` °C for two hours, then a straight line to `to` over an hour, then held for
    /// half an hour — a reading every 30 s.
    fn drift(hold: f64, to: f64) -> Vec<f64> {
        let mut values = vec![hold; 240];
        values.extend((1..=120).map(|i| hold + (to - hold) * i as f64 / 120.0));
        values.extend([to; 60]);
        values
    }

    fn wide_watch() -> Watch {
        let mut w = temp_watch();
        (w.temp_min_c, w.temp_max_c) = (Some(10.0), Some(25.0));
        w
    }

    #[test]
    fn a_fast_change_inside_the_band_raises_the_rate() {
        let values = drift(18.0, 15.5);
        let conds = evaluate(
            &wide_watch(),
            &temps(values.clone()),
            at(values.len() as i64 * 30),
        );
        let Verdict::Raise { detail } = verdict(&conds, Rule::TempRate) else {
            panic!("{conds:?}");
        };
        assert!(
            detail.starts_with("fell 2.5 °C in 1 h (18.0 → 15.5 °C)"),
            "{detail}"
        );
        assert_eq!(verdict(&conds, Rule::TempLow), &Verdict::Clear);
    }

    #[test]
    fn the_rate_has_a_margin_and_forgets_after_its_window() {
        let w = wide_watch();
        let near = drift(18.0, 16.2);
        let conds = evaluate(&w, &temps(near.clone()), at(near.len() as i64 * 30));
        assert_eq!(
            verdict(&conds, Rule::TempRate),
            &Verdict::Unknown,
            "1.8 is in the margin"
        );
        let small = drift(18.0, 16.5);
        let conds = evaluate(&w, &temps(small.clone()), at(small.len() as i64 * 30));
        assert_eq!(verdict(&conds, Rule::TempRate), &Verdict::Clear);

        let mut settled = drift(18.0, 15.5);
        settled.extend([15.5; 360]);
        let conds = evaluate(&w, &temps(settled.clone()), at(settled.len() as i64 * 30));
        assert_eq!(verdict(&conds, Rule::TempRate), &Verdict::Clear);
    }

    #[test]
    fn a_short_spike_does_not_count_toward_the_rate() {
        let mut values = vec![18.0; 240];
        values.extend([24.0; 8]);
        values.extend([18.0; 20]);
        let conds = evaluate(
            &wide_watch(),
            &temps(values.clone()),
            at(values.len() as i64 * 30),
        );
        assert_eq!(verdict(&conds, Rule::TempRate), &Verdict::Clear);
    }

    #[test]
    fn a_ramp_into_the_band_is_not_a_rate() {
        // 12 → 17 into 16–20: only the degree inside the band counts.
        let values = drift(12.0, 17.0);
        let conds = evaluate(
            &temp_watch(),
            &temps(values.clone()),
            at(values.len() as i64 * 30),
        );
        assert_eq!(verdict(&conds, Rule::TempRate), &Verdict::Clear);
        assert_eq!(verdict(&conds, Rule::TempLow), &Verdict::Clear);
    }

    #[test]
    fn out_of_the_band_is_left_to_the_band_rules() {
        let values = drift(18.0, 23.0);
        let conds = evaluate(
            &temp_watch(),
            &temps(values.clone()),
            at(values.len() as i64 * 30),
        );
        assert_eq!(verdict(&conds, Rule::TempRate), &Verdict::Unknown);
        assert!(raised(verdict(&conds, Rule::TempHigh)), "{conds:?}");
    }

    fn device(columns: &[&str], rows: impl IntoIterator<Item = (i64, Vec<f64>)>) -> DeviceBuffer {
        let mut b = DeviceBuffer::new(columns.iter().map(|c| c.to_string()).collect());
        for (t, values) in rows {
            let cells: Vec<_> = values.iter().map(|v| Some(v.to_string())).collect();
            b.push(at(t), &cells);
        }
        b
    }

    fn silent_watch() -> Watch {
        let mut w = watch(1);
        w.gravity = None;
        w.temp = None;
        w.devices = vec!["ispindel".into()];
        w
    }

    #[test]
    fn a_device_is_silent_past_three_intervals_and_clears_on_a_row() {
        let rows = (0..20).map(|i| (i * 30 * MIN, vec![1.040, 4.1, -60.0]));
        let mut b = device(&["gravity", "battery", "rssi"], rows);
        let newest = 19 * 30 * MIN;
        let mut buffers = Buffers::from([("ispindel".to_owned(), b.clone())]);

        let conds = evaluate(&silent_watch(), &buffers, at(newest + 90 * MIN));
        assert_eq!(conds[0].verdict, Verdict::Clear);
        assert_eq!(conds[0].device.as_deref(), Some("ispindel"));

        let conds = evaluate(&silent_watch(), &buffers, at(newest + 91 * MIN));
        assert_eq!(
            conds[0].verdict,
            Verdict::Raise {
                detail: "no reading for 1 h".into()
            }
        );

        b.push(at(newest + 95 * MIN), &[Some("1.040".into()), None, None]);
        buffers.insert("ispindel".into(), b);
        let conds = evaluate(&silent_watch(), &buffers, at(newest + 96 * MIN));
        assert_eq!(conds[0].verdict, Verdict::Clear);
    }

    #[test]
    fn a_fast_device_dropping_a_few_sends_is_not_silent_for_an_hour() {
        let rows = (0..100).map(|i| (i * 5 * MIN, vec![1.040, 4.1, -90.0]));
        let buffers = Buffers::from([(
            "ispindel".to_owned(),
            device(&["gravity", "battery", "rssi"], rows),
        )]);
        let newest = 99 * 5 * MIN;

        let conds = evaluate(&silent_watch(), &buffers, at(newest + 60 * MIN));
        assert_eq!(conds[0].verdict, Verdict::Clear);

        let conds = evaluate(&silent_watch(), &buffers, at(newest + 61 * MIN));
        assert_eq!(
            conds[0].verdict,
            Verdict::Raise {
                detail: "no reading for 1 h — signal likely".into()
            }
        );
    }

    #[test]
    fn a_silent_device_with_a_low_battery_says_so() {
        let rows = (0..20).map(|i| (i * 15 * MIN, vec![1.040, 3.65, -60.0]));
        let buffers = Buffers::from([(
            "ispindel".to_owned(),
            device(&["gravity", "battery", "rssi"], rows),
        )]);
        let conds = evaluate(&silent_watch(), &buffers, at(30 * HOUR));
        let Verdict::Raise { detail } = &conds[0].verdict else {
            panic!("{conds:?}");
        };
        assert!(detail.ends_with("battery likely"), "{detail}");
    }

    #[test]
    fn spans_read_like_a_notification() {
        assert_eq!(span(59), "0 min");
        assert_eq!(span(45 * MIN), "45 min");
        assert_eq!(span(18 * HOUR + 59 * MIN), "18 h");
        assert_eq!(span(48 * HOUR), "2 d");
        assert_eq!(span(76 * HOUR), "3 d 4 h");
    }
}
