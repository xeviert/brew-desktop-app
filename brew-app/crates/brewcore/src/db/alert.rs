//! Phase 6, the desktop's half: what `brewd` should watch, and what it said.
//!
//! [`build_watches`] turns `brewery.db` into the [`WatchSet`] `brewd` judges — every
//! fermenting or conditioning batch that isn't archived or muted, with its journal
//! feeds, OG, expected FG and temperature band. [`apply_records`] folds `brewd`'s alert
//! log back into `alert`, so an alert raised with the laptop shut is on screen when it
//! opens.
//!
//! Same contract as the rest of `db`: a borrowed connection, no transaction of its own
//! except where a write must land whole.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params, types::FromSql, types::ToSql};

use super::{
    Metric, SourceKind,
    batch::{BATCH_COLUMNS, BatchStatus, batch_from_row, get_batch},
    schedule::{StepRow, list_steps, step_start},
    stability::expected_fg,
};
use crate::{
    Error, Result,
    alert::{Feed, GravityFeed, LogEvent, LogRecord, Rule, Watch},
    brewd::discovery::JOURNALS_PATH,
    config::Config,
    journal::{JOURNAL_EXTENSION, rotated_stem},
    units::{Sg, Timestamp},
};

impl ToSql for Rule {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for Rule {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        text.parse().map_err(|_| {
            rusqlite::types::FromSqlError::Other(format!("unknown rule '{text}'").into())
        })
    }
}

// ------------------------------------------------------------------ settings

/// A batch's own alert settings. Every field unset is the defaults.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BatchAlert {
    /// `None` takes the yeast's.
    pub temp_min_c: Option<f64>,
    pub temp_max_c: Option<f64>,
    pub target_sg: Option<Sg>,
    pub muted: bool,
}

/// A batch's settings and the yeast band the unset ends fall back to, for the form's
/// placeholders.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchAlertView {
    pub settings: BatchAlert,
    pub yeast_min_c: Option<f64>,
    pub yeast_max_c: Option<f64>,
}

pub fn get_batch_alert(conn: &Connection, batch_id: i64) -> Result<BatchAlertView> {
    get_batch(conn, batch_id)?;
    let (yeast_min_c, yeast_max_c) = yeast_temp_band(conn, batch_id)?;
    Ok(BatchAlertView {
        settings: settings(conn, batch_id)?,
        yeast_min_c,
        yeast_max_c,
    })
}

