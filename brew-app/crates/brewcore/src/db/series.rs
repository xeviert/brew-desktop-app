//! Phase 4a. The series query: a batch's readings, scoped and downsampled for a chart.
//!
//! **Scoped by `batch_stream`, not by stream.** A reading outside every one of the
//! batch's windows is not the batch's, even from a stream it is bound to — that is what
//! lets batch 17 and 18 share a fermenter. Windows are half-open, `[starts_at, ends_at)`,
//! as everywhere else in [`timeline`](crate::db::timeline).
//!
//! **One series per stream, not per metric.** An iSpindle `sg` and a hydrometer `sg` on
//! one batch are two series on one axis; merging them hides the drift between the two
//! instruments that the chart exists to show.
//!
//! **Min/max per bucket, not LTTB.** LTTB picks points that look right and can drop the
//! one spike that says a probe fell out of the thermowell. Downsampling is a `GROUP BY`
//! riding `reading`'s clustered `(stream_id, ts)` key: the IPC payload is what costs,
//! not the chart.
//!
//! **Gaps survive.** A gap is "no reading for > 3× the stream's median interval", the
//! rule 4f's staleness uses. Zoomed in past a device's interval most buckets are empty,
//! so an empty bucket alone is not a gap: a run of them becomes one `None` only when the
//! raw readings either side of it are further apart than the rule allows.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own.

use rusqlite::{Connection, params};

use super::{Metric, batch::get_batch, tier1::SourceKind};
use crate::{Error, Result, units::Timestamp};

/// How many trailing readings [`median_interval`] looks at. Enough to shrug off a
/// dropout or two, few enough to stay one short primary-key scan.
const MEDIAN_SAMPLE: i64 = 257;

/// A half-open `[start, end)` span of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeriesRange {
    pub start: Timestamp,
    pub end: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchSeries {
    /// The range actually queried. `None` for a batch with no pitch and no bindings,
    /// which has neither a t=0 nor a span to default to.
    pub range: Option<SeriesRange>,
    pub series: Vec<Series>,
}

/// One stream's readings over the range, columnar because that is uPlot's own shape
/// and the smallest JSON.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Series {
    pub stream_id: i64,
    pub metric: Metric,
    pub source_kind: SourceKind,
    pub label: String,
    /// `None` when these are raw rows, one per reading.
    pub bucket_secs: Option<i64>,
    /// Raw: the reading's time. Bucketed: the first reading's time in the bucket, or
    /// the bucket's start for a gap.
    pub ts: Vec<Timestamp>,
    /// Raw: the reading itself. `None` is a gap, never a missing value to interpolate.
    pub avg: Vec<Option<f64>>,
    /// Empty when raw.
    pub min: Vec<Option<f64>>,
    /// Empty when raw.
    pub max: Vec<Option<f64>>,
}

/// Every stream bound to this batch whose metric is in `metrics`, clipped to the
/// batch's windows and to `range`, at most `max_points` points per sensor series.
///
/// `range` defaults to `[pitched_at, now)`. An unpitched batch has no t=0, so it
/// defaults to its bindings' span. A range with `start >= end` — a zoom can produce
/// one — is every series, empty.
///
/// Manual streams ignore `max_points`: a dozen points over a month is the whole record,
/// and averaging two pH readings invents a third.
pub fn batch_series(
    conn: &Connection,
    batch_id: i64,
    metrics: &[Metric],
    range: Option<SeriesRange>,
    max_points: usize,
) -> Result<BatchSeries> {
    series_at(conn, batch_id, metrics, range, max_points, Timestamp::now())
}

