//! The screens' results as rows. Pure: no connection, so each is tested on literals.
//!
//! Each table's header is one list and each row one function; `rows_match_headers`
//! holds them to the same length. Lists inside a cell (batch ids, strain names) are
//! joined with ` | `, which neither dialect's separator is.

use super::{Cell, Table, time_cells};
use crate::{
    calc::batch_metrics::{Computed, Spread},
    db::{
        analytics::{BatchMetrics, StrainRef},
        compare::{Comparison, YAxis},
        query::{QueryResult, Unevaluated},
        rollup::{Rollup, RollupGroup, StrainRollup},
        series::{BatchSeries, Series},
    },
    units::Timestamp,
};

const LIST_SEPARATOR: &str = " | ";

/// A serde-tagged enum's wire name — `no_og`, `below` — the same string the UI gets.
fn tag(v: impl serde::Serialize) -> Cell {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Cell::Text(s),
        _ => Cell::Empty,
    }
}

fn list<T>(items: &[T], f: impl Fn(&T) -> String) -> Cell {
    Cell::Text(items.iter().map(f).collect::<Vec<_>>().join(LIST_SEPARATOR))
}

fn strain_ids(strains: &[StrainRef]) -> Cell {
    list(strains, |s| s.id.to_string())
}

fn strain_names(strains: &[StrainRef]) -> Cell {
    list(strains, |s| s.name.clone())
}

/// `N` value cells then the reason, or `N` empties then the reason.
fn computed<T, const N: usize>(c: &Computed<T>, f: impl FnOnce(&T) -> [Cell; N]) -> Vec<Cell> {
    let (values, reason) = match c {
        Computed::Value(v) => (f(v), Cell::Empty),
        Computed::Missing(m) => (std::array::from_fn(|_| Cell::Empty), tag(m)),
    };
    values.into_iter().chain([reason]).collect()
}

fn spread(s: &Option<Spread>) -> [Cell; 4] {
    match s {
        Some(s) => [
            Cell::num(s.median),
            Cell::num(s.min),
            Cell::num(s.max),
            Cell::count(s.n),
        ],
        None => [Cell::Empty, Cell::Empty, Cell::Empty, Cell::Empty],
    }
}

// ------------------------------------------------------------------ 5c: batches

const METRICS_HEADER: &[&str] = &[
    "match",
    "batch_id",
    "code",
    "beverage_type",
    "status",
    "pitched_ts",
    "pitched_utc",
    "archived_ts",
    "archived_utc",
    "og_sg",
    "fg_sg",
    "lag_secs",
    "lag_missing",
    "peak_rate_sg_per_day",
    "peak_rate_secs_from_pitch",
    "peak_rate_missing",
    "terminal_secs_from_pitch",
    "terminal_fg_unchecked",
    "terminal_missing",
    "apparent_attenuation_fraction",
    "apparent_attenuation_missing",
    "abv_alternate_fraction",
    "abv_alternate_missing",
    "ferm_temp_c_median",
    "ferm_temp_c_min",
    "ferm_temp_c_max",
    "ferm_temp_n",
    "ferm_temp_to_terminal",
    "ferm_temp_missing",
    "yan_added_mg_per_l",
    "yan_added_missing",
    "cost_per_litre",
    "cost_currency",
    "cost_per_litre_missing",
    "cost_lines_without_cost",
    "strain_ids",
    "strains",
    "unevaluated_criterion",
    "unevaluated_reason",
];

/// 5c's matches, then the batches it couldn't judge — listed, as on screen, never
/// dropped and never counted as matches.
pub fn metrics_table(result: &QueryResult) -> Table {
    Table {
        header: METRICS_HEADER.to_vec(),
        rows: result
            .rows
            .iter()
            .map(metrics_row)
            .chain(result.unevaluated.iter().map(unevaluated_row))
            .collect(),
    }
}