fn settings(conn: &Connection, batch_id: i64) -> Result<BatchAlert> {
    Ok(conn
        .query_row(
            "SELECT temp_min_c, temp_max_c, target_sg, muted FROM batch_alert WHERE batch_id = ?",
            [batch_id],
            |row| {
                Ok(BatchAlert {
                    temp_min_c: row.get(0)?,
                    temp_max_c: row.get(1)?,
                    target_sg: row.get(2)?,
                    muted: row.get(3)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub fn set_batch_alert(conn: &Connection, batch_id: i64, alert: &BatchAlert) -> Result<()> {
    get_batch(conn, batch_id)?;
    for (field, value) in [
        ("temp_min_c", alert.temp_min_c),
        ("temp_max_c", alert.temp_max_c),
    ] {
        if let Some(value) = value.filter(|v| !v.is_finite()) {
            return Err(Error::NotFinite { field, value });
        }
    }
    if let (Some(lo), Some(hi)) = (alert.temp_min_c, alert.temp_max_c) {
        if lo > hi {
            return Err(Error::RangeInverted { field: "temp band" });
        }
    }
    conn.execute(
        "INSERT INTO batch_alert (batch_id, temp_min_c, temp_max_c, target_sg, muted) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(batch_id) DO UPDATE SET temp_min_c = ?2, temp_max_c = ?3, \
         target_sg = ?4, muted = ?5",
        params![
            batch_id,
            alert.temp_min_c,
            alert.temp_max_c,
            alert.target_sg,
            alert.muted
        ],
    )?;
    Ok(())
}

/// Where every yeast actually pitched is comfortable: the highest minimum and the
/// lowest maximum. From actuals, as 5a's strain is — a substituted yeast is the one
/// whose band matters.
fn yeast_temp_band(conn: &Connection, batch_id: i64) -> Result<(Option<f64>, Option<f64>)> {
    Ok(conn.query_row(
        "SELECT MAX(ys.temp_min_c), MIN(ys.temp_max_c) \
         FROM batch_ingredient bi \
         JOIN ingredient_lot l ON l.id = bi.ingredient_lot_id \
         JOIN ingredient i ON i.id = l.ingredient_id \
         JOIN yeast_strain ys ON ys.id = i.yeast_strain_id \
         WHERE bi.batch_id = ?",
        [batch_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

// ------------------------------------------------------------------ watches

/// An open binding of a journal stream: what a feed is made of.
struct JournalFeed {
    device: String,
    column: String,
    metric: Metric,
    since: Timestamp,
}

/// Open bindings of `brewd` journal streams, newest binding first. A stream on a
/// rotated journal has no live device to judge, so it is left out.
fn journal_feeds(conn: &Connection, batch_id: i64) -> Result<Vec<JournalFeed>> {
    let mut stmt = conn.prepare(
        "SELECT ss.remote_path, s.external_id, s.metric, bs.starts_at \
         FROM batch_stream bs \
         JOIN stream s ON s.id = bs.stream_id \
         JOIN source src ON src.id = s.source_id \
         JOIN sync_state ss ON ss.source_id = src.id \
         WHERE bs.batch_id = ? AND bs.ends_at IS NULL AND src.kind = ? \
         ORDER BY bs.starts_at DESC, s.id",
    )?;
    let rows = stmt.query_map(params![batch_id, SourceKind::BrewdJournal], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Metric>(2)?,
            row.get::<_, Timestamp>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (path, column, metric, since) = row?;
        let Some(name) = path.strip_prefix(JOURNALS_PATH) else {
            continue;
        };
        if rotated_stem(name).is_some() {
            continue;
        }
        let Some(device) = name
            .strip_suffix(JOURNAL_EXTENSION)
            .and_then(|s| s.strip_suffix('.'))
        else {
            continue;
        };
        out.push(JournalFeed {
            device: device.to_owned(),
            column,
            metric,
            since,
        });
    }
    Ok(out)
}

/// Every batch `brewd` should judge, as of `now`'s database. A batch with no journal
/// feed has nothing to judge and is left out; so is a muted one.
pub fn build_watches(conn: &Connection, config: &Config) -> Result<Vec<Watch>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BATCH_COLUMNS} FROM batch \
         WHERE status IN (?, ?) AND archived_at IS NULL ORDER BY id"
    ))?;
    let batches = stmt
        .query_map(
            params![BatchStatus::Fermenting, BatchStatus::Conditioning],
            batch_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::new();
    for batch in batches {
        let own = settings(conn, batch.id)?;
        if own.muted {
            continue;
        }
        let feeds = journal_feeds(conn, batch.id)?;
        if feeds.is_empty() {
            continue;
        }
        let find = |metric| feeds.iter().find(|f| f.metric == metric);
        // Only once the desktop knows the unit: a gravity column read in the wrong one
        // would judge a 12 °P wash as finished.
        let gravity = find(Metric::Sg)
            .zip(config.gravity.ispindel_unit)
            .map(|(f, unit)| GravityFeed {
                device: f.device.clone(),
                column: f.column.clone(),
                since: f.since,
                unit,
            });
        // 5a's ferment temperature: the vessel probe, else the iSpindle's own.
        let temp = find(Metric::TempC)
            .or_else(|| find(Metric::GravityTempC))
            .map(|f| Feed {
                device: f.device.clone(),
                column: f.column.clone(),
                since: f.since,
            });
        let devices: BTreeSet<String> = feeds.iter().map(|f| f.device.clone()).collect();

        let steps = list_steps(conn, batch.id)?;
        let scheduled = schedule_band(&steps, batch.pitched_at);
        let mut layers: Vec<_> = scheduled.map(|(band, _)| band).into_iter().collect();
        layers.push((own.temp_min_c, own.temp_max_c));
        layers.push(yeast_temp_band(conn, batch.id)?);
        let band = resolve_band(&layers);

        out.push(Watch {
            batch_id: batch.id,
            code: batch.code,
            gravity,
            temp,
            devices: devices.into_iter().collect(),
            og: batch.og,
            expected_fg: expected_fg(conn, batch.id)?,
            target_sg: own.target_sg,
            temp_min_c: band.0,
            temp_max_c: band.1,
            band_since: scheduled.map(|(_, since)| since),
            stability: config.stability.params(),
            health: config.health.params(),
        });
    }
    Ok(out)
}

/// The band the schedule sets now and when it began: the current step's, else the
/// nearest step before it that sets one. Once every step is done, the last that did.
fn schedule_band(steps: &[StepRow], pitched_at: Option<Timestamp>) -> Option<(Band, Timestamp)> {
    let current = steps
        .iter()
        .position(|s| s.done_event_id.is_none())
        .unwrap_or(steps.len().saturating_sub(1));
    let steps_so_far = steps.get(..=current)?;
    let i = steps_so_far
        .iter()
        .rposition(|s| s.temp_min_c.is_some() || s.temp_max_c.is_some())?;
    Some((
        (steps[i].temp_min_c, steps[i].temp_max_c),
        step_start(steps, i, pitched_at)?,
    ))
}

type Band = (Option<f64>, Option<f64>);

/// Each end from the first layer that sets it — schedule step, the batch's own, the
/// yeast's. Where that crosses, the last layer is dropped, then the next: the more
/// specific word wins.
fn resolve_band(layers: &[Band]) -> Band {
    (1..=layers.len())
        .rev()
        .map(|n| {
            let top = &layers[..n];
            (top.iter().find_map(|l| l.0), top.iter().find_map(|l| l.1))
        })
        .find(|band| !band.0.zip(band.1).is_some_and(|(lo, hi)| lo > hi))
        .unwrap_or_default()
}

// ------------------------------------------------------------------ alerts

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AlertRow {
    pub id: i64,
    pub batch_id: i64,
    pub code: String,
    pub rule: Rule,
    pub device: Option<String>,
    pub raised_at: Timestamp,
    pub detail: String,
    pub cleared_at: Option<Timestamp>,
    pub cleared_note: Option<String>,
    pub acked_at: Option<Timestamp>,
    pub snoozed_until: Option<Timestamp>,
    pub notified_at: Option<Timestamp>,
}

/// Newest first. `open_only` leaves out the cleared.
pub fn list_alerts(
    conn: &Connection,
    batch_id: Option<i64>,
    open_only: bool,
) -> Result<Vec<AlertRow>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.batch_id, b.code, a.rule, a.device, a.raised_at, a.detail, \
         a.cleared_at, a.cleared_note, a.acked_at, a.snoozed_until, a.notified_at \
         FROM alert a JOIN batch b ON b.id = a.batch_id \
         WHERE (?1 IS NULL OR a.batch_id = ?1) AND (?2 = 0 OR a.cleared_at IS NULL) \
         ORDER BY a.raised_at DESC, a.id DESC",
    )?;
    let rows = stmt.query_map(params![batch_id, open_only], |row| {
        Ok(AlertRow {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            code: row.get(2)?,
            rule: row.get(3)?,
            device: row.get(4)?,
            raised_at: row.get(5)?,
            detail: row.get(6)?,
            cleared_at: row.get(7)?,
            cleared_note: row.get(8)?,
            acked_at: row.get(9)?,
            snoozed_until: row.get(10)?,
            notified_at: row.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Folds log records into `alert`, in order, and moves the offset past them in the
/// same transaction — a crash between the two would re-apply or skip. Every write is
/// idempotent, so re-applying after a truncation changes nothing. A record for a batch
/// this database doesn't have — another app's `brewd`, a demo database — is skipped.
pub fn apply_records(
    conn: &mut Connection,
    base_url: &str,
    records: &[LogRecord],
    offset: u64,
    now: Timestamp,
) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut applied = 0;
    for r in records {
        let id = i64::try_from(r.alert_id).map_err(|_| Error::InvalidAlertLog {
            detail: format!("alert id {} is out of range", r.alert_id),
        })?;
        let detail = (!r.detail.is_empty()).then_some(r.detail.as_str());
        applied += match r.event {
            LogEvent::Raised => tx.execute(
                "INSERT INTO alert (id, batch_id, rule, device, raised_at, detail) \
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM batch WHERE id = ?2) \
                 ON CONFLICT(id) DO NOTHING",
                params![id, r.batch_id, r.rule, r.device, r.time, r.detail],
            )?,
            LogEvent::Cleared => tx.execute(
                "UPDATE alert SET cleared_at = ?2, cleared_note = ?3 WHERE id = ?1",
                params![id, r.time, detail],
            )?,
            LogEvent::Acked => tx.execute(
                "UPDATE alert SET acked_at = ?2, snoozed_until = NULL WHERE id = ?1",
                params![id, r.time],
            )?,
            LogEvent::Snoozed => tx.execute(
                "UPDATE alert SET snoozed_until = ?2 WHERE id = ?1",
                params![id, r.until],
            )?,
            LogEvent::Notified => tx.execute(
                "UPDATE alert SET notified_at = ?2 WHERE id = ?1",
                params![id, r.time],
            )?,
        };
    }
    set_alert_offset(&tx, base_url, offset, now)?;
    tx.commit()?;
    Ok(applied)
}

/// How far the alert log of the `brewd` at `base_url` has been read.
pub fn alert_offset(conn: &Connection, base_url: &str) -> Result<u64> {
    let offset: Option<i64> = conn
        .query_row(
            "SELECT byte_offset FROM alert_sync WHERE base_url = ?",
            [base_url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(offset.map_or(0, |o| o.max(0) as u64))
}

pub fn set_alert_offset(
    conn: &Connection,
    base_url: &str,
    offset: u64,
    now: Timestamp,
) -> Result<()> {
    conn.execute(
        "INSERT INTO alert_sync (base_url, byte_offset, synced_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(base_url) DO UPDATE SET byte_offset = ?2, synced_at = ?3",
        params![base_url, i64::try_from(offset).unwrap_or(i64::MAX), now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::IspindelUnit,
        db::{
            batch::{
                NewBatch, NewBatchIngredient, add_batch_ingredient, archive_batch, create_batch,
                transition_batch,
            },
            recipe::{BeverageType, Stage},
            reference::{
                Category, NewIngredient, NewLot, NewYeastStrain, create_ingredient, create_lot,
                create_yeast_strain,
            },
            tier1::{create_source, create_stream},
            timeline::{bind_stream, record_pitch},
        },
        units::{Celsius, Quantity, Unit},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const BASE: &str = "http://brewd.local:8080";

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection, code: &str, og: Option<f64>) -> i64 {
        create_batch(
            conn,
            &NewBatch {
                code: code.into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: og.map(Sg::new_unchecked),
                notes: None,
            },
            T0,
        )
        .unwrap()
    }

    fn pitched(conn: &Connection, code: &str) -> i64 {
        let id = batch(conn, code, Some(1.040));
        record_pitch(conn, id, T0, None).unwrap();
        id
    }

    /// A journal source with its `sync_state`, and a stream per `(column, metric)`.
    fn journal(conn: &mut Connection, journal: &str, columns: &[(&str, Metric)]) -> Vec<i64> {
        let source = create_source(conn, SourceKind::BrewdJournal, journal, None).unwrap();
        conn.execute(
            "INSERT INTO sync_state (source_id, remote_path, byte_offset, synced_at) \
             VALUES (?, ?, 0, 0)",
            params![source, format!("{JOURNALS_PATH}{journal}")],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let ids = columns
            .iter()
            .map(|(column, metric)| create_stream(&tx, source, column, *metric).unwrap())
            .collect();
        tx.commit().unwrap();
        ids
    }

    fn bind(conn: &Connection, batch_id: i64, streams: &[i64]) {
        for &s in streams {
            bind_stream(conn, batch_id, s, T0, None).unwrap();
        }
    }

    fn config(unit: Option<IspindelUnit>) -> Config {
        let mut config = Config::default();
        config.gravity.ispindel_unit = unit;
        config
    }

    fn yeast(conn: &Connection, batch_id: i64, temp: (f64, f64)) {
        let strain = create_yeast_strain(
            conn,
            &NewYeastStrain {
                name: format!("Y{}", temp.0),
                temp_min_c: Some(Celsius::new_unchecked(temp.0)),
                temp_max_c: Some(Celsius::new_unchecked(temp.1)),
                ..Default::default()
            },
        )
        .unwrap();
        let ingredient = create_ingredient(
            conn,
            &NewIngredient {
                name: format!("Y{}", temp.0),
                category: Category::Yeast,
                default_unit: Unit::G,
                yeast_strain_id: Some(strain),
                attrs: None,
                notes: None,
            },
        )
        .unwrap();
        let lot = create_lot(
            conn,
            &NewLot {
                ingredient_id: ingredient,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(100.0, Unit::G).unwrap(),
                cost: None,
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        add_batch_ingredient(
            conn,
            batch_id,
            &NewBatchIngredient {
                ingredient_lot_id: lot,
                entry: Quantity::new(5.0, Unit::G).unwrap(),
                stage: Stage::Pitch,
                added_at: None,
                notes: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn a_pitched_batch_is_watched_through_its_journal_feeds() {
        let mut conn = db();
        let id = pitched(&conn, "SELTZER-001");
        let ispindel = journal(
            &mut conn,
            "ispindel-1.csv",
            &[
                ("gravity", Metric::Sg),
                ("temperature", Metric::GravityTempC),
            ],
        );
        let probe = journal(&mut conn, "esp32-1.csv", &[("vessel_temp", Metric::TempC)]);
        bind(&conn, id, &ispindel);
        bind(&conn, id, &probe);

        let watches = build_watches(&conn, &config(Some(IspindelUnit::Sg))).unwrap();
        assert_eq!(watches.len(), 1);
        let w = &watches[0];
        assert_eq!(w.code, "SELTZER-001");
        let g = w.gravity.as_ref().unwrap();
        assert_eq!(
            (g.device.as_str(), g.column.as_str(), g.since),
            ("ispindel-1", "gravity", T0)
        );
        let t = w.temp.as_ref().unwrap();
        assert_eq!(
            (t.device.as_str(), t.column.as_str()),
            ("esp32-1", "vessel_temp")
        );
        assert_eq!(w.devices, ["esp32-1", "ispindel-1"]);
        assert_eq!(w.og, Some(Sg::new_unchecked(1.040)));
        assert_eq!((w.temp_min_c, w.temp_max_c), (None, None));

        let no_unit = build_watches(&conn, &config(None)).unwrap();
        assert!(no_unit[0].gravity.is_none(), "no unit, no gravity rule");
    }

    #[test]
    fn unpitched_archived_packaged_muted_and_unfed_batches_are_not_watched() {
        let mut conn = db();
        let probe = journal(&mut conn, "esp32-1.csv", &[("vessel_temp", Metric::TempC)]);
        batch(&conn, "SELTZER-001", None);
        let archived = pitched(&conn, "SELTZER-002");
        bind(&conn, archived, &probe);
        archive_batch(&conn, archived, T0).unwrap();
        let muted = pitched(&conn, "SELTZER-003");
        set_batch_alert(
            &conn,
            muted,
            &BatchAlert {
                muted: true,
                ..Default::default()
            },
        )
        .unwrap();
        pitched(&conn, "SELTZER-004");
        let packaged = pitched(&conn, "SELTZER-005");
        transition_batch(&conn, packaged, BatchStatus::Conditioning, T0).unwrap();
        transition_batch(&conn, packaged, BatchStatus::Packaged, T0).unwrap();

        assert!(build_watches(&conn, &config(None)).unwrap().is_empty());
    }

    #[test]
    fn the_band_is_the_yeasts_overlap_unless_the_batch_sets_its_own() {
        let mut conn = db();
        let id = pitched(&conn, "SELTZER-001");
        let probe = journal(&mut conn, "esp32-1.csv", &[("vessel_temp", Metric::TempC)]);
        bind(&conn, id, &probe);
        yeast(&conn, id, (10.0, 25.0));
        yeast(&conn, id, (15.0, 30.0));

        let view = get_batch_alert(&conn, id).unwrap();
        assert_eq!(
            (view.yeast_min_c, view.yeast_max_c),
            (Some(15.0), Some(25.0))
        );
        let w = &build_watches(&conn, &config(None)).unwrap()[0];
        assert_eq!((w.temp_min_c, w.temp_max_c), (Some(15.0), Some(25.0)));

        let own = BatchAlert {
            temp_max_c: Some(19.0),
            target_sg: Some(Sg::new_unchecked(1.005)),
            ..Default::default()
        };
        set_batch_alert(&conn, id, &own).unwrap();
        assert_eq!(get_batch_alert(&conn, id).unwrap().settings, own);
        let w = &build_watches(&conn, &config(None)).unwrap()[0];
        assert_eq!((w.temp_min_c, w.temp_max_c), (Some(15.0), Some(19.0)));
        assert_eq!(w.target_sg, Some(Sg::new_unchecked(1.005)));

        // 26 against the yeast's 25 would cross: the batch's own end stands alone.
        let crossing = BatchAlert {
            temp_min_c: Some(26.0),
            ..Default::default()
        };
        set_batch_alert(&conn, id, &crossing).unwrap();
        let w = &build_watches(&conn, &config(None)).unwrap()[0];
        assert_eq!((w.temp_min_c, w.temp_max_c), (Some(26.0), None));

        let inverted = BatchAlert {
            temp_min_c: Some(20.0),
            temp_max_c: Some(18.0),
            ..Default::default()
        };
        let err = set_batch_alert(&conn, id, &inverted).unwrap_err();
        assert_eq!(err.code(), "units.range_inverted");
    }

    #[test]
    fn the_current_schedule_steps_band_comes_first() {
        use crate::db::schedule::{NewStep, add_step, complete_step};
        let mut conn = db();
        let id = pitched(&conn, "LAGER-001");
        let probe = journal(&mut conn, "esp32-1.csv", &[("vessel_temp", Metric::TempC)]);
        bind(&conn, id, &probe);
        yeast(&conn, id, (9.0, 15.0));
        let step = |name: &str, band: Band| NewStep {
            name: name.into(),
            temp_min_c: band.0,
            temp_max_c: band.1,
            ..NewStep::default()
        };
        let primary = add_step(&conn, id, &step("Primary", (None, Some(12.0)))).unwrap();
        let rest = add_step(&conn, id, &step("Diacetyl rest", (Some(16.0), Some(18.0)))).unwrap();
        let taste = add_step(&conn, id, &step("Taste", (None, None))).unwrap();
        let watch = |conn: &Connection| {
            let w = build_watches(conn, &config(None)).unwrap().remove(0);
            (w.temp_min_c, w.temp_max_c, w.band_since)
        };

        assert_eq!(
            watch(&conn),
            (Some(9.0), Some(12.0), Some(T0)),
            "the yeast fills the min"
        );
        let rested = T0.saturating_add_secs(72 * 3_600);
        complete_step(&conn, primary, rested, None).unwrap();
        // 16 against the yeast's 15 would cross: the step stands alone.
        assert_eq!(watch(&conn), (Some(16.0), Some(18.0), Some(rested)));
        complete_step(&conn, rest, rested.saturating_add_secs(3_600), None).unwrap();
        assert_eq!(
            watch(&conn),
            (Some(16.0), Some(18.0), Some(rested)),
            "kept through a step with none"
        );
        complete_step(&conn, taste, rested.saturating_add_secs(7_200), None).unwrap();
        assert_eq!(
            watch(&conn),
            (Some(16.0), Some(18.0), Some(rested)),
            "and once all are done"
        );
    }

    #[test]
    fn a_batch_with_no_schedule_has_no_band_since() {
        let mut conn = db();
        let id = pitched(&conn, "SELTZER-001");
        let probe = journal(&mut conn, "esp32-1.csv", &[("vessel_temp", Metric::TempC)]);
        bind(&conn, id, &probe);
        yeast(&conn, id, (10.0, 25.0));
        let w = &build_watches(&conn, &config(None)).unwrap()[0];
        assert_eq!(
            (w.temp_min_c, w.temp_max_c, w.band_since),
            (Some(10.0), Some(25.0), None)
        );
    }

    fn record(id: u64, batch_id: i64, event: LogEvent, time: i64) -> LogRecord {
        LogRecord {
            time: Timestamp::from_secs(time),
            alert_id: id,
            batch_id,
            rule: Rule::TempHigh,
            device: None,
            event,
            until: (event == LogEvent::Snoozed).then(|| Timestamp::from_secs(time + 3_600)),
            detail: if event == LogEvent::Raised {
                "hot".into()
            } else {
                String::new()
            },
        }
    }

    #[test]
    fn the_log_folds_into_alerts_idempotently_and_skips_unknown_batches() {
        let mut conn = db();
        let id = pitched(&conn, "SELTZER-001");
        let records = [
            record(10, id, LogEvent::Raised, 100),
            record(10, id, LogEvent::Notified, 101),
            record(10, id, LogEvent::Snoozed, 102),
            record(11, 999, LogEvent::Raised, 103),
            record(12, id, LogEvent::Raised, 104),
            record(12, id, LogEvent::Acked, 105),
            record(10, id, LogEvent::Cleared, 106),
        ];
        apply_records(&mut conn, BASE, &records, 500, T0).unwrap();
        apply_records(&mut conn, BASE, &records, 500, T0).unwrap();
        assert_eq!(alert_offset(&conn, BASE).unwrap(), 500);
        assert_eq!(alert_offset(&conn, "http://other").unwrap(), 0);

        let all = list_alerts(&conn, Some(id), false).unwrap();
        assert_eq!(all.len(), 2);
        let ten = all.iter().find(|a| a.id == 10).unwrap();
        assert_eq!(ten.detail, "hot");
        assert_eq!(ten.notified_at, Some(Timestamp::from_secs(101)));
        assert_eq!(ten.snoozed_until, Some(Timestamp::from_secs(102 + 3_600)));
        assert_eq!(ten.cleared_at, Some(Timestamp::from_secs(106)));

        let open = list_alerts(&conn, None, true).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!((open[0].id, open[0].code.as_str()), (12, "SELTZER-001"));
        assert_eq!(open[0].acked_at, Some(Timestamp::from_secs(105)));
    }
}
