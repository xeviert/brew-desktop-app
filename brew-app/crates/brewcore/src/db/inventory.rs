//! Phase 7c. Ingredient inventory: what is left in each lot. See `0017_lot_adjustment.sql`.
//!
//! **Remaining is derived, never stored**: the lot's amount, less what batches drew from
//! it, less adjustments. The same rule `update_lot` relies on for cost — a corrected
//! addition or receipt corrects the stock with it, and there is no running balance to
//! drift.
//!
//! **Over-drawing is allowed.** A batch addition records what went in, and the books are
//! the less trustworthy of the two, so a lot can read below zero; the UI says "over" and a
//! stocktake squares it. Archiving stays how a lot leaves the pickers.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::reference::{CanonicalQty, canonical_for_ingredient, canonical_from_columns};
use crate::{
    Error, Result,
    units::{Currency, Money, Quantity, Timestamp, Unit},
};

/// A lot's books, in its own entry unit so "2.5 kg" reads back as kilograms.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct LotStock {
    pub lot_id: i64,
    pub bought: Quantity,
    /// Drawn into batches.
    pub used: Quantity,
    /// Net of adjustments: positive removed, negative found.
    pub adjusted: Quantity,
    /// Below zero when the books say more went out than came in.
    pub remaining: Quantity,
    /// What the remainder cost, at the lot's price. `None` unpriced or empty.
    pub value: Option<Money>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewAdjustment {
    /// Positive removes, negative puts back.
    pub entry: Quantity,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AdjustmentRow {
    pub id: i64,
    pub ingredient_lot_id: i64,
    pub adjusted_at: Timestamp,
    pub qty: CanonicalQty,
    pub entry: Quantity,
    pub reason: String,
}

/// A lot's books and the adjustments behind them, newest first.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LotLedger {
    pub stock: LotStock,
    pub adjustments: Vec<AdjustmentRow>,
}

/// One ingredient's stock across its active lots, in its `default_unit`. Lots below
/// zero count as empty: a short lot does not make a full one less full.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct IngredientStock {
    pub ingredient_id: i64,
    pub on_hand: Quantity,
    pub lots: usize,
    /// `None` when nothing priced is left, or the lots are in more than one currency.
    pub value: Option<Money>,
}

/// Everything a stock figure is made from, per lot.
struct Books {
    ingredient_id: i64,
    bought: CanonicalQty,
    entry_unit: Unit,
    cost: Option<Money>,
    archived: bool,
    used: f64,
    adjusted: f64,
}

impl Books {
    fn remaining(&self) -> f64 {
        self.bought.amount() - self.used - self.adjusted
    }

    fn stock(&self, lot_id: i64) -> Result<LotStock> {
        let unit = self.entry_unit;
        let q = |canonical: f64| Quantity::new(canonical / unit.factor(), unit);
        let remaining = self.remaining();
        let value = match self.cost {
            Some(cost) if remaining > 0.0 && self.bought.amount() > 0.0 => {
                Some(cost.checked_mul_f64(remaining / self.bought.amount())?)
            }
            _ => None,
        };
        Ok(LotStock {
            lot_id,
            bought: q(self.bought.amount())?,
            used: q(self.used)?,
            adjusted: q(self.adjusted)?,
            remaining: q(remaining)?,
            value,
        })
    }
}

