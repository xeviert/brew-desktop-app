//! Phase 7a. Packaging records: what left the vessel, in what, and how much. See
//! `0015_packaging.sql`.
//!
//! **The time is an event.** Each line owns a `package` `batch_event`, written and
//! kept in step here, so the timeline shows every fill and stays the one record of
//! when. Deleting that event from the timeline takes the line with it.
//!
//! Loss is derived on read, from the batch's volume (or its recipe's target, as cost
//! uses) less what was packaged — never stored.
//!
//! Same contract as the rest of `db`: a borrowed connection, never a transaction of its
//! own except in [`write_packaging`].

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    batch::{
        BatchRow, BatchStatus, ensure_open, get_batch, target_or_batch_volume, transition_batch,
    },
    enums::db_enum,
    timeline::{EventKind, NewBatchEvent, add_event, delete_event, update_event},
};
use crate::{
    Error, Result, calc,
    units::{Litres, Money, Timestamp},
};

db_enum!(
    /// What a line went into. Not a `CHECK` in the schema — see `0015_packaging.sql`.
    PackageFormat,
    UnknownPackageFormat,
    (Bottle, "bottle"),
    (Can, "can"),
    (Keg, "keg"),
    (Other, "other"),
);

/// One line as entered. The time is passed beside it: [`package_batch`] gives every
/// line the move's own time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewPackaging {
    pub format: PackageFormat,
    pub container_size_l: Option<Litres>,
    pub container_count: Option<i64>,
    /// `None` is size × count.
    pub volume_l: Option<Litres>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PackagingRow {
    pub id: i64,
    pub batch_id: i64,
    pub event_id: i64,
    /// The event's time.
    pub at: Timestamp,
    pub format: PackageFormat,
    pub container_size_l: Option<Litres>,
    pub container_count: Option<i64>,
    pub volume_l: Litres,
    pub notes: Option<String>,
}

/// A batch's lines and what they add up to.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatchPackaging {
    pub batch_id: i64,
    pub lines: Vec<PackagingRow>,
    pub pitched_at: Option<Timestamp>,
    /// The batch's own volume, or the recipe version's target when it has none.
    pub volume_l: Option<Litres>,
    pub packaged_l: Litres,
    /// Volume less packaged. `None` with no lines or no volume; negative when more
    /// was packaged than the batch says it held (a top-up, or a volume never updated).
    pub loss_l: Option<Litres>,
}

const COLUMNS: &str = "p.id, p.batch_id, p.event_id, e.ts, p.format, p.container_size_l, \
                       p.container_count, p.volume_l, p.notes";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PackagingRow> {
    Ok(PackagingRow {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        event_id: row.get(2)?,
        at: row.get(3)?,
        format: row.get(4)?,
        container_size_l: row.get(5)?,
        container_count: row.get(6)?,
        volume_l: row.get(7)?,
        notes: row.get(8)?,
    })
}

/// The volume to store: as given, else size × count. Positive either way.
fn volume(line: &NewPackaging) -> Result<Litres> {
    if let Some(size) = line.container_size_l {
        positive("container_size_l", size.get())?;
    }
    if let Some(count) = line.container_count {
        positive("container_count", count as f64)?;
    }
    let volume = match (line.volume_l, line.container_size_l, line.container_count) {
        (Some(v), _, _) => v,
        (None, Some(size), Some(count)) => Litres::new(size.get() * count as f64)?,
        _ => return Err(Error::PackagingNoVolume),
    };
    positive("volume_l", volume.get())?;
    Ok(volume)
}

fn positive(field: &'static str, value: f64) -> Result<()> {
    if !value.is_finite() {
        return Err(Error::NotFinite { field, value });
    }
    if value <= 0.0 {
        return Err(Error::NotPositive { field, value });
    }
    Ok(())
}

/// Open, pitched, and not before the pitch: a line is beer that left a vessel, which
/// takes beer that went into one.
fn check_time(batch: &BatchRow, at: Timestamp) -> Result<()> {
    ensure_open(batch)?;
    match batch.pitched_at {
        None => Err(Error::BatchNotPitched { batch_id: batch.id }),
        Some(pitched_at) if at < pitched_at => Err(Error::BeforePitch { batch_id: batch.id }),
        Some(_) => Ok(()),
    }
}

/// Litres as typed back, without a float's tail: `19`, `0.33`.
fn litres_text(v: f64) -> String {
    format!("{}", (v * 1000.0).round() / 1000.0)
}

