//! Phase 5b. The batch comparison overlay: several batches' gravity on one axis,
//! aligned on pitch.
//!
//! **Aligned by range, not by rewriting time.** One span in seconds since pitch is
//! queried for every batch at `[pitch + start, pitch + end)`, so each gets the same x
//! extent and, at the same `max_points`, the same resolution. The series keep 4a's
//! absolute timestamps; hours since pitch is 4d's `toX`, applied at the chart.
//!
//! **Fraction of apparent attenuation**, `(OG − SG)/(OG − 1)`, is what puts a 1.030 and
//! a 1.050 wash on one axis. Computed here in SG, before any display conversion, from
//! the recorded `batch.og` — never from a first reading, for 4e's reason.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::Connection;

use super::{
    Metric,
    analytics::ferm_temp_series,
    batch::{BatchRow, get_batch},
    series::{Series, SeriesRange, batch_series, binding_span},
};
use crate::{
    Error, Result,
    calc::{attenuation::apparent_progress, batch_metrics::Missing},
    units::{Sg, Timestamp},
};

pub const MAX_BATCHES: usize = 8;

/// A half-open span in seconds since each batch's pitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelRange {
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YMode {
    Raw,
    Fraction,
}

/// What a batch's `gravity` series hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum YAxis {
    /// Fraction of apparent attenuation. Not clamped: a seltzer finishes past 1.0.
    Fraction,
    /// SG. `reason` is why a fraction was asked for and not given; `None` when raw was
    /// what was asked for.
    Raw { reason: Option<Missing> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ComparedBatch {
    pub batch_id: i64,
    pub code: String,
    pub pitched_at: Timestamp,
    pub og: Option<Sg>,
    pub y: YAxis,
    /// Every `sg` stream, sensor and manual, as 4a returns them.
    pub gravity: Vec<Series>,
    /// 5a's ferment temperature stream, when asked for and there is one.
    pub temp: Option<Series>,
}

/// Asked for and not drawn, with why — never silently missing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Excluded {
    pub batch_id: i64,
    pub code: String,
    pub reason: Missing,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Comparison {
    /// The span queried. `None` when no pitched batch has a binding, so there is
    /// nothing to span.
    pub span: Option<RelRange>,
    /// In the order asked for, so a batch keeps its colour across refetches.
    pub batches: Vec<ComparedBatch>,
    pub excluded: Vec<Excluded>,
}

/// 2 to [`MAX_BATCHES`] batches over `span`, at most `max_points` per sensor series.
///
/// `span` defaults to pitch through the latest binding end among them — an open
/// binding ending at now. Not pitch to now: an archived batch's `[pitch, now)` is a
/// year, and bucketing that would cost the ferment its detail.
pub fn compare_batches(
    conn: &Connection,
    batch_ids: &[i64],
    y: YMode,
    with_temp: bool,
    span: Option<RelRange>,
    max_points: usize,
) -> Result<Comparison> {
    compare_at(
        conn,
        batch_ids,
        y,
        with_temp,
        span,
        max_points,
        Timestamp::now(),
    )
}

fn compare_at(
    conn: &Connection,
    batch_ids: &[i64],
    y: YMode,
    with_temp: bool,
    span: Option<RelRange>,
    max_points: usize,
    now: Timestamp,
) -> Result<Comparison> {
    let mut ids = Vec::with_capacity(batch_ids.len());
    for &id in batch_ids {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if !(2..=MAX_BATCHES).contains(&ids.len()) {
        return Err(Error::CompareCount { n: ids.len() });
    }
    if max_points == 0 {
        return Err(Error::NotPositive {
            field: "max_points",
            value: 0.0,
        });
    }

    let mut pitched = Vec::new();
    let mut excluded = Vec::new();
    for id in ids {
        let batch = get_batch(conn, id)?;
        match batch.pitched_at {
            Some(pitch) => pitched.push((batch, pitch)),
            None => excluded.push(Excluded {
                batch_id: batch.id,
                code: batch.code,
                reason: Missing::NoPitch,
            }),
        }
    }

    let span = match span {
        Some(span) => Some(span),
        None => default_span(conn, &pitched, now)?,
    };
    let batches = pitched
        .into_iter()
        .map(|(batch, pitch)| compared(conn, batch, pitch, y, with_temp, span, max_points))
        .collect::<Result<_>>()?;
    Ok(Comparison {
        span,
        batches,
        excluded,
    })
}

fn default_span(
    conn: &Connection,
    pitched: &[(BatchRow, Timestamp)],
    now: Timestamp,
) -> Result<Option<RelRange>> {
    let mut end = None;
    for (batch, pitch) in pitched {
        if let Some(bound) = binding_span(conn, batch.id, now)? {
            end = end.max(Some(pitch.secs_until(bound.end)));
        }
    }
    Ok(end
        .filter(|&end| end > 0)
        .map(|end| RelRange { start: 0, end }))
}

fn compared(
    conn: &Connection,
    batch: BatchRow,
    pitch: Timestamp,
    y: YMode,
    with_temp: bool,
    span: Option<RelRange>,
    max_points: usize,
) -> Result<ComparedBatch> {
    let (mut gravity, temp) = match span {
        Some(span) => {
            let range = Some(SeriesRange {
                start: pitch.saturating_add_secs(span.start),
                end: pitch.saturating_add_secs(span.end),
            });
            let gravity = batch_series(conn, batch.id, &[Metric::Sg], range, max_points)?.series;
            let temp = if with_temp {
                let temps = [Metric::TempC, Metric::GravityTempC];
                let temps = batch_series(conn, batch.id, &temps, range, max_points)?.series;
                ferm_temp_series(&temps).cloned()
            } else {
                None
            };
            (gravity, temp)
        }
        None => (Vec::new(), None),
    };

    let y = match (y, batch.og) {
        (YMode::Raw, _) => YAxis::Raw { reason: None },
        (YMode::Fraction, None) => YAxis::Raw {
            reason: Some(Missing::NoOg),
        },
        (YMode::Fraction, Some(og)) => match apparent_progress(og) {
            Ok(f) => {
                gravity.iter_mut().for_each(|s| to_fraction(s, &f));
                YAxis::Fraction
            }
            Err(Error::CalcOutOfRange { .. }) => YAxis::Raw {
                reason: Some(Missing::Implausible),
            },
            Err(e) => return Err(e),
        },
    };

    Ok(ComparedBatch {
        batch_id: batch.id,
        code: batch.code,
        pitched_at: pitch,
        og: batch.og,
        y,
        gravity,
        temp,
    })
}

/// Decreasing in SG, so a bucket's lowest gravity is its highest fraction: `min` and
/// `max` trade places.
fn to_fraction(s: &mut Series, f: &impl Fn(f64) -> f64) {
    for v in [&mut s.avg, &mut s.min, &mut s.max, &mut s.smoothed]
        .into_iter()
        .flatten()
        .flatten()
    {
        *v = f(*v);
    }
    std::mem::swap(&mut s.min, &mut s.max);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        SourceKind,
        batch::{NewBatch, archive_batch, create_batch, set_pitched_at},
        recipe::BeverageType,
        tier1::{create_source, create_stream, insert_readings},
        timeline::bind_stream,
    };
    use brewfixture::curve::gravity_at;

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;

    fn at(offset_secs: i64) -> Timestamp {
        T0.saturating_add_secs(offset_secs)
    }

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection, code: &str, og: Option<f64>, pitch: Option<Timestamp>) -> i64 {
        let id = create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: og.map(|v| Sg::new(v).unwrap()),
                notes: None,
            },
            T0,
        )
        .unwrap();
        if let Some(pitch) = pitch {
            set_pitched_at(conn, id, pitch).unwrap();
        }
        id
    }

    /// A stream bound from `from`, closed at `until` if given, with `f(hours since
    /// from)` every `interval` over `[from, from + len)`.
    #[allow(clippy::too_many_arguments)]
    fn stream(
        conn: &mut Connection,
        batch_id: i64,
        kind: SourceKind,
        metric: Metric,
        from: Timestamp,
        until: Option<Timestamp>,
        (len, interval): (i64, i64),
        f: impl Fn(f64) -> f64,
    ) -> i64 {
        let name = format!("{batch_id}-{}", metric.as_str());
        let source_id = create_source(conn, kind, &name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, &name, metric).unwrap();
        let rows: Vec<_> = (0..len)
            .step_by(interval as usize)
            .map(|t| {
                let ts = from.saturating_add_secs(t);
                (stream_id, ts, f(t as f64 / HOUR as f64))
            })
            .collect();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
        bind_stream(conn, batch_id, stream_id, from, until).unwrap();
        stream_id
    }

    fn ispindel(conn: &mut Connection, batch_id: i64, pitch: Timestamp, len: i64, interval: i64) {
        stream(
            conn,
            batch_id,
            SourceKind::BrewdJournal,
            Metric::Sg,
            pitch,
            None,
            (len, interval),
            gravity_at,
        );
    }

    fn compare(conn: &Connection, ids: &[i64], y: YMode, span: Option<RelRange>) -> Comparison {
        compare_at(conn, ids, y, false, span, 1000, at(200 * DAY)).unwrap()
    }

    #[test]
    fn two_pitches_are_queried_at_the_same_offsets() {
        let mut conn = db();
        let (pa, pb) = (T0, at(30 * DAY));
        let a = batch(&conn, "A", Some(1.045), Some(pa));
        let b = batch(&conn, "B", Some(1.045), Some(pb));
        ispindel(&mut conn, a, pa, 10 * DAY, 15 * 60);
        ispindel(&mut conn, b, pb, 10 * DAY, 15 * 60);

        let span = RelRange {
            start: HOUR,
            end: 5 * DAY,
        };
        let got = compare(&conn, &[a, b], YMode::Raw, Some(span));
        assert_eq!(got.span, Some(span));
        for (c, pitch) in got.batches.iter().zip([pa, pb]) {
            let ts = &c.gravity[0].ts;
            assert_eq!(pitch.secs_until(ts[0]), HOUR);
            assert!(pitch.secs_until(*ts.last().unwrap()) < 5 * DAY);
            assert_eq!(c.pitched_at, pitch);
        }
        assert_eq!(
            got.batches[0].gravity[0].ts.len(),
            got.batches[1].gravity[0].ts.len()
        );
    }

    /// An archived batch's `[pitch, now)` would be half a year here.
    #[test]
    fn the_default_span_is_the_data_not_now() {
        let mut conn = db();
        let (pa, pb) = (T0, at(100 * DAY));
        let a = batch(&conn, "A", None, Some(pa));
        let b = batch(&conn, "B", None, Some(pb));
        stream(
            &mut conn,
            a,
            SourceKind::BrewdJournal,
            Metric::Sg,
            pa,
            Some(at(14 * DAY)),
            (14 * DAY, HOUR),
            gravity_at,
        );
        archive_batch(&conn, a, at(20 * DAY)).unwrap();
        ispindel(&mut conn, b, pb, 3 * DAY, HOUR);

        let now = pb.saturating_add_secs(3 * DAY);
        let got = compare_at(&conn, &[a, b], YMode::Raw, false, None, 1000, now).unwrap();
        assert_eq!(
            got.span,
            Some(RelRange {
                start: 0,
                end: 14 * DAY
            })
        );
    }

    #[test]
    fn nothing_bound_has_no_span_and_no_series() {
        let conn = db();
        let a = batch(&conn, "A", None, Some(T0));
        let b = batch(&conn, "B", None, Some(T0));
        let got = compare(&conn, &[a, b], YMode::Raw, None);
        assert_eq!(got.span, None);
        assert!(got.batches.iter().all(|c| c.gravity.is_empty()));
    }

    #[test]
    fn an_unpitched_batch_is_excluded_with_its_reason() {
        let mut conn = db();
        let a = batch(&conn, "A", None, Some(T0));
        let b = batch(&conn, "B", None, None);
        let c = batch(&conn, "C", None, Some(T0));
        ispindel(&mut conn, a, T0, DAY, HOUR);

        let got = compare(&conn, &[c, b, a], YMode::Raw, None);
        let ids: Vec<_> = got.batches.iter().map(|c| c.batch_id).collect();
        assert_eq!(ids, [c, a]);
        assert_eq!(
            got.excluded,
            [Excluded {
                batch_id: b,
                code: "B".into(),
                reason: Missing::NoPitch
            }]
        );
    }

    #[test]
    fn a_fraction_maps_every_column_and_swaps_min_and_max() {
        let mut conn = db();
        let a = batch(&conn, "A", Some(1.045), Some(T0));
        let b = batch(&conn, "B", Some(1.045), Some(T0));
        // Bucketed: 10 days at 60 s into 50 points.
        ispindel(&mut conn, a, T0, 10 * DAY, 60);
        let run = |y| compare_at(&conn, &[a, b], y, false, None, 50, at(10 * DAY)).unwrap();
        let (raw, frac) = (run(YMode::Raw), run(YMode::Fraction));
        let (raw, frac) = (&raw.batches[0], &frac.batches[0]);
        assert_eq!(frac.y, YAxis::Fraction);

        let f = apparent_progress(Sg::new(1.045).unwrap()).unwrap();
        let (r, g) = (&raw.gravity[0], &frac.gravity[0]);
        assert!(r.bucket_secs.is_some());
        let close = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (Some(x), Some(y)) => (f(x) - y).abs() < 1e-12,
            (None, None) => true,
            _ => false,
        };
        for i in 0..r.ts.len() {
            assert!(close(r.avg[i], g.avg[i]));
            assert!(close(r.smoothed[i], g.smoothed[i]));
            assert!(close(r.min[i], g.max[i]));
            assert!(close(r.max[i], g.min[i]));
            if let (Some(lo), Some(hi)) = (g.min[i], g.max[i]) {
                assert!(lo <= hi);
            }
        }
        let last = g.smoothed.iter().rev().find_map(|v| *v).unwrap();
        assert!(last > 0.5, "{last}");
    }

    #[test]
    fn no_og_stays_raw_and_says_why() {
        let mut conn = db();
        let a = batch(&conn, "A", None, Some(T0));
        let b = batch(&conn, "B", Some(1.001), Some(T0));
        ispindel(&mut conn, a, T0, DAY, HOUR);

        let got = compare(&conn, &[a, b], YMode::Fraction, None);
        assert_eq!(
            got.batches[0].y,
            YAxis::Raw {
                reason: Some(Missing::NoOg)
            }
        );
        assert_eq!(
            got.batches[1].y,
            YAxis::Raw {
                reason: Some(Missing::Implausible)
            }
        );
        assert!(
            got.batches[0].gravity[0]
                .avg
                .iter()
                .flatten()
                .all(|&v| v > 0.9)
        );

        let raw = compare(&conn, &[a, b], YMode::Raw, None);
        assert_eq!(raw.batches[0].y, YAxis::Raw { reason: None });
    }

    #[test]
    fn temp_is_the_ferment_temperature_stream() {
        let mut conn = db();
        let a = batch(&conn, "A", None, Some(T0));
        let b = batch(&conn, "B", None, Some(T0));
        let span = (5 * DAY, HOUR);
        let probe = stream(
            &mut conn,
            a,
            SourceKind::BrewdJournal,
            Metric::TempC,
            T0,
            None,
            span,
            |_| 18.0,
        );
        stream(
            &mut conn,
            a,
            SourceKind::BrewdJournal,
            Metric::GravityTempC,
            T0,
            None,
            span,
            |_| 20.0,
        );
        let ispindel_temp = stream(
            &mut conn,
            b,
            SourceKind::BrewdJournal,
            Metric::GravityTempC,
            T0,
            None,
            span,
            |_| 20.0,
        );
        stream(
            &mut conn,
            b,
            SourceKind::Manual,
            Metric::TempC,
            T0,
            None,
            span,
            |_| 17.0,
        );

        let run = |with_temp| {
            compare_at(
                &conn,
                &[a, b],
                YMode::Raw,
                with_temp,
                None,
                1000,
                at(5 * DAY),
            )
            .unwrap()
        };
        let got = run(true);
        let temps: Vec<_> = got
            .batches
            .iter()
            .map(|c| c.temp.as_ref().map(|s| s.stream_id))
            .collect();
        assert_eq!(temps, [Some(probe), Some(ispindel_temp)]);
        assert!(run(false).batches.iter().all(|c| c.temp.is_none()));
    }

    #[test]
    fn the_count_is_checked_after_duplicates() {
        let conn = db();
        let ids: Vec<_> = (0..9)
            .map(|i| batch(&conn, &format!("B{i}"), None, Some(T0)))
            .collect();
        let count = |ids: &[i64]| {
            compare_at(&conn, ids, YMode::Raw, false, None, 1000, T0)
                .err()
                .map(|e| e.code())
        };
        assert_eq!(count(&[ids[0], ids[0]]), Some("compare.batch_count"));
        assert_eq!(count(&ids), Some("compare.batch_count"));
        assert_eq!(count(&ids[..8]), None);

        let got = compare(&conn, &[ids[1], ids[0], ids[1]], YMode::Raw, None);
        let order: Vec<_> = got.batches.iter().map(|c| c.batch_id).collect();
        assert_eq!(order, [ids[1], ids[0]]);
    }
}