/// The books of every lot, or of one. One pass over each table rather than a query per
/// lot: the ingredients screen asks for all of them at once.
fn books(conn: &Connection, lot_id: Option<i64>) -> Result<HashMap<i64, Books>> {
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT id, ingredient_id, qty_g, qty_l, qty_each, entry_unit, total_cost_minor, \
                currency, archived_at IS NOT NULL \
         FROM ingredient_lot WHERE ?1 IS NULL OR id = ?1",
    )?;
    let rows = stmt.query_map([lot_id], |row| {
        let cost = row
            .get::<_, Option<i64>>(6)?
            .zip(row.get::<_, Option<Currency>>(7)?)
            .map(|(minor, currency)| Money::new(minor, currency));
        Ok((
            row.get::<_, i64>(0)?,
            Books {
                ingredient_id: row.get(1)?,
                bought: canonical_from_columns(2, row.get(2)?, row.get(3)?, row.get(4)?)?,
                entry_unit: row.get(5)?,
                cost,
                archived: row.get(8)?,
                used: 0.0,
                adjusted: 0.0,
            },
        ))
    })?;
    for row in rows {
        let (id, b) = row?;
        out.insert(id, b);
    }

    // `COALESCE` across the three columns: exactly one is set per row, and a lot's
    // additions and adjustments share its dimension, so the sum is in one unit.
    let sums = [
        "SELECT ingredient_lot_id, SUM(COALESCE(qty_g, qty_l, qty_each)) FROM batch_ingredient \
         WHERE ?1 IS NULL OR ingredient_lot_id = ?1 GROUP BY ingredient_lot_id",
        "SELECT ingredient_lot_id, SUM(COALESCE(qty_g, qty_l, qty_each)) FROM lot_adjustment \
         WHERE ?1 IS NULL OR ingredient_lot_id = ?1 GROUP BY ingredient_lot_id",
    ];
    for (i, sql) in sums.iter().enumerate() {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([lot_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?;
        for row in rows {
            let (id, sum) = row?;
            if let Some(b) = out.get_mut(&id) {
                if i == 0 {
                    b.used = sum;
                } else {
                    b.adjusted = sum;
                }
            }
        }
    }
    Ok(out)
}

pub fn lot_stock(conn: &Connection, lot_id: i64) -> Result<LotStock> {
    books(conn, Some(lot_id))?
        .get(&lot_id)
        .ok_or(Error::NoSuchLot { lot_id })?
        .stock(lot_id)
}

/// Every lot's stock, archived ones included, by lot id.
pub fn all_lot_stock(conn: &Connection) -> Result<HashMap<i64, LotStock>> {
    books(conn, None)?
        .iter()
        .map(|(id, b)| Ok((*id, b.stock(*id)?)))
        .collect()
}

/// On hand per ingredient, over active lots, in each ingredient's `default_unit`. An
/// ingredient with no active lot is absent.
pub fn ingredient_stock(conn: &Connection) -> Result<HashMap<i64, IngredientStock>> {
    let mut units = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, default_unit FROM ingredient")?;
    for row in stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Unit>(1)?))
    })? {
        let (id, unit) = row?;
        units.insert(id, unit);
    }

    struct Acc {
        canonical: f64,
        lots: usize,
        value: Option<Money>,
        mixed: bool,
    }
    let mut acc: HashMap<i64, Acc> = HashMap::new();
    for (id, b) in books(conn, None)? {
        if b.archived {
            continue;
        }
        let left = b.remaining().max(0.0);
        let value = b.stock(id)?.value;
        let a = acc.entry(b.ingredient_id).or_insert(Acc {
            canonical: 0.0,
            lots: 0,
            value: None,
            mixed: false,
        });
        a.canonical += left;
        a.lots += 1;
        if let Some(v) = value {
            match a.value {
                None if !a.mixed => a.value = Some(v),
                Some(total) => match total.checked_add(v) {
                    Ok(sum) => a.value = Some(sum),
                    Err(Error::CurrencyMismatch { .. }) => {
                        a.value = None;
                        a.mixed = true;
                    }
                    Err(e) => return Err(e),
                },
                None => {}
            }
        }
    }

    acc.into_iter()
        .filter_map(|(ingredient_id, a)| {
            let unit = *units.get(&ingredient_id)?;
            Some((ingredient_id, a, unit))
        })
        .map(|(ingredient_id, a, unit)| {
            Ok((
                ingredient_id,
                IngredientStock {
                    ingredient_id,
                    on_hand: Quantity::new(a.canonical / unit.factor(), unit)?,
                    lots: a.lots,
                    value: a.value,
                },
            ))
        })
        .collect()
}