fn series_at(
    conn: &Connection,
    batch_id: i64,
    metrics: &[Metric],
    range: Option<SeriesRange>,
    max_points: usize,
    now: Timestamp,
) -> Result<BatchSeries> {
    if max_points == 0 {
        return Err(Error::NotPositive {
            field: "max_points",
            value: 0.0,
        });
    }
    let batch = get_batch(conn, batch_id)?;
    let bound = bound_streams(conn, batch_id, now)?;

    let range = range.or_else(|| match batch.pitched_at {
        Some(pitched_at) => Some(SeriesRange {
            start: pitched_at,
            end: now,
        }),
        None => {
            let windows = bound.iter().flat_map(|s| &s.windows);
            let start = windows.clone().map(|w| w.start).min()?;
            let end = windows.map(|w| w.end).max()?;
            Some(SeriesRange { start, end })
        }
    });
    let Some(range) = range else {
        return Ok(BatchSeries {
            range: None,
            series: Vec::new(),
        });
    };

    let mut series = bound
        .into_iter()
        .filter(|s| metrics.contains(&s.metric))
        .map(|s| stream_series(conn, s, range, max_points))
        .collect::<Result<Vec<_>>>()?;
    series.sort_by_key(|s| (s.metric, s.stream_id));
    Ok(BatchSeries {
        range: Some(range),
        series,
    })
}

/// The typical time between readings, in seconds, over the newest readings of a stream
/// inside `range`. `None` with fewer than two.
///
/// Derived, not configured: a 30 s ESP32 and a 15 min iSpindle cannot share a
/// threshold, and the interval is a device setting that changes. 4f's staleness uses
/// the same number.
pub fn median_interval(
    conn: &Connection,
    stream_id: i64,
    range: SeriesRange,
) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT ts FROM reading WHERE stream_id = ? AND ts >= ? AND ts < ? \
         ORDER BY ts DESC LIMIT ?",
    )?;
    let ts: Vec<i64> = stmt
        .query_map(
            params![stream_id, range.start, range.end, MEDIAN_SAMPLE],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;

    let mut diffs: Vec<i64> = ts.windows(2).map(|w| w[0] - w[1]).collect();
    if diffs.is_empty() {
        return Ok(None);
    }
    diffs.sort_unstable();
    Ok(Some(diffs[diffs.len() / 2]))
}

// ------------------------------------------------------------- bindings

struct BoundStream {
    stream_id: i64,
    metric: Metric,
    source_kind: SourceKind,
    label: String,
    /// Merged, sorted, non-overlapping; an open binding ends at `now`.
    windows: Vec<SeriesRange>,
}

fn bound_streams(conn: &Connection, batch_id: i64, now: Timestamp) -> Result<Vec<BoundStream>> {
    let mut stmt = conn.prepare(
        "SELECT bs.stream_id, s.metric, src.kind, COALESCE(s.label, src.name), \
                bs.starts_at, bs.ends_at \
           FROM batch_stream bs \
           JOIN stream s ON s.id = bs.stream_id \
           JOIN source src ON src.id = s.source_id \
          WHERE bs.batch_id = ? \
          ORDER BY bs.stream_id, bs.starts_at",
    )?;
    let rows = stmt.query_map([batch_id], |row| {
        let ends_at: Option<Timestamp> = row.get(5)?;
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Metric>(1)?,
            row.get::<_, SourceKind>(2)?,
            row.get::<_, String>(3)?,
            SeriesRange {
                start: row.get(4)?,
                end: ends_at.unwrap_or(now),
            },
        ))
    })?;

    let mut out: Vec<BoundStream> = Vec::new();
    for row in rows {
        let (stream_id, metric, source_kind, label, window) = row?;
        // An open binding that starts in the future is empty until then.
        if window.start >= window.end {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.stream_id == stream_id => {
                // Same-batch windows may overlap (3d's unplugged-and-reconnected case);
                // sorted by start, so only the last merged window can touch this one.
                let prev = last.windows.last_mut().expect("never pushed empty");
                if window.start <= prev.end {
                    prev.end = prev.end.max(window.end);
                } else {
                    last.windows.push(window);
                }
            }
            _ => out.push(BoundStream {
                stream_id,
                metric,
                source_kind,
                label,
                windows: vec![window],
            }),
        }
    }
    Ok(out)
}

// --------------------------------------------------------------- series