/// The event's note: the line, readable on the timeline without opening Packaging.
fn event_note(line: &NewPackaging, volume: Litres) -> String {
    let mut text = format!("{}: ", line.format);
    if let (Some(size), Some(count)) = (line.container_size_l, line.container_count) {
        text.push_str(&format!("{count} × {} L, ", litres_text(size.get())));
    }
    text.push_str(&format!("{} L", litres_text(volume.get())));
    if let Some(notes) = trimmed(&line.notes) {
        text.push_str(&format!(" · {notes}"));
    }
    text
}

fn trimmed(notes: &Option<String>) -> Option<&str> {
    notes.as_deref().map(str::trim).filter(|n| !n.is_empty())
}

pub fn batch_packaging(conn: &Connection, batch_id: i64) -> Result<BatchPackaging> {
    let batch = get_batch(conn, batch_id)?;
    let lines = list_packaging(conn, batch_id)?;
    let packaged: f64 = lines.iter().map(|l| l.volume_l.get()).sum();
    let volume_l = target_or_batch_volume(conn, &batch)?;
    let loss_l = match volume_l {
        Some(v) if !lines.is_empty() => Some(Litres::new(v.get() - packaged)?),
        _ => None,
    };
    Ok(BatchPackaging {
        batch_id,
        lines,
        pitched_at: batch.pitched_at,
        volume_l,
        packaged_l: Litres::new(packaged)?,
        loss_l,
    })
}

/// What the packaged beer cost to make: the batch's cost shared out by volume, so the
/// loss is priced into what was packaged rather than vanishing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PackagingCost {
    /// `None` with no batch cost or nothing packaged.
    pub per_packaged_l: Option<Money>,
    pub lines: Vec<LineCost>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct LineCost {
    pub packaging_id: i64,
    pub cost: Option<Money>,
    /// `cost` over `container_count`.
    pub cost_each: Option<Money>,
}

/// Lots in more than one currency are [`Error::CurrencyMismatch`], as from
/// [`batch_cost`](super::batch::batch_cost).
pub fn packaging_cost(conn: &Connection, packaging: &BatchPackaging) -> Result<PackagingCost> {
    let total = super::batch::batch_cost(conn, packaging.batch_id)?
        .total
        .filter(|_| packaging.packaged_l.get() > 0.0);
    let packaged = packaging.packaged_l.get();
    let lines = packaging
        .lines
        .iter()
        .map(|line| {
            let share = line.volume_l.get() / packaged;
            let cost = total.map(|t| t.checked_mul_f64(share)).transpose()?;
            // From the total, not from `cost`: rounding once, not twice.
            let cost_each = match (total, line.container_count) {
                (Some(t), Some(n)) if n > 0 => Some(t.checked_mul_f64(share / n as f64)?),
                _ => None,
            };
            Ok(LineCost {
                packaging_id: line.id,
                cost,
                cost_each,
            })
        })
        .collect::<Result<_>>()?;
    Ok(PackagingCost {
        per_packaged_l: total
            .map(|t| calc::cost::per_litre(t, packaging.packaged_l))
            .transpose()?,
        lines,
    })
}

/// One write against a line in its own transaction, then the batch's packaging — what
/// every line command answers with.
pub fn write_packaging(
    conn: &mut Connection,
    packaging_id: i64,
    write: impl FnOnce(&Connection) -> Result<()>,
) -> Result<BatchPackaging> {
    let tx = conn.transaction()?;
    let batch_id = get_packaging(&tx, packaging_id)?.batch_id;
    write(&tx)?;
    let packaging = batch_packaging(&tx, batch_id)?;
    tx.commit()?;
    Ok(packaging)
}

/// Oldest first.
pub fn list_packaging(conn: &Connection, batch_id: i64) -> Result<Vec<PackagingRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM packaging_record p JOIN batch_event e ON e.id = p.event_id \
         WHERE p.batch_id = ? ORDER BY e.ts, p.id"
    ))?;
    let rows = stmt.query_map([batch_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_packaging(conn: &Connection, packaging_id: i64) -> Result<PackagingRow> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM packaging_record p \
             JOIN batch_event e ON e.id = p.event_id WHERE p.id = ?"
        ),
        [packaging_id],
        from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchPackaging { packaging_id })
}