// --------------------------------------------------------- lot_adjustment

fn lot_for_write(conn: &Connection, lot_id: i64) -> Result<i64> {
    let (ingredient_id, archived): (i64, bool) = conn
        .query_row(
            "SELECT ingredient_id, archived_at IS NOT NULL FROM ingredient_lot WHERE id = ?",
            [lot_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NoSuchLot { lot_id })?;
    if archived {
        return Err(Error::LotArchived { lot_id });
    }
    Ok(ingredient_id)
}

fn adjustment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AdjustmentRow> {
    Ok(AdjustmentRow {
        id: row.get(0)?,
        ingredient_lot_id: row.get(1)?,
        adjusted_at: row.get(2)?,
        qty: canonical_from_columns(3, row.get(3)?, row.get(4)?, row.get(5)?)?,
        entry: Quantity {
            value: row.get(6)?,
            unit: row.get(7)?,
        },
        reason: row.get(8)?,
    })
}

const ADJUSTMENT_COLUMNS: &str =
    "id, ingredient_lot_id, adjusted_at, qty_g, qty_l, qty_each, entry_qty, entry_unit, reason";

/// Newest first.
pub fn list_adjustments(conn: &Connection, lot_id: i64) -> Result<Vec<AdjustmentRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ADJUSTMENT_COLUMNS} FROM lot_adjustment WHERE ingredient_lot_id = ? \
         ORDER BY adjusted_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([lot_id], adjustment_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn lot_ledger(conn: &Connection, lot_id: i64) -> Result<LotLedger> {
    Ok(LotLedger {
        stock: lot_stock(conn, lot_id)?,
        adjustments: list_adjustments(conn, lot_id)?,
    })
}

/// Take stock out of a lot (or, negative, put it back). Not into a batch: that is an
/// addition, and costs the batch.
pub fn add_adjustment(
    conn: &Connection,
    lot_id: i64,
    at: Timestamp,
    adjustment: &NewAdjustment,
) -> Result<i64> {
    let ingredient_id = lot_for_write(conn, lot_id)?;
    let reason = adjustment.reason.trim();
    if reason.is_empty() {
        return Err(Error::EntryEmpty);
    }
    if adjustment.entry.value == 0.0 {
        return Err(Error::NotPositive {
            field: "quantity",
            value: 0.0,
        });
    }
    let (qty_g, qty_l, qty_each) = canonical_for_ingredient(conn, ingredient_id, adjustment.entry)?;
    conn.execute(
        "INSERT INTO lot_adjustment \
         (ingredient_lot_id, adjusted_at, qty_g, qty_l, qty_each, entry_qty, entry_unit, reason) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            lot_id,
            at,
            qty_g,
            qty_l,
            qty_each,
            adjustment.entry.value,
            adjustment.entry.unit,
            reason
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// A stocktake: what is actually left, written as the adjustment that makes the books
/// agree. `None` when they already do.
pub fn set_remaining(
    conn: &Connection,
    lot_id: i64,
    at: Timestamp,
    counted: Quantity,
    reason: Option<&str>,
) -> Result<Option<i64>> {
    if counted.value < 0.0 {
        return Err(Error::NotPositive {
            field: "quantity",
            value: counted.value,
        });
    }
    let ingredient_id = lot_for_write(conn, lot_id)?;
    // Through the ingredient, so a count in the wrong dimension is refused as an
    // adjustment would be.
    let (g, l, each) = canonical_for_ingredient(conn, ingredient_id, counted)?;
    let counted_canonical = g
        .map(|g| g.get())
        .or(l.map(|l| l.get()))
        .or(each)
        .unwrap_or(0.0);
    let books = books(conn, Some(lot_id))?;
    let remaining = books
        .get(&lot_id)
        .ok_or(Error::NoSuchLot { lot_id })?
        .remaining();

    let delta = remaining - counted_canonical;
    // Below a thousandth of the entry unit is float noise, not a correction.
    if delta.abs() < counted.unit.factor() * 1e-3 {
        return Ok(None);
    }
    let reason = reason
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("stocktake");
    add_adjustment(
        conn,
        lot_id,
        at,
        &NewAdjustment {
            entry: Quantity::new(delta / counted.unit.factor(), counted.unit)?,
            reason: reason.to_owned(),
        },
    )
    .map(Some)
}

pub fn get_adjustment(conn: &Connection, adjustment_id: i64) -> Result<AdjustmentRow> {
    conn.query_row(
        &format!("SELECT {ADJUSTMENT_COLUMNS} FROM lot_adjustment WHERE id = ?"),
        [adjustment_id],
        adjustment_from_row,
    )
    .optional()?
    .ok_or(Error::NoSuchAdjustment { adjustment_id })
}

/// A mistyped adjustment records nothing, so this is a real delete.
pub fn delete_adjustment(conn: &Connection, adjustment_id: i64) -> Result<i64> {
    let lot_id = get_adjustment(conn, adjustment_id)?.ingredient_lot_id;
    lot_for_write(conn, lot_id)?;
    conn.execute("DELETE FROM lot_adjustment WHERE id = ?", [adjustment_id])?;
    Ok(lot_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        batch::{NewBatch, NewBatchIngredient, add_batch_ingredient, create_batch},
        recipe::{BeverageType, Stage},
        reference::{Category, NewIngredient, NewLot, archive_lot, create_ingredient, create_lot},
    };
    use crate::units::{Currency, Unit};

    const T0: Timestamp = Timestamp::from_secs(1_700_000_000);

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn sugar(conn: &Connection) -> i64 {
        create_ingredient(
            conn,
            &NewIngredient {
                name: "Sucrose".into(),
                category: Category::Sugar,
                default_unit: Unit::Kg,
                yeast_strain_id: None,
                attrs: None,
                notes: None,
            },
        )
        .unwrap()
    }

    fn lot(conn: &Connection, ingredient: i64, kg: f64, cost_minor: Option<i64>) -> i64 {
        create_lot(
            conn,
            &NewLot {
                ingredient_id: ingredient,
                supplier: None,
                lot_code: None,
                entry: Quantity::new(kg, Unit::Kg).unwrap(),
                cost: cost_minor.map(|m| Money::new(m, Currency::EUR)),
                purchased_at: None,
                expires_at: None,
                notes: None,
            },
        )
        .unwrap()
    }

    fn draw(conn: &Connection, lot: i64, value: f64, unit: Unit) {
        let batch = create_batch(
            conn,
            &NewBatch {
                code: format!("B{lot}-{value}"),
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
        add_batch_ingredient(
            conn,
            batch,
            &NewBatchIngredient {
                ingredient_lot_id: lot,
                entry: Quantity::new(value, unit).unwrap(),
                stage: Stage::Prep,
                added_at: None,
                notes: None,
            },
        )
        .unwrap();
    }

    fn adjust(value: f64, unit: Unit, reason: &str) -> NewAdjustment {
        NewAdjustment {
            entry: Quantity::new(value, unit).unwrap(),
            reason: reason.into(),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn remaining_is_bought_less_batches_less_adjustments_in_the_lots_unit() {
        let conn = db();
        let s = sugar(&conn);
        let l = lot(&conn, s, 5.0, Some(1_250));
        draw(&conn, l, 4.0, Unit::Kg);
        add_adjustment(&conn, l, T0, &adjust(250.0, Unit::G, "spilt")).unwrap();

        let stock = lot_stock(&conn, l).unwrap();
        assert_eq!(stock.remaining.unit, Unit::Kg);
        assert!(close(stock.used.value, 4.0));
        assert!(close(stock.adjusted.value, 0.25));
        assert!(close(stock.remaining.value, 0.75));
        assert_eq!(
            stock.value,
            Some(Money::new(188, Currency::EUR)),
            "€12.50 × 0.75/5"
        );
    }

    #[test]
    fn a_lot_can_be_overdrawn_and_a_stocktake_squares_it() {
        let conn = db();
        let s = sugar(&conn);
        let l = lot(&conn, s, 1.0, None);
        draw(&conn, l, 1.2, Unit::Kg);
        let stock = lot_stock(&conn, l).unwrap();
        assert!(close(stock.remaining.value, -0.2));
        assert_eq!(stock.value, None);

        let id = set_remaining(&conn, l, T0, Quantity::new(0.0, Unit::G).unwrap(), None)
            .unwrap()
            .unwrap();
        let row = get_adjustment(&conn, id).unwrap();
        assert_eq!(row.reason, "stocktake");
        assert!(close(row.entry.value, -200.0), "typed in g, found 200 g");
        assert!(close(lot_stock(&conn, l).unwrap().remaining.value, 0.0));

        assert_eq!(
            set_remaining(&conn, l, T0, Quantity::new(0.0, Unit::Kg).unwrap(), None).unwrap(),
            None,
            "books already agree"
        );
    }

    #[test]
    fn an_adjustment_needs_a_reason_an_amount_and_an_open_lot() {
        let conn = db();
        let s = sugar(&conn);
        let l = lot(&conn, s, 1.0, None);
        let code = |a: NewAdjustment| add_adjustment(&conn, l, T0, &a).unwrap_err().code();
        assert_eq!(code(adjust(1.0, Unit::G, " ")), "db.entry_empty");
        assert_eq!(code(adjust(0.0, Unit::G, "x")), "units.not_positive");
        assert_eq!(code(adjust(1.0, Unit::L, "x")), "units.dimension_mismatch");

        let id = add_adjustment(&conn, l, T0, &adjust(100.0, Unit::G, "gift")).unwrap();
        archive_lot(&conn, l, T0).unwrap();
        assert_eq!(code(adjust(1.0, Unit::G, "x")), "db.lot_archived");
        assert_eq!(
            delete_adjustment(&conn, id).unwrap_err().code(),
            "db.lot_archived"
        );
        assert_eq!(
            get_adjustment(&conn, 99).unwrap_err().code(),
            "db.no_such_adjustment"
        );
    }

    #[test]
    fn on_hand_sums_active_lots_and_ignores_an_overdrawn_one() {
        let conn = db();
        let s = sugar(&conn);
        let a = lot(&conn, s, 5.0, Some(1_000));
        let b = lot(&conn, s, 1.0, Some(500));
        let c = lot(&conn, s, 2.0, Some(100));
        draw(&conn, a, 1.0, Unit::Kg);
        draw(&conn, b, 1.5, Unit::Kg);
        archive_lot(&conn, c, T0).unwrap();

        let stock = ingredient_stock(&conn).unwrap()[&s];
        assert!(close(stock.on_hand.value, 4.0));
        assert_eq!(stock.on_hand.unit, Unit::Kg);
        assert_eq!(stock.lots, 2);
        assert_eq!(stock.value, Some(Money::new(800, Currency::EUR)));

        let all = all_lot_stock(&conn).unwrap();
        assert_eq!(all.len(), 3);
        assert!(close(all[&c].remaining.value, 2.0));
    }

    #[test]
    fn deleting_an_adjustment_restores_the_stock() {
        let conn = db();
        let s = sugar(&conn);
        let l = lot(&conn, s, 1.0, None);
        let id = add_adjustment(&conn, l, T0, &adjust(0.5, Unit::Kg, "spilt")).unwrap();
        assert_eq!(lot_ledger(&conn, l).unwrap().adjustments.len(), 1);
        assert_eq!(delete_adjustment(&conn, id).unwrap(), l);
        let ledger = lot_ledger(&conn, l).unwrap();
        assert!(ledger.adjustments.is_empty());
        assert!(close(ledger.stock.remaining.value, 1.0));
    }
}