fn metrics_row(m: &BatchMetrics) -> Vec<Cell> {
    let mut row = vec![
        Cell::text("matched"),
        Cell::int(m.batch_id),
        Cell::text(&m.code),
        Cell::text(m.beverage_type.as_str()),
        Cell::text(m.status.as_str()),
    ];
    row.extend(time_cells(m.pitched_at));
    row.extend(time_cells(m.archived_at));
    row.push(Cell::opt(m.og, |g| Cell::num(g.get())));
    row.push(Cell::opt(m.fg, |g| Cell::num(g.get())));
    row.extend(computed(&m.lag, |l| [Cell::int(l.secs)]));
    row.extend(computed(&m.peak_rate, |p| {
        [Cell::num(p.sg_per_day), Cell::int(p.secs_from_pitch)]
    }));
    row.extend(computed(&m.terminal, |t| {
        [Cell::int(t.secs_from_pitch), Cell::flag(t.fg_unchecked)]
    }));
    row.extend(computed(&m.apparent_attenuation, |&a| [Cell::num(a)]));
    row.extend(computed(&m.abv_alternate, |&a| [Cell::num(a)]));
    row.extend(computed(&m.ferm_temp, |t| {
        let [median, min, max, n] = spread(&Some(t.spread));
        [median, min, max, n, Cell::flag(t.to_terminal)]
    }));
    row.extend(computed(&m.yan_added_mg_per_l, |&y| [Cell::num(y)]));
    row.extend(computed(&m.cost_per_litre, |c| {
        [Cell::Num(c.decimal()), Cell::text(c.currency.as_str())]
    }));
    row.push(Cell::count(m.cost_lines_without_cost));
    row.push(strain_ids(&m.strains));
    row.push(strain_names(&m.strains));
    row.extend([Cell::Empty, Cell::Empty]);
    row
}

fn unevaluated_row(u: &Unevaluated) -> Vec<Cell> {
    let mut row = vec![Cell::text("unevaluated"), Cell::int(u.batch_id), Cell::text(&u.code)];
    row.resize(METRICS_HEADER.len() - 2, Cell::Empty);
    row.extend([tag(u.criterion), tag(u.reason)]);
    row
}

// ------------------------------------------------------------------ 5d: rollup

const ROLLUP_HEADER: &[&str] = &[
    "group",
    "strain_ids",
    "strains",
    "n",
    "thin",
    "batch_ids",
    "attenuation_fraction_median",
    "attenuation_fraction_min",
    "attenuation_fraction_max",
    "attenuation_fraction_n",
    "lag_secs_median",
    "lag_secs_min",
    "lag_secs_max",
    "lag_secs_n",
    "terminal_secs_median",
    "terminal_secs_min",
    "terminal_secs_max",
    "terminal_secs_n",
    "ferm_temp_c_median",
    "ferm_temp_c_min",
    "ferm_temp_c_max",
    "ferm_temp_c_n",
    "band_min_fraction",
    "band_max_fraction",
    "band_check",
];

/// 5d's rows, then one `no_yeast` row for the matched batches with no strain to count
/// them under — shown under the table on screen, so not dropped here.
pub fn rollup_table(rollup: &Rollup) -> Table {
    let mut rows: Vec<_> = rollup.rows.iter().map(rollup_row).collect();
    if !rollup.no_yeast.is_empty() {
        let mut row = vec![
            Cell::text("no_yeast"),
            Cell::Empty,
            Cell::Empty,
            Cell::count(rollup.no_yeast.len()),
            Cell::Empty,
            list(&rollup.no_yeast, i64::to_string),
        ];
        row.resize(ROLLUP_HEADER.len(), Cell::Empty);
        rows.push(row);
    }
    Table {
        header: ROLLUP_HEADER.to_vec(),
        rows,
    }
}

fn rollup_row(r: &StrainRollup) -> Vec<Cell> {
    let (group, strains) = match &r.group {
        RollupGroup::Strain(s) => ("strain", std::slice::from_ref(s)),
        RollupGroup::Mixed { strains } => ("mixed", strains.as_slice()),
    };
    let mut row = vec![
        Cell::text(group),
        strain_ids(strains),
        strain_names(strains),
        Cell::count(r.n),
        Cell::flag(r.thin),
        list(&r.batch_ids, i64::to_string),
    ];
    for s in [
        &r.attenuation,
        &r.lag_secs,
        &r.terminal_secs,
        &r.ferm_temp_c,
    ] {
        row.extend(spread(s));
    }
    row.push(Cell::opt(r.band, |b| Cell::num(b.min)));
    row.push(Cell::opt(r.band, |b| Cell::num(b.max)));
    row.push(Cell::opt(r.band_check, tag));
    row
}

