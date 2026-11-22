-- Phase 7c: stock taken out of a lot other than into a batch — a spill, a stocktake
-- that found less (or more), a bag given away.
--
-- What is left in a lot is never stored. It is the lot's amount less every
-- `batch_ingredient` drawn from it less every row here, summed at query time by
-- `db::inventory`, so a corrected addition or receipt corrects the stock with it.
--
-- Positive is removed; negative puts back (a stocktake that found more than the books).
-- Canonical column plus the entry pair, the rule `ingredient_lot` and `batch_ingredient`
-- follow (docs/BUILDOUT.md 1c), so every sum reads one column.

CREATE TABLE lot_adjustment (
  id                INTEGER PRIMARY KEY,
  ingredient_lot_id INTEGER NOT NULL REFERENCES ingredient_lot(id),
  adjusted_at       INTEGER NOT NULL,
  qty_g             REAL,
  qty_l             REAL,
  qty_each          REAL,
  entry_qty         REAL NOT NULL,
  entry_unit        TEXT NOT NULL,
  reason            TEXT NOT NULL,
  CHECK ((qty_g IS NOT NULL) + (qty_l IS NOT NULL) + (qty_each IS NOT NULL) = 1)
);

CREATE INDEX lot_adjustment_lot_idx ON lot_adjustment(ingredient_lot_id);
