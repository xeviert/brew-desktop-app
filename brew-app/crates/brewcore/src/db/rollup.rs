//! Phase 5d. 5a's metrics grouped by the strain actually pitched: how a yeast behaves in
//! this process, beside what its datasheet claims.
//!
//! **Over a query's rows.** The rollup groups whatever 5c's [`run_query`] matched, so
//! "EC-1118 on seltzer only" is a [`BatchFilter`] and not a second way of choosing
//! batches. Batches 5c couldn't judge aren't in `rows`, so they aren't here either.
//!
//! **Median, not mean.** At n = 4 one stuck batch owns the mean; the median ignores it
//! and the range still shows it.
//!
//! **Each batch counted once.** A batch pitched with two strains is neither strain's
//! evidence, so it goes to one [`RollupGroup::Mixed`] row rather than into both.
//!
//! [`run_query`]: super::query::run_query
//! [`BatchFilter`]: super::query::BatchFilter

use std::collections::HashMap;

use rusqlite::Connection;

use super::{
    analytics::{BatchMetrics, StrainRef},
    reference::list_yeast_strains,
};
use crate::{
    Result,
    calc::batch_metrics::{Computed, Spread, spread},
};

/// Below this a row is shown, greyed: two batches are an anecdote, not a strain's
/// behaviour.
pub const MIN_ROLLUP_N: usize = 3;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RollupGroup {
    Strain(StrainRef),
    /// Every batch pitched with more than one strain, whichever they were.
    Mixed {
        strains: Vec<StrainRef>,
    },
}

/// The strain record's `attenuation_min`/`max`, as fractions like 5a's attenuation.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct AttenuationBand {
    pub min: f64,
    pub max: f64,
}