// ------------------------------------------------------------------ series

const COMPARISON_HEADER: &[&str] = &[
    "batch_id",
    "code",
    "stream_id",
    "source_kind",
    "label",
    "metric",
    "secs_from_pitch",
    "ts",
    "time_utc",
    "value",
    "smoothed",
    "fraction_missing",
    "excluded_reason",
];

/// 5b's overlay, one row per point. Long, not one column per batch: batches don't share
/// timestamps, and one batch can have several gravity streams.
///
/// `metric` is the value's unit: `attenuation_fraction` where 5b's fraction was drawn,
/// else the stream's own (`sg`, `temp_c`, `gravity_temp_c`). `fraction_missing` is why
/// a batch stayed in SG when the fraction was asked for.
pub fn comparison_table(c: &Comparison) -> Table {
    let mut rows = Vec::new();
    for b in &c.batches {
        let (fraction, fraction_missing) = match b.y {
            YAxis::Fraction => (true, Cell::Empty),
            YAxis::Raw { reason } => (false, Cell::opt(reason, tag)),
        };
        let lead = [Cell::int(b.batch_id), Cell::text(&b.code)];
        let tail = [fraction_missing, Cell::Empty];
        for s in &b.gravity {
            let metric = if fraction {
                "attenuation_fraction"
            } else {
                s.metric.as_str()
            };
            point_rows(&mut rows, &lead, s, metric, Some(b.pitched_at), &tail);
        }
        if let Some(s) = &b.temp {
            point_rows(&mut rows, &lead, s, s.metric.as_str(), Some(b.pitched_at), &tail);
        }
    }
    for e in &c.excluded {
        let mut row = vec![Cell::int(e.batch_id), Cell::text(&e.code)];
        row.resize(COMPARISON_HEADER.len() - 1, Cell::Empty);
        row.push(tag(e.reason));
        rows.push(row);
    }
    Table {
        header: COMPARISON_HEADER.to_vec(),
        rows,
    }
}

const READINGS_HEADER: &[&str] = &[
    "stream_id",
    "source_kind",
    "label",
    "metric",
    "secs_from_pitch",
    "ts",
    "time_utc",
    "value",
    "smoothed",
];

/// 4a's series for one batch, one row per reading. `metric` is the unit (`sg`,
/// `temp_c`, …); `secs_from_pitch` is negative before the pitch and empty for a batch
/// never pitched.
pub fn readings_table(series: &BatchSeries, pitched_at: Option<Timestamp>) -> Table {
    let mut rows = Vec::new();
    for s in &series.series {
        point_rows(&mut rows, &[], s, s.metric.as_str(), pitched_at, &[]);
    }
    Table {
        header: READINGS_HEADER.to_vec(),
        rows,
    }
}