fn stream_series(
    conn: &Connection,
    stream: BoundStream,
    range: SeriesRange,
    max_points: usize,
) -> Result<Series> {
    let clipped: Vec<SeriesRange> = stream
        .windows
        .iter()
        .map(|w| SeriesRange {
            start: w.start.max(range.start),
            end: w.end.min(range.end),
        })
        .filter(|w| w.start < w.end)
        .collect();

    let mut series = Series {
        stream_id: stream.stream_id,
        metric: stream.metric,
        source_kind: stream.source_kind,
        label: stream.label,
        bucket_secs: None,
        ts: Vec::new(),
        avg: Vec::new(),
        min: Vec::new(),
        max: Vec::new(),
    };
    if clipped.is_empty() {
        return Ok(series);
    }

    if stream.source_kind == SourceKind::Manual {
        for (ts, value) in raw_rows(conn, stream.stream_id, &clipped, None)? {
            series.ts.push(ts);
            series.avg.push(Some(value));
        }
        return Ok(series);
    }

    // Over the batch's whole span, not the zoomed range, so zooming does not move
    // where the gaps are.
    let span = SeriesRange {
        start: stream.windows[0].start,
        end: stream.windows[stream.windows.len() - 1].end,
    };
    let gap_secs = median_interval(conn, stream.stream_id, span)?.map(|m| m * 3);

    let rows = raw_rows(conn, stream.stream_id, &clipped, Some(max_points + 1))?;
    if rows.len() <= max_points {
        let gaps = match gap_secs {
            Some(gap) => rows
                .windows(2)
                .filter(|w| w[0].0.secs_until(w[1].0) > gap)
                .count(),
            None => 0,
        };
        if rows.len() + gaps <= max_points {
            let gap = gap_secs.unwrap_or(i64::MAX);
            let mut prev: Option<Timestamp> = None;
            for &(ts, value) in &rows {
                match prev {
                    Some(prev) if prev.secs_until(ts) > gap => {
                        // Strictly between the two: the gap exceeds 3 s, since
                        // readings are at least a second apart.
                        series.ts.push(Timestamp::from_secs(
                            prev.as_secs() + prev.secs_until(ts) / 2,
                        ));
                        series.avg.push(None);
                    }
                    _ => {}
                }
                series.ts.push(ts);
                series.avg.push(Some(value));
                prev = Some(ts);
            }
            return Ok(series);
        }
    }

    let bucket = bucket_secs(range, max_points);
    let buckets = bucketed_rows(conn, stream.stream_id, &clipped, range.start, bucket)?;
    series.bucket_secs = Some(bucket);
    let gap = gap_secs.unwrap_or(i64::MAX);
    let mut prev: Option<&Bucket> = None;
    for b in &buckets {
        match prev {
            Some(prev) if b.index > prev.index + 1 && prev.last_ts.secs_until(b.first_ts) > gap => {
                // One empty bucket's slot, so the count stays within `max_points`.
                series.ts.push(Timestamp::from_secs(
                    range.start.as_secs() + (prev.index + 1) * bucket,
                ));
                series.avg.push(None);
                series.min.push(None);
                series.max.push(None);
            }
            _ => {}
        }
        prev = Some(b);
        series.ts.push(b.first_ts);
        series.avg.push(Some(b.sum / b.count as f64));
        series.min.push(Some(b.min));
        series.max.push(Some(b.max));
    }
    Ok(series)
}

/// Seconds per bucket such that every bucket index over `range` is below
/// `max_points`. At least one.
fn bucket_secs(range: SeriesRange, max_points: usize) -> i64 {
    let span = range.start.secs_until(range.end);
    let max_points = i64::try_from(max_points).unwrap_or(i64::MAX);
    (span + max_points - 1) / max_points
}