/// Whether a line hangs on this event. [`update_event`] asks before letting its kind
/// change.
pub fn owns_event(conn: &Connection, event_id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM packaging_record WHERE event_id = ?",
            [event_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Writes the `package` event and the line. Returns the line's id.
pub fn add_packaging(
    conn: &Connection,
    batch_id: i64,
    at: Timestamp,
    line: &NewPackaging,
) -> Result<i64> {
    check_time(&get_batch(conn, batch_id)?, at)?;
    let volume = volume(line)?;
    let event_id = add_event(
        conn,
        batch_id,
        &NewBatchEvent {
            ts: at,
            kind: EventKind::Package,
            payload: None,
            note: Some(event_note(line, volume)),
        },
    )?;
    conn.execute(
        "INSERT INTO packaging_record \
         (batch_id, event_id, format, container_size_l, container_count, volume_l, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            batch_id,
            event_id,
            line.format,
            line.container_size_l,
            line.container_count,
            volume,
            trimmed(&line.notes),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The line and its event's time and note, together.
pub fn update_packaging(
    conn: &Connection,
    packaging_id: i64,
    at: Timestamp,
    line: &NewPackaging,
) -> Result<()> {
    let current = get_packaging(conn, packaging_id)?;
    check_time(&get_batch(conn, current.batch_id)?, at)?;
    let volume = volume(line)?;
    update_event(
        conn,
        current.event_id,
        &NewBatchEvent {
            ts: at,
            kind: EventKind::Package,
            payload: None,
            note: Some(event_note(line, volume)),
        },
    )?;
    conn.execute(
        "UPDATE packaging_record SET format = ?, container_size_l = ?, container_count = ?, \
         volume_l = ?, notes = ? WHERE id = ?",
        params![
            line.format,
            line.container_size_l,
            line.container_count,
            volume,
            trimmed(&line.notes),
            packaging_id,
        ],
    )?;
    Ok(())
}

/// Through its event, so the line and its place on the timeline go together.
pub fn delete_packaging(conn: &Connection, packaging_id: i64) -> Result<()> {
    delete_event(conn, get_packaging(conn, packaging_id)?.event_id)?;
    Ok(())
}

/// The move to `packaged` and what was packaged, at one time. Lines may be empty: the
/// move alone is what [`transition_batch`] already did. A batch already packaged takes
/// more lines through [`add_packaging`].
pub fn package_batch(
    conn: &Connection,
    batch_id: i64,
    at: Timestamp,
    lines: &[NewPackaging],
) -> Result<bool> {
    let moved = transition_batch(conn, batch_id, BatchStatus::Packaged, at)?;
    for line in lines {
        add_packaging(conn, batch_id, at, line)?;
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, archive_batch, create_batch},
        recipe::BeverageType,
        timeline::{get_event, list_batch_events, record_pitch},
    };

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);
    const DAY: i64 = 86_400;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn batch(conn: &Connection, volume_l: Option<f64>) -> i64 {
        let id = create_batch(
            conn,
            &NewBatch {
                code: "B1".into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: volume_l.map(Litres::new_unchecked),
                og: None,
                notes: None,
            },
            T0,
        )
        .unwrap();
        record_pitch(conn, id, T0, None).unwrap();
        id
    }

    fn line(
        format: PackageFormat,
        size: Option<f64>,
        count: Option<i64>,
        volume: Option<f64>,
    ) -> NewPackaging {
        NewPackaging {
            format,
            container_size_l: size.map(Litres::new_unchecked),
            container_count: count,
            volume_l: volume.map(Litres::new_unchecked),
            notes: None,
        }
    }

    fn day(n: i64) -> Timestamp {
        T0.saturating_add_secs(n * DAY)
    }

    #[test]
    fn lines_add_up_and_the_rest_is_loss() {
        let conn = db();
        let id = batch(&conn, Some(20.0));
        add_packaging(
            &conn,
            id,
            day(14),
            &line(PackageFormat::Keg, Some(9.5), Some(1), None),
        )
        .unwrap();
        add_packaging(
            &conn,
            id,
            day(14),
            &line(PackageFormat::Bottle, Some(0.33), Some(24), Some(7.8)),
        )
        .unwrap();

        let p = batch_packaging(&conn, id).unwrap();
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[0].volume_l.get(), 9.5);
        assert_eq!(
            p.lines[1].volume_l.get(),
            7.8,
            "volume wins over size × count"
        );
        assert!((p.packaged_l.get() - 17.3).abs() < 1e-9);
        assert!((p.loss_l.unwrap().get() - 2.7).abs() < 1e-9);
    }

    #[test]
    fn the_batch_cost_is_shared_by_volume_packaged() {
        use crate::db::{
            batch::{NewBatchIngredient, add_batch_ingredient},
            recipe::Stage,
            reference::{Category, NewIngredient, NewLot, create_ingredient, create_lot},
        };
        use crate::units::{Currency, Quantity, Unit};

        let conn = db();
        let id = batch(&conn, Some(20.0));
        let sugar = create_ingredient(
            &conn,
            &NewIngredient {
                name: "Sucrose".into(),
                category: Category::Sugar,
                default_unit: Unit::Kg,
                yeast_strain_id: None,
                attrs: None,
                notes: None,
            },
        )
        .unwrap();
        let lot = create_lot(
            &conn,
            &NewLot {
                ingredient_id: sugar,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(5.0, Unit::Kg).unwrap(),
                cost: Some(Money::new(1_200, Currency::EUR)),
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap();
        add_batch_ingredient(
            &conn,
            id,
            &NewBatchIngredient {
                ingredient_lot_id: lot,
                entry: Quantity::new(5.0, Unit::Kg).unwrap(),
                stage: Stage::Fermentation,
                added_at: None,
                notes: None,
            },
        )
        .unwrap();

        let empty = batch_packaging(&conn, id).unwrap();
        assert_eq!(packaging_cost(&conn, &empty).unwrap().per_packaged_l, None);

        add_packaging(
            &conn,
            id,
            day(14),
            &line(PackageFormat::Keg, None, Some(1), Some(10.0)),
        )
        .unwrap();
        add_packaging(
            &conn,
            id,
            day(14),
            &line(PackageFormat::Bottle, Some(0.5), Some(10), None),
        )
        .unwrap();
        let cost = packaging_cost(&conn, &batch_packaging(&conn, id).unwrap()).unwrap();
        let eur = |minor| Some(Money::new(minor, Currency::EUR));
        assert_eq!(
            cost.per_packaged_l,
            eur(80),
            "€12 over 15 L, not the 20 L brewed"
        );
        assert_eq!(
            (cost.lines[0].cost, cost.lines[0].cost_each),
            (eur(800), eur(800))
        );
        assert_eq!(
            (cost.lines[1].cost, cost.lines[1].cost_each),
            (eur(400), eur(40))
        );
    }

    #[test]
    fn no_lines_or_no_volume_is_no_loss() {
        let conn = db();
        let id = batch(&conn, None);
        assert_eq!(batch_packaging(&conn, id).unwrap().loss_l, None);
        add_packaging(
            &conn,
            id,
            day(1),
            &line(PackageFormat::Keg, None, None, Some(19.0)),
        )
        .unwrap();
        assert_eq!(batch_packaging(&conn, id).unwrap().loss_l, None);
    }

    #[test]
    fn a_line_needs_a_volume_and_a_pitch_before_it() {
        let conn = db();
        let id = batch(&conn, Some(20.0));
        let code = |l: &NewPackaging, at| add_packaging(&conn, id, at, l).unwrap_err().code();

        assert_eq!(
            code(&line(PackageFormat::Keg, Some(19.0), None, None), day(1)),
            "db.packaging_no_volume"
        );
        assert_eq!(
            code(&line(PackageFormat::Keg, None, None, Some(0.0)), day(1)),
            "units.not_positive"
        );
        assert_eq!(
            code(&line(PackageFormat::Can, Some(0.5), Some(0), None), day(1)),
            "units.not_positive"
        );
        assert_eq!(
            code(
                &line(PackageFormat::Keg, None, None, Some(19.0)),
                T0.saturating_add_secs(-1)
            ),
            "db.before_pitch"
        );

        let unpitched = create_batch(
            &conn,
            &NewBatch {
                code: "B2".into(),
                recipe_version_id: None,
                beverage_type: BeverageType::Seltzer,
                vessel_id: None,
                volume_l: None,
                og: None,
                notes: None,
            },
            T0,
        )
        .unwrap();
        assert_eq!(
            add_packaging(
                &conn,
                unpitched,
                day(1),
                &line(PackageFormat::Keg, None, None, Some(1.0))
            )
            .unwrap_err()
            .code(),
            "db.batch_not_pitched"
        );
    }

    #[test]
    fn the_line_is_its_event_on_the_timeline() {
        let conn = db();
        let id = batch(&conn, Some(20.0));
        let mut bottles = line(PackageFormat::Bottle, Some(0.33), Some(24), None);
        bottles.notes = Some(" primed ".into());
        let p = add_packaging(&conn, id, day(14), &bottles).unwrap();

        let row = get_packaging(&conn, p).unwrap();
        assert_eq!(row.notes.as_deref(), Some("primed"));
        let event = get_event(&conn, row.event_id).unwrap();
        assert_eq!((event.kind, event.ts), (EventKind::Package, day(14)));
        assert_eq!(
            event.note.as_deref(),
            Some("bottle: 24 × 0.33 L, 7.92 L · primed")
        );

        update_packaging(
            &conn,
            p,
            day(15),
            &line(PackageFormat::Keg, None, None, Some(19.0)),
        )
        .unwrap();
        let event = get_event(&conn, row.event_id).unwrap();
        assert_eq!(
            (event.ts, event.note.as_deref()),
            (day(15), Some("keg: 19 L"))
        );
        assert_eq!(get_packaging(&conn, p).unwrap().at, day(15));

        let as_note = NewBatchEvent {
            ts: day(15),
            kind: EventKind::Note,
            payload: None,
            note: None,
        };
        assert_eq!(
            update_event(&conn, row.event_id, &as_note)
                .unwrap_err()
                .code(),
            "db.packaging_event_locked"
        );

        delete_event(&conn, row.event_id).unwrap();
        assert_eq!(
            get_packaging(&conn, p).unwrap_err().code(),
            "db.no_such_packaging"
        );
    }

    #[test]
    fn deleting_a_line_takes_its_event() {
        let conn = db();
        let id = batch(&conn, Some(20.0));
        let p = add_packaging(
            &conn,
            id,
            day(14),
            &line(PackageFormat::Keg, None, None, Some(19.0)),
        )
        .unwrap();
        delete_packaging(&conn, p).unwrap();
        assert!(batch_packaging(&conn, id).unwrap().lines.is_empty());
        assert!(
            list_batch_events(&conn, id)
                .unwrap()
                .iter()
                .all(|e| e.kind != EventKind::Package)
        );
    }

    #[test]
    fn packaging_a_batch_moves_it_and_records_its_lines_at_one_time() {
        let conn = db();
        let id = batch(&conn, Some(20.0));
        transition_batch(&conn, id, BatchStatus::Fermenting, day(0)).unwrap();
        transition_batch(&conn, id, BatchStatus::Conditioning, day(10)).unwrap();

        let lines = [
            line(PackageFormat::Keg, Some(9.5), Some(1), None),
            line(PackageFormat::Bottle, Some(0.5), Some(18), None),
        ];
        assert!(package_batch(&conn, id, day(14), &lines).unwrap());
        assert_eq!(get_batch(&conn, id).unwrap().status, BatchStatus::Packaged);
        let p = batch_packaging(&conn, id).unwrap();
        assert!(p.lines.iter().all(|l| l.at == day(14)));
        assert_eq!(p.packaged_l.get(), 18.5);

        // A late keg, after the move.
        add_packaging(
            &conn,
            id,
            day(20),
            &line(PackageFormat::Keg, None, None, Some(1.0)),
        )
        .unwrap();
        assert_eq!(batch_packaging(&conn, id).unwrap().lines.len(), 3);
    }

    #[test]
    fn a_refused_line_is_a_refused_move() {
        let mut conn = db();
        let id = batch(&conn, Some(20.0));
        transition_batch(&conn, id, BatchStatus::Fermenting, day(0)).unwrap();
        transition_batch(&conn, id, BatchStatus::Conditioning, day(10)).unwrap();

        let tx = conn.transaction().unwrap();
        let lines = [line(PackageFormat::Keg, None, None, None)];
        assert!(package_batch(&tx, id, day(14), &lines).is_err());
        drop(tx);
        assert_eq!(
            get_batch(&conn, id).unwrap().status,
            BatchStatus::Conditioning
        );
    }

    #[test]
    fn an_archived_batchs_packaging_is_read_only() {
        let conn = db();
        let id = batch(&conn, Some(20.0));
        let p = add_packaging(
            &conn,
            id,
            day(14),
            &line(PackageFormat::Keg, None, None, Some(19.0)),
        )
        .unwrap();
        archive_batch(&conn, id, day(30)).unwrap();
        let keg = line(PackageFormat::Keg, None, None, Some(1.0));
        assert!(add_packaging(&conn, id, day(20), &keg).is_err());
        assert!(update_packaging(&conn, p, day(20), &keg).is_err());
        assert!(delete_packaging(&conn, p).is_err());
        assert_eq!(batch_packaging(&conn, id).unwrap().lines.len(), 1);
    }
}