/// Where the measured median falls against the band. Either side is a finding: the
/// strain record is wrong, or the process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BandCheck {
    Below,
    Within,
    Above,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StrainRollup {
    pub group: RollupGroup,
    pub batch_ids: Vec<i64>,
    pub n: usize,
    /// `n < MIN_ROLLUP_N`.
    pub thin: bool,
    /// Each over the batches that have the metric; its own `n` says how many did.
    pub attenuation: Option<Spread>,
    pub lag_secs: Option<Spread>,
    pub terminal_secs: Option<Spread>,
    /// Of each batch's median ferment temperature, °C.
    pub ferm_temp_c: Option<Spread>,
    /// Only for a single strain with both ends recorded.
    pub band: Option<AttenuationBand>,
    pub band_check: Option<BandCheck>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Rollup {
    /// Most batches first, then by name; the mixed row last.
    pub rows: Vec<StrainRollup>,
    /// Matched batches with no yeast addition recorded: no strain to count them under.
    pub no_yeast: Vec<i64>,
}

/// [`group`] with each strain's band read from its record.
pub fn strain_rollup(conn: &Connection, rows: &[BatchMetrics]) -> Result<Rollup> {
    let bands = list_yeast_strains(conn)?
        .into_iter()
        .filter_map(|s| s.attenuation_band().map(|b| (s.id, b)))
        .collect();
    Ok(group(rows, &bands))
}

pub fn group(rows: &[BatchMetrics], bands: &HashMap<i64, (f64, f64)>) -> Rollup {
    let mut by_strain: Vec<(StrainRef, Vec<&BatchMetrics>)> = Vec::new();
    let mut mixed: Vec<&BatchMetrics> = Vec::new();
    let mut no_yeast = Vec::new();

    for m in rows {
        match m.strains.as_slice() {
            [] => no_yeast.push(m.batch_id),
            [strain] => match by_strain.iter_mut().find(|(s, _)| s.id == strain.id) {
                Some((_, batches)) => batches.push(m),
                None => by_strain.push((strain.clone(), vec![m])),
            },
            _ => mixed.push(m),
        }
    }

    by_strain.sort_by(|(a, am), (b, bm)| bm.len().cmp(&am.len()).then(a.name.cmp(&b.name)));
    let mut out: Vec<StrainRollup> = by_strain
        .into_iter()
        .map(|(strain, batches)| {
            let band = bands
                .get(&strain.id)
                .map(|&(min, max)| AttenuationBand { min, max });
            summarise(RollupGroup::Strain(strain), &batches, band)
        })
        .collect();

    if !mixed.is_empty() {
        let mut strains: Vec<StrainRef> = Vec::new();
        for s in mixed.iter().flat_map(|m| &m.strains) {
            if !strains.iter().any(|x| x.id == s.id) {
                strains.push(s.clone());
            }
        }
        strains.sort_by(|a, b| a.name.cmp(&b.name));
        out.push(summarise(RollupGroup::Mixed { strains }, &mixed, None));
    }

    Rollup {
        rows: out,
        no_yeast,
    }
}

fn summarise(
    group: RollupGroup,
    batches: &[&BatchMetrics],
    band: Option<AttenuationBand>,
) -> StrainRollup {
    let over = |f: fn(&BatchMetrics) -> Option<f64>| {
        spread(&batches.iter().filter_map(|m| f(m)).collect::<Vec<_>>())
    };
    let attenuation = over(|m| m.apparent_attenuation.value().copied());
    let band_check = band.zip(attenuation).map(|(b, a)| {
        if a.median < b.min {
            BandCheck::Below
        } else if a.median > b.max {
            BandCheck::Above
        } else {
            BandCheck::Within
        }
    });
    StrainRollup {
        group,
        batch_ids: batches.iter().map(|m| m.batch_id).collect(),
        n: batches.len(),
        thin: batches.len() < MIN_ROLLUP_N,
        attenuation,
        lag_secs: over(|m| m.lag.value().map(|l| l.secs as f64)),
        terminal_secs: over(|m| m.terminal.value().map(|t| t.secs_from_pitch as f64)),
        ferm_temp_c: over(|m| match &m.ferm_temp {
            Computed::Value(t) => Some(t.spread.median),
            Computed::Missing(_) => None,
        }),
        band,
        band_check,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calc::batch_metrics::{Lag, Missing, Terminal},
        db::{analytics::FermTemp, batch::BatchStatus, recipe::BeverageType},
        units::Timestamp,
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const HOUR: i64 = 3_600;

    fn strain(id: i64, name: &str) -> StrainRef {
        StrainRef {
            id,
            name: name.into(),
        }
    }

    /// A batch with only attenuation known; the builders below fill in the rest.
    fn batch(id: i64, strains: Vec<StrainRef>, attenuation: Option<f64>) -> BatchMetrics {
        BatchMetrics {
            batch_id: id,
            code: format!("SELTZER-{id:03}"),
            beverage_type: BeverageType::Seltzer,
            status: BatchStatus::Fermenting,
            archived_at: None,
            pitched_at: Some(T0),
            og: None,
            fg: None,
            lag: Computed::Missing(Missing::NoPitch),
            peak_rate: Computed::Missing(Missing::NoPitch),
            terminal: Computed::Missing(Missing::NoPitch),
            apparent_attenuation: attenuation
                .map_or(Computed::Missing(Missing::NoFg), Computed::Value),
            abv_alternate: Computed::Missing(Missing::NoPitch),
            ferm_temp: Computed::Missing(Missing::NoPitch),
            yan_added_mg_per_l: Computed::Missing(Missing::NoPitch),
            cost_per_litre: Computed::Missing(Missing::NoPitch),
            cost_lines_without_cost: 0,
            strains,
        }
    }

    fn timed(mut m: BatchMetrics, lag_h: i64, terminal_h: i64, temp_c: f64) -> BatchMetrics {
        m.lag = Computed::Value(Lag {
            at: T0,
            secs: lag_h * HOUR,
        });
        m.terminal = Computed::Value(Terminal {
            at: T0,
            secs_from_pitch: terminal_h * HOUR,
            fg_unchecked: false,
        });
        m.ferm_temp = Computed::Value(FermTemp {
            spread: spread(&[temp_c]).unwrap(),
            stream_id: 1,
            to_terminal: true,
        });
        m
    }

    fn no_bands() -> HashMap<i64, (f64, f64)> {
        HashMap::new()
    }

    #[test]
    fn a_strain_row_is_median_and_range() {
        let ec = strain(1, "EC-1118");
        let rows = [
            timed(batch(1, vec![ec.clone()], Some(0.98)), 10, 120, 18.0),
            timed(batch(2, vec![ec.clone()], Some(1.02)), 14, 96, 19.0),
            timed(batch(3, vec![ec.clone()], Some(1.00)), 12, 108, 18.5),
        ];
        let got = group(&rows, &no_bands());

        assert_eq!(got.rows.len(), 1);
        let row = &got.rows[0];
        assert_eq!(row.group, RollupGroup::Strain(ec));
        assert_eq!(row.batch_ids, [1, 2, 3]);
        assert_eq!((row.n, row.thin), (3, false));
        let a = row.attenuation.unwrap();
        assert_eq!((a.median, a.min, a.max, a.n), (1.00, 0.98, 1.02, 3));
        assert_eq!(row.lag_secs.unwrap().median, (12 * HOUR) as f64);
        assert_eq!(row.terminal_secs.unwrap().median, (108 * HOUR) as f64);
        assert_eq!(row.ferm_temp_c.unwrap().median, 18.5);
    }

    /// The reason it's a median: at n = 4 a stuck batch would drag the mean ten points.
    #[test]
    fn one_stuck_batch_does_not_move_the_median() {
        let ec = strain(1, "EC-1118");
        let rows = [
            batch(1, vec![ec.clone()], Some(0.99)),
            batch(2, vec![ec.clone()], Some(1.00)),
            batch(3, vec![ec.clone()], Some(1.01)),
            batch(4, vec![ec.clone()], Some(0.55)),
        ];
        let a = group(&rows, &no_bands()).rows[0].attenuation.unwrap();

        assert!((a.median - 0.995).abs() < 1e-9, "{}", a.median);
        assert_eq!(a.min, 0.55);
    }

    #[test]
    fn a_two_strain_batch_is_counted_once_under_mixed() {
        let ec = strain(1, "EC-1118");
        let us = strain(2, "US-05");
        let rows = [
            batch(1, vec![ec.clone()], Some(1.0)),
            batch(2, vec![us.clone(), ec.clone()], Some(0.9)),
            batch(3, vec![us.clone()], Some(0.8)),
            batch(4, vec![us.clone()], Some(0.8)),
        ];
        let got = group(&rows, &no_bands());

        let groups: Vec<_> = got.rows.iter().map(|r| (&r.group, &r.batch_ids)).collect();
        assert_eq!(
            groups,
            [
                (&RollupGroup::Strain(us.clone()), &vec![3, 4]),
                (&RollupGroup::Strain(ec.clone()), &vec![1]),
                (
                    &RollupGroup::Mixed {
                        strains: vec![ec, us]
                    },
                    &vec![2]
                ),
            ]
        );
        assert_eq!(got.rows.iter().map(|r| r.n).sum::<usize>(), 4);
    }

    #[test]
    fn a_batch_without_yeast_is_listed_not_grouped() {
        let rows = [
            batch(1, vec![], Some(1.0)),
            batch(2, vec![strain(1, "EC-1118")], Some(1.0)),
        ];
        let got = group(&rows, &no_bands());

        assert_eq!(got.no_yeast, [1]);
        assert_eq!(got.rows[0].batch_ids, [2]);
    }

    /// A batch that can't say is counted out of that metric only.
    #[test]
    fn a_missing_metric_is_left_out_of_its_spread_only() {
        let ec = strain(1, "EC-1118");
        let rows = [
            timed(batch(1, vec![ec.clone()], Some(1.0)), 10, 100, 18.0),
            timed(batch(2, vec![ec.clone()], None), 20, 200, 20.0),
            batch(3, vec![ec.clone()], Some(0.9)),
        ];
        let row = &group(&rows, &no_bands()).rows[0];

        assert_eq!(row.n, 3);
        assert_eq!(row.attenuation.unwrap().n, 2);
        assert_eq!(row.lag_secs.unwrap().n, 2);
        assert_eq!(row.ferm_temp_c.unwrap().n, 2);
    }

    #[test]
    fn nothing_known_is_none_not_zero() {
        let row = &group(&[batch(1, vec![strain(1, "EC-1118")], None)], &no_bands()).rows[0];

        assert_eq!(row.attenuation, None);
        assert_eq!(row.lag_secs, None);
        assert_eq!(row.band_check, None);
    }

    #[test]
    fn the_median_is_checked_against_the_strain_band() {
        let bands = HashMap::from([(1, (0.90, 1.00)), (2, (0.90, 1.00)), (3, (0.90, 1.00))]);
        let rows = [
            batch(1, vec![strain(1, "A")], Some(0.85)),
            batch(2, vec![strain(2, "B")], Some(0.95)),
            batch(3, vec![strain(3, "C")], Some(1.03)),
            batch(4, vec![strain(4, "D")], Some(0.95)),
        ];
        let got = group(&rows, &bands);

        let checks: Vec<_> = got.rows.iter().map(|r| r.band_check).collect();
        assert_eq!(
            checks,
            [
                Some(BandCheck::Below),
                Some(BandCheck::Within),
                Some(BandCheck::Above),
                None,
            ]
        );
        assert_eq!(
            got.rows[0].band,
            Some(AttenuationBand {
                min: 0.90,
                max: 1.00
            })
        );
        assert_eq!(got.rows[3].band, None);
    }

    #[test]
    fn a_mixed_row_has_no_band() {
        let bands = HashMap::from([(1, (0.0, 0.5)), (2, (0.0, 0.5))]);
        let rows = [batch(1, vec![strain(1, "A"), strain(2, "B")], Some(1.0))];
        let row = &group(&rows, &bands).rows[0];

        assert!(matches!(row.group, RollupGroup::Mixed { .. }));
        assert_eq!((row.band, row.band_check), (None, None));
    }

    #[test]
    fn under_three_batches_is_thin() {
        let ec = strain(1, "EC-1118");
        let two = [
            batch(1, vec![ec.clone()], None),
            batch(2, vec![ec.clone()], None),
        ];
        let three = [two[0].clone(), two[1].clone(), batch(3, vec![ec], None)];

        assert!(group(&two, &no_bands()).rows[0].thin);
        assert!(!group(&three, &no_bands()).rows[0].thin);
    }

    #[test]
    fn serialises_the_group_with_a_kind() {
        let json = serde_json::to_value(RollupGroup::Strain(strain(1, "EC-1118"))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "strain", "id": 1, "name": "EC-1118" })
        );
        let json = serde_json::to_value(RollupGroup::Mixed { strains: vec![] }).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "mixed", "strains": [] }));
    }
}