/// Readings in the windows, oldest first, stopping once `limit` are collected.
fn raw_rows(
    conn: &Connection,
    stream_id: i64,
    windows: &[SeriesRange],
    limit: Option<usize>,
) -> Result<Vec<(Timestamp, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT ts, value FROM reading WHERE stream_id = ? AND ts >= ? AND ts < ? \
         ORDER BY ts LIMIT ?",
    )?;
    let mut out = Vec::new();
    for w in windows {
        let remaining = match limit {
            Some(limit) if out.len() >= limit => break,
            Some(limit) => (limit - out.len()) as i64,
            None => -1,
        };
        let rows = stmt.query_map(params![stream_id, w.start, w.end, remaining], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

#[derive(Debug)]
struct Bucket {
    index: i64,
    first_ts: Timestamp,
    last_ts: Timestamp,
    min: f64,
    max: f64,
    sum: f64,
    count: i64,
}

const BUCKET_SQL: &str = "SELECT (ts - ?1) / ?2 AS b, MIN(ts), MAX(ts), \
                                 MIN(value), MAX(value), SUM(value), COUNT(*) \
                            FROM reading \
                           WHERE stream_id = ?3 AND ts >= ?4 AND ts < ?5 \
                           GROUP BY b ORDER BY b";

/// Non-empty buckets across the windows, in order. A bucket straddling two windows is
/// merged into one point: the windows are sorted and disjoint, so it can only be the
/// last bucket of one and the first of the next.
fn bucketed_rows(
    conn: &Connection,
    stream_id: i64,
    windows: &[SeriesRange],
    origin: Timestamp,
    bucket: i64,
) -> Result<Vec<Bucket>> {
    let mut stmt = conn.prepare(BUCKET_SQL)?;
    let mut out: Vec<Bucket> = Vec::new();
    for w in windows {
        let rows = stmt.query_map(params![origin, bucket, stream_id, w.start, w.end], |row| {
            Ok(Bucket {
                index: row.get(0)?,
                first_ts: row.get(1)?,
                last_ts: row.get(2)?,
                min: row.get(3)?,
                max: row.get(4)?,
                sum: row.get(5)?,
                count: row.get(6)?,
            })
        })?;
        for row in rows {
            let b = row?;
            match out.last_mut() {
                Some(last) if last.index == b.index => {
                    last.last_ts = b.last_ts;
                    last.min = last.min.min(b.min);
                    last.max = last.max.max(b.max);
                    last.sum += b.sum;
                    last.count += b.count;
                }
                _ => out.push(b),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, create_batch, set_pitched_at},
        recipe::BeverageType,
        tier1::{create_source, create_stream, insert_readings},
        timeline::bind_stream,
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;

    fn at(offset_secs: i64) -> Timestamp {
        Timestamp::from_secs(T0.as_secs() + offset_secs)
    }

    fn range(start: i64, end: i64) -> Option<SeriesRange> {
        Some(SeriesRange {
            start: at(start),
            end: at(end),
        })
    }

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection, code: &str) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    fn stream(conn: &mut Connection, kind: SourceKind, name: &str, metric: Metric) -> i64 {
        let source_id = create_source(conn, kind, name, None).unwrap();
        let tx = conn.transaction().unwrap();
        let stream_id = create_stream(&tx, source_id, name, metric).unwrap();
        tx.commit().unwrap();
        stream_id
    }

    fn readings(conn: &mut Connection, stream_id: i64, rows: impl IntoIterator<Item = (i64, f64)>) {
        let rows: Vec<_> = rows
            .into_iter()
            .map(|(offset, value)| (stream_id, at(offset), value))
            .collect();
        let tx = conn.transaction().unwrap();
        insert_readings(&tx, &rows).unwrap();
        tx.commit().unwrap();
    }

    /// Every `interval` seconds over `[start, end)`, all at `value`.
    fn flat(start: i64, end: i64, interval: i64, value: f64) -> impl Iterator<Item = (i64, f64)> {
        (start..end)
            .step_by(interval as usize)
            .map(move |t| (t, value))
    }

    fn only(series: &BatchSeries) -> &Series {
        assert_eq!(series.series.len(), 1, "{series:#?}");
        &series.series[0]
    }

    fn gaps(s: &Series) -> usize {
        s.avg.iter().filter(|v| v.is_none()).count()
    }

    fn values(s: &Series) -> Vec<f64> {
        s.avg.iter().map(|v| v.unwrap()).collect()
    }

    #[test]
    fn extremes_survive_bucketing() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), None).unwrap();
        readings(
            &mut conn,
            probe,
            flat(0, 10_000 * 60, 60, 18.0).map(|(t, v)| match t {
                t if t == 4_321 * 60 => (t, 35.0),
                t if t == 7_777 * 60 => (t, 2.0),
                _ => (t, v),
            }),
        );

        let got = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(0, 10_000 * 60),
            100,
            at(DAY * 30),
        )
        .unwrap();
        let s = only(&got);
        assert!(s.bucket_secs.is_some());
        assert!(s.ts.len() <= 100);
        assert!(s.max.contains(&Some(35.0)), "the spike was dropped");
        assert!(s.min.contains(&Some(2.0)), "the dip was dropped");
        // The average smears them, which is why the chart gets min and max too.
        assert!(s.avg.iter().all(|v| v.unwrap() < 35.0 && v.unwrap() > 2.0));
    }

    #[test]
    fn the_point_count_never_exceeds_max_points() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        // Two windows with a hole between them, and a dropout inside the first.
        bind_stream(&conn, batch_id, probe, at(0), Some(at(2 * DAY))).unwrap();
        bind_stream(&conn, batch_id, probe, at(3 * DAY), Some(at(5 * DAY))).unwrap();
        readings(
            &mut conn,
            probe,
            flat(0, 5 * DAY, 60, 18.0).filter(|(t, _)| !(DAY..DAY + 6 * HOUR).contains(t)),
        );

        for max_points in [1, 2, 7, 100, 999, 5_000, 10_000] {
            for (start, end) in [(0, 5 * DAY), (DAY - HOUR, DAY + 7 * HOUR), (0, 10 * HOUR)] {
                let got = series_at(
                    &conn,
                    batch_id,
                    &[Metric::TempC],
                    range(start, end),
                    max_points,
                    at(10 * DAY),
                )
                .unwrap();
                let s = only(&got);
                assert!(
                    s.ts.len() <= max_points,
                    "{} points for max_points {max_points} over [{start}, {end})",
                    s.ts.len()
                );
                assert_eq!(s.avg.len(), s.ts.len());
                if s.bucket_secs.is_some() {
                    assert_eq!(s.min.len(), s.ts.len());
                    assert_eq!(s.max.len(), s.ts.len());
                }
                assert!(s.ts.windows(2).all(|w| w[0] < w[1]), "not strictly ordered");
            }
        }
    }

    #[test]
    fn a_gap_stays_a_gap_raw_and_bucketed() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), None).unwrap();
        // The Pi off for six hours on day two.
        readings(
            &mut conn,
            probe,
            flat(0, 4 * DAY, 60, 18.0).filter(|(t, _)| !(DAY..DAY + 6 * HOUR).contains(t)),
        );
        let now = at(4 * DAY);

        let raw = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(0, 4 * DAY),
            10_000,
            now,
        )
        .unwrap();
        let raw = only(&raw);
        assert_eq!(raw.bucket_secs, None);
        assert_eq!(gaps(raw), 1);
        let hole = raw.avg.iter().position(Option::is_none).unwrap();
        assert!(raw.ts[hole] > at(DAY) && raw.ts[hole] < at(DAY + 6 * HOUR));

        let bucketed = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(0, 4 * DAY),
            200,
            now,
        )
        .unwrap();
        let bucketed = only(&bucketed);
        assert!(bucketed.bucket_secs.is_some());
        assert_eq!(gaps(bucketed), 1);
        let hole = bucketed.avg.iter().position(Option::is_none).unwrap();
        assert!(bucketed.ts[hole] > at(DAY - HOUR) && bucketed.ts[hole] < at(DAY + 6 * HOUR));
    }

    /// Zoomed in past the device's interval, most buckets are empty. That is sparse
    /// data, not a gap, and breaking the line at every other bucket would draw dots.
    #[test]
    fn empty_buckets_narrower_than_the_gap_rule_are_not_gaps() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), None).unwrap();
        // Irregular: 100 s then 160 s, repeating. The median is one of the two, so the
        // gap rule is at least 300 s and nothing here is a gap.
        readings(
            &mut conn,
            probe,
            flat(0, DAY, 260, 18.0).chain(flat(100, DAY, 260, 18.5)),
        );

        // ~665 readings into 600 buckets of 144 s: a 160 s step skips a bucket.
        let got = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(0, DAY),
            600,
            at(DAY),
        )
        .unwrap();
        let s = only(&got);
        assert_eq!(s.bucket_secs, Some(144));
        assert!(
            s.ts.len() < 600,
            "no bucket was empty, so this proves nothing"
        );
        assert_eq!(gaps(s), 0);

        let got = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(HOUR, 2 * HOUR),
            1_000,
            at(DAY),
        )
        .unwrap();
        let s = only(&got);
        assert_eq!(s.bucket_secs, None);
        assert_eq!(gaps(s), 0);
    }

    #[test]
    fn the_window_is_half_open() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(100), Some(at(200))).unwrap();
        readings(
            &mut conn,
            probe,
            [(99, 1.0), (100, 2.0), (199, 3.0), (200, 4.0), (201, 5.0)],
        );

        let got = series_at(&conn, batch_id, &[Metric::TempC], None, 100, at(DAY)).unwrap();
        assert_eq!(got.range, range(100, 200));
        assert_eq!(values(only(&got)), [2.0, 3.0]);
    }

    /// 3d's unplugged-and-reconnected case: one batch, one stream, two windows.
    #[test]
    fn two_bindings_of_one_stream_read_as_one_series_with_a_gap() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), Some(at(DAY))).unwrap();
        bind_stream(&conn, batch_id, probe, at(2 * DAY), None).unwrap();
        readings(&mut conn, probe, flat(0, 3 * DAY, 60, 18.0));

        let got = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            None,
            100_000,
            at(3 * DAY),
        )
        .unwrap();
        let s = only(&got);
        assert_eq!(gaps(s), 1);
        // The day between the windows is not this batch's, even though it is logged.
        assert_eq!(s.ts.len() - 1, 2 * DAY as usize / 60);

        let got = series_at(&conn, batch_id, &[Metric::TempC], None, 50, at(3 * DAY)).unwrap();
        assert_eq!(gaps(only(&got)), 1);
    }

    #[test]
    fn two_batches_sharing_a_fermenter_see_only_their_own_readings() {
        let mut conn = db();
        let first = batch(&conn, "SELTZER-017");
        let second = batch(&conn, "SELTZER-018");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, first, probe, at(0), Some(at(DAY))).unwrap();
        bind_stream(&conn, second, probe, at(DAY), None).unwrap();
        readings(
            &mut conn,
            probe,
            flat(0, DAY, 600, 17.0).chain(flat(DAY, 2 * DAY, 600, 20.0)),
        );

        let now = at(2 * DAY);
        let got = series_at(&conn, first, &[Metric::TempC], None, 1_000, now).unwrap();
        assert!(values(only(&got)).iter().all(|&v| v == 17.0));
        let got = series_at(&conn, second, &[Metric::TempC], None, 1_000, now).unwrap();
        assert!(values(only(&got)).iter().all(|&v| v == 20.0));
        assert_eq!(only(&got).ts[0], at(DAY));
    }

    #[test]
    fn an_ispindel_and_a_hydrometer_are_two_series_on_one_metric() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let ispindel = stream(&mut conn, SourceKind::BrewdJournal, "ispindel", Metric::Sg);
        let manual = stream(&mut conn, SourceKind::Manual, "manual", Metric::Sg);
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        for stream_id in [ispindel, manual, probe] {
            bind_stream(&conn, batch_id, stream_id, at(0), None).unwrap();
        }
        readings(&mut conn, ispindel, flat(0, DAY, 900, 1.040));
        readings(&mut conn, manual, [(HOUR, 1.042), (20 * HOUR, 1.041)]);
        readings(&mut conn, probe, flat(0, DAY, 60, 18.0));

        let got = series_at(&conn, batch_id, &[Metric::Sg], None, 1_000, at(DAY)).unwrap();
        let kinds: Vec<_> = got
            .series
            .iter()
            .map(|s| (s.stream_id, s.source_kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (ispindel, SourceKind::BrewdJournal),
                (manual, SourceKind::Manual)
            ]
        );
        assert_eq!(got.series[1].label, "manual");
    }

    #[test]
    fn a_manual_stream_is_never_bucketed() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let manual = stream(&mut conn, SourceKind::Manual, "manual", Metric::Ph);
        bind_stream(&conn, batch_id, manual, at(0), None).unwrap();
        // Days apart, which a sensor's gap rule would break at every point.
        readings(
            &mut conn,
            manual,
            (0..12).map(|d| (d * 3 * DAY, 3.2 + d as f64 / 100.0)),
        );

        let got = series_at(&conn, batch_id, &[Metric::Ph], None, 5, at(40 * DAY)).unwrap();
        let s = only(&got);
        assert_eq!(s.bucket_secs, None);
        assert_eq!(s.ts.len(), 12);
        assert_eq!(gaps(s), 0);
        assert!(s.min.is_empty() && s.max.is_empty());
    }

    #[test]
    fn the_default_range_starts_at_the_pitch() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), None).unwrap();
        readings(&mut conn, probe, flat(0, DAY, 600, 18.0));
        set_pitched_at(&conn, batch_id, at(HOUR)).unwrap();

        let got = series_at(&conn, batch_id, &[Metric::TempC], None, 1_000, at(DAY)).unwrap();
        assert_eq!(got.range, range(HOUR, DAY));
        assert_eq!(only(&got).ts[0], at(HOUR));
    }

    #[test]
    fn an_unpitched_batch_defaults_to_its_bindings_span() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        let other = stream(
            &mut conn,
            SourceKind::BrewdJournal,
            "room",
            Metric::RoomTempC,
        );
        bind_stream(&conn, batch_id, probe, at(HOUR), Some(at(5 * HOUR))).unwrap();
        bind_stream(&conn, batch_id, other, at(2 * HOUR), Some(at(9 * HOUR))).unwrap();

        // The span is the batch's, whichever metrics are asked for, so two charts of
        // one batch line up.
        let got = series_at(&conn, batch_id, &[Metric::TempC], None, 100, at(DAY)).unwrap();
        assert_eq!(got.range, range(HOUR, 9 * HOUR));
    }

    #[test]
    fn a_batch_with_no_pitch_and_no_bindings_has_no_range() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let got = series_at(&conn, batch_id, &Metric::ALL, None, 100, at(DAY)).unwrap();
        assert_eq!(
            got,
            BatchSeries {
                range: None,
                series: Vec::new()
            }
        );
    }

    #[test]
    fn an_empty_range_is_every_series_empty() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), None).unwrap();
        readings(&mut conn, probe, flat(0, DAY, 600, 18.0));

        let got = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(HOUR, HOUR),
            100,
            at(DAY),
        )
        .unwrap();
        assert!(only(&got).ts.is_empty());
    }

    #[test]
    fn zero_max_points_is_refused() {
        let conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let err = series_at(&conn, batch_id, &Metric::ALL, None, 0, at(DAY)).unwrap_err();
        assert_eq!(err.code(), "units.not_positive");
    }

    #[test]
    fn an_unknown_batch_is_an_error() {
        let conn = db();
        let err = batch_series(&conn, 404, &Metric::ALL, None, 100).unwrap_err();
        assert_eq!(err.code(), "db.no_such_batch");
    }

    #[test]
    fn the_median_interval_ignores_a_dropout() {
        let mut conn = db();
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        readings(
            &mut conn,
            probe,
            flat(0, DAY, 60, 18.0).filter(|(t, _)| !(HOUR..3 * HOUR).contains(t)),
        );
        let whole = range(0, DAY).unwrap();
        assert_eq!(median_interval(&conn, probe, whole).unwrap(), Some(60));
        assert_eq!(
            median_interval(&conn, probe, range(HOUR, 3 * HOUR).unwrap()).unwrap(),
            None
        );
    }

    /// The downsample is only cheap if it rides `reading`'s clustered key.
    #[test]
    fn the_bucket_query_searches_the_primary_key() {
        let conn = db();
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {BUCKET_SQL}"))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params![0, 60, 1, 0, 86_400], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.starts_with("SEARCH reading USING PRIMARY KEY")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("SCAN reading")),
            "{plan:?}"
        );
    }

    /// The Phase 4 "done when": a 60-day, 30 s journal. `cargo test -p brewcore
    /// --release -- --ignored sixty_days` for a real number.
    #[test]
    #[ignore]
    fn sixty_days_at_thirty_seconds() {
        let mut conn = db();
        let batch_id = batch(&conn, "SELTZER-001");
        let probe = stream(&mut conn, SourceKind::BrewdJournal, "probe", Metric::TempC);
        bind_stream(&conn, batch_id, probe, at(0), None).unwrap();
        readings(&mut conn, probe, flat(0, 60 * DAY, 30, 18.0));

        let started = std::time::Instant::now();
        let got = series_at(
            &conn,
            batch_id,
            &[Metric::TempC],
            range(0, 60 * DAY),
            1_000,
            at(60 * DAY),
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert!(only(&got).ts.len() <= 1_000);
        println!(
            "172 800 rows → {} points in {elapsed:?}",
            only(&got).ts.len()
        );
    }
}