/// One row per point, gaps skipped: a gap is an absence, not a reading. `smoothed` is
/// 4b's line, sensor `sg` only.
fn point_rows(
    rows: &mut Vec<Vec<Cell>>,
    lead: &[Cell],
    s: &Series,
    metric: &str,
    pitched_at: Option<Timestamp>,
    tail: &[Cell],
) {
    for (i, (&ts, value)) in s.ts.iter().zip(&s.avg).enumerate() {
        let Some(value) = value else { continue };
        let mut row = lead.to_vec();
        row.extend([
            Cell::int(s.stream_id),
            Cell::text(s.source_kind.as_str()),
            Cell::text(&s.label),
            Cell::text(metric),
            Cell::opt(pitched_at, |p| Cell::int(p.secs_until(ts))),
        ]);
        row.extend(time_cells(Some(ts)));
        row.push(Cell::num(*value));
        row.push(Cell::opt(s.smoothed.get(i).copied().flatten(), Cell::num));
        row.extend_from_slice(tail);
        rows.push(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calc::batch_metrics::{Lag, Missing, PeakRate, Terminal},
        db::{
            Metric, SourceKind,
            analytics::FermTemp,
            batch::BatchStatus,
            compare::{ComparedBatch, Excluded},
            query::Criterion,
            recipe::BeverageType,
            rollup::{AttenuationBand, BandCheck},
        },
        units::{Currency, Money, Sg},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const HOUR: i64 = 3_600;

    fn strain(id: i64, name: &str) -> StrainRef {
        StrainRef {
            id,
            name: name.into(),
        }
    }

    fn missing(m: Missing) -> BatchMetrics {
        BatchMetrics {
            batch_id: 2,
            code: "SELTZER-002".into(),
            beverage_type: BeverageType::Seltzer,
            status: BatchStatus::Planned,
            archived_at: None,
            pitched_at: None,
            og: None,
            fg: None,
            lag: Computed::Missing(m),
            peak_rate: Computed::Missing(m),
            terminal: Computed::Missing(m),
            apparent_attenuation: Computed::Missing(Missing::NoOg),
            abv_alternate: Computed::Missing(Missing::NoOg),
            ferm_temp: Computed::Missing(m),
            yan_added_mg_per_l: Computed::Missing(Missing::NoNutrientYield),
            cost_per_litre: Computed::Missing(Missing::MixedCurrency),
            cost_lines_without_cost: 0,
            strains: Vec::new(),
        }
    }

    fn complete() -> BatchMetrics {
        BatchMetrics {
            batch_id: 1,
            code: "SELTZER-001".into(),
            status: BatchStatus::Fermenting,
            pitched_at: Some(T0),
            og: Some(Sg::new(1.038).unwrap()),
            fg: Some(Sg::new(0.998).unwrap()),
            lag: Computed::Value(Lag {
                at: T0,
                secs: 9 * HOUR,
            }),
            peak_rate: Computed::Value(PeakRate {
                sg_per_day: 0.012,
                at: T0,
                secs_from_pitch: 30 * HOUR,
            }),
            terminal: Computed::Value(Terminal {
                at: T0,
                secs_from_pitch: 120 * HOUR,
                fg_unchecked: true,
            }),
            apparent_attenuation: Computed::Value(1.05),
            abv_alternate: Computed::Value(0.053),
            ferm_temp: Computed::Value(FermTemp {
                spread: Spread {
                    median: 18.0,
                    min: 17.5,
                    max: 19.25,
                    n: 400,
                },
                stream_id: 7,
                to_terminal: true,
            }),
            yan_added_mg_per_l: Computed::Value(250.0),
            cost_per_litre: Computed::Value(Money::new(42, Currency::new("EUR").unwrap())),
            cost_lines_without_cost: 1,
            strains: vec![strain(3, "EC-1118"), strain(4, "Côte des Blancs; \"CdB\"")],
            ..missing(Missing::NoPitch)
        }
    }

    fn column<'a>(t: &'a Table, row: usize, name: &str) -> &'a Cell {
        let i = t.header.iter().position(|h| *h == name).expect(name);
        &t.rows[row][i]
    }

    fn series(metric: Metric, ts: &[i64], avg: &[Option<f64>], smoothed: &[Option<f64>]) -> Series {
        Series {
            stream_id: 5,
            source_id: 1,
            metric,
            source_kind: SourceKind::BrewdJournal,
            label: "iSpindel".into(),
            bucket_secs: None,
            ts: ts.iter().map(|&t| T0.saturating_add_secs(t)).collect(),
            avg: avg.to_vec(),
            min: Vec::new(),
            max: Vec::new(),
            smoothed: smoothed.to_vec(),
            smoothed_partial: vec![false; smoothed.len()],
        }
    }

    fn query_result() -> QueryResult {
        QueryResult {
            rows: vec![complete(), missing(Missing::NoPitch)],
            unevaluated: vec![Unevaluated {
                batch_id: 9,
                code: "WINE-009".into(),
                criterion: Criterion::FermTemp,
                reason: Missing::NoTemp,
            }],
        }
    }

    fn rollup() -> Rollup {
        let s = Spread {
            median: 0.98,
            min: 0.9,
            max: 1.1,
            n: 2,
        };
        Rollup {
            rows: vec![
                StrainRollup {
                    group: RollupGroup::Strain(strain(3, "EC-1118")),
                    batch_ids: vec![1, 4],
                    n: 2,
                    thin: true,
                    attenuation: Some(s),
                    lag_secs: None,
                    terminal_secs: Some(s),
                    ferm_temp_c: Some(s),
                    band: Some(AttenuationBand { min: 0.8, max: 0.95 }),
                    band_check: Some(BandCheck::Above),
                },
                StrainRollup {
                    group: RollupGroup::Mixed {
                        strains: vec![strain(3, "EC-1118"), strain(5, "K1-V1116")],
                    },
                    batch_ids: vec![6],
                    n: 1,
                    thin: true,
                    attenuation: None,
                    lag_secs: None,
                    terminal_secs: None,
                    ferm_temp_c: None,
                    band: None,
                    band_check: None,
                },
            ],
            no_yeast: vec![7, 8],
        }
    }

    fn comparison() -> Comparison {
        let gravity = series(Metric::Sg, &[-HOUR, 0, HOUR], &[Some(0.0), None, Some(0.1)], &[
            Some(0.0),
            None,
            Some(0.09),
        ]);
        Comparison {
            span: None,
            batches: vec![
                ComparedBatch {
                    batch_id: 1,
                    code: "A".into(),
                    pitched_at: T0,
                    og: None,
                    y: YAxis::Fraction,
                    gravity: vec![gravity.clone()],
                    temp: Some(series(Metric::TempC, &[0], &[Some(18.5)], &[])),
                },
                ComparedBatch {
                    batch_id: 2,
                    code: "B".into(),
                    pitched_at: T0.saturating_add_secs(HOUR),
                    og: None,
                    y: YAxis::Raw {
                        reason: Some(Missing::NoOg),
                    },
                    gravity: vec![series(Metric::Sg, &[HOUR], &[Some(1.04)], &[Some(1.04)])],
                    temp: None,
                },
            ],
            excluded: vec![Excluded {
                batch_id: 3,
                code: "C".into(),
                reason: Missing::NoPitch,
            }],
        }
    }

    #[test]
    fn rows_match_headers() {
        let series = BatchSeries {
            range: None,
            series: vec![series(Metric::Sg, &[0], &[Some(1.0)], &[None])],
        };
        for t in [
            metrics_table(&query_result()),
            rollup_table(&rollup()),
            comparison_table(&comparison()),
            readings_table(&series, Some(T0)),
        ] {
            assert!(!t.rows.is_empty());
            for row in &t.rows {
                assert_eq!(row.len(), t.header.len(), "{:?}", t.header[0]);
            }
        }
    }

    #[test]
    fn a_metric_is_its_value_or_empty_with_the_reason() {
        let t = metrics_table(&query_result());
        assert_eq!(column(&t, 0, "og_sg"), &Cell::Num("1.038".into()));
        assert_eq!(column(&t, 0, "lag_secs"), &Cell::Num("32400".into()));
        assert_eq!(column(&t, 0, "lag_missing"), &Cell::Empty);
        assert_eq!(column(&t, 0, "terminal_fg_unchecked"), &Cell::text("true"));
        assert_eq!(column(&t, 0, "ferm_temp_c_max"), &Cell::Num("19.25".into()));
        assert_eq!(column(&t, 0, "cost_per_litre"), &Cell::Num("0.42".into()));
        assert_eq!(column(&t, 0, "cost_currency"), &Cell::text("EUR"));
        assert_eq!(column(&t, 0, "pitched_utc"), &Cell::text("2023-11-14T22:13:20Z"));
        assert_eq!(
            column(&t, 0, "strains"),
            &Cell::text("EC-1118 | Côte des Blancs; \"CdB\"")
        );

        assert_eq!(column(&t, 1, "og_sg"), &Cell::Empty);
        assert_eq!(column(&t, 1, "lag_secs"), &Cell::Empty);
        assert_eq!(column(&t, 1, "lag_missing"), &Cell::text("no_pitch"));
        assert_eq!(column(&t, 1, "ferm_temp_c_median"), &Cell::Empty);
        assert_eq!(column(&t, 1, "ferm_temp_missing"), &Cell::text("no_pitch"));
        assert_eq!(column(&t, 1, "cost_per_litre_missing"), &Cell::text("mixed_currency"));
        assert_eq!(column(&t, 1, "strains"), &Cell::text(""));
    }

    #[test]
    fn unevaluated_batches_are_listed_not_matched() {
        let t = metrics_table(&query_result());
        assert_eq!(t.rows.len(), 3);
        assert_eq!(column(&t, 2, "match"), &Cell::text("unevaluated"));
        assert_eq!(column(&t, 2, "code"), &Cell::text("WINE-009"));
        assert_eq!(column(&t, 2, "og_sg"), &Cell::Empty);
        assert_eq!(column(&t, 2, "unevaluated_criterion"), &Cell::text("ferm_temp"));
        assert_eq!(column(&t, 2, "unevaluated_reason"), &Cell::text("no_temp"));
    }

    #[test]
    fn the_rollup_carries_thin_the_band_and_the_batches_without_yeast() {
        let t = rollup_table(&rollup());
        assert_eq!(t.rows.len(), 3);
        assert_eq!(column(&t, 0, "group"), &Cell::text("strain"));
        assert_eq!(column(&t, 0, "thin"), &Cell::text("true"));
        assert_eq!(column(&t, 0, "batch_ids"), &Cell::text("1 | 4"));
        assert_eq!(column(&t, 0, "lag_secs_median"), &Cell::Empty);
        assert_eq!(column(&t, 0, "attenuation_fraction_n"), &Cell::Num("2".into()));
        assert_eq!(column(&t, 0, "band_check"), &Cell::text("above"));

        assert_eq!(column(&t, 1, "group"), &Cell::text("mixed"));
        assert_eq!(column(&t, 1, "strain_ids"), &Cell::text("3 | 5"));
        assert_eq!(column(&t, 1, "band_check"), &Cell::Empty);

        assert_eq!(column(&t, 2, "group"), &Cell::text("no_yeast"));
        assert_eq!(column(&t, 2, "n"), &Cell::Num("2".into()));
        assert_eq!(column(&t, 2, "batch_ids"), &Cell::text("7 | 8"));
    }

    #[test]
    fn the_overlay_is_one_row_per_point_in_its_own_unit() {
        let t = comparison_table(&comparison());
        // A: two gravity points (the gap skipped) and one temperature; B: one; C: excluded.
        assert_eq!(t.rows.len(), 5);
        assert_eq!(column(&t, 0, "metric"), &Cell::text("attenuation_fraction"));
        assert_eq!(column(&t, 0, "secs_from_pitch"), &Cell::Num("-3600".into()));
        assert_eq!(column(&t, 1, "smoothed"), &Cell::Num("0.09".into()));
        assert_eq!(column(&t, 2, "metric"), &Cell::text("temp_c"));
        assert_eq!(column(&t, 2, "value"), &Cell::Num("18.5".into()));

        assert_eq!(column(&t, 3, "metric"), &Cell::text("sg"));
        assert_eq!(column(&t, 3, "secs_from_pitch"), &Cell::Num("0".into()));
        assert_eq!(column(&t, 3, "fraction_missing"), &Cell::text("no_og"));

        assert_eq!(column(&t, 4, "code"), &Cell::text("C"));
        assert_eq!(column(&t, 4, "ts"), &Cell::Empty);
        assert_eq!(column(&t, 4, "excluded_reason"), &Cell::text("no_pitch"));
    }

    #[test]
    fn readings_without_a_pitch_have_no_offset() {
        let s = BatchSeries {
            range: None,
            series: vec![series(Metric::TempC, &[0, 60], &[Some(18.0), Some(18.5)], &[])],
        };
        let t = readings_table(&s, None);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(column(&t, 1, "secs_from_pitch"), &Cell::Empty);
        assert_eq!(column(&t, 1, "ts"), &Cell::Num("1700000060".into()));
        assert_eq!(column(&t, 1, "smoothed"), &Cell::Empty);
        assert_eq!(column(&t, 1, "source_kind"), &Cell::text("brewd_journal"));
    }
}
