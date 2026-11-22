-- Phase 3c: batches. What a recipe version is *for* — the moment a batch points at
-- one, that version freezes (`db::batch::create_batch` calls 3b's
-- `lock_recipe_version` in the caller's transaction), and "every batch of
-- Citron-Verveine v3" becomes a question with an answer.
-- See docs/BUILDOUT.md "Phase 3 in detail" §2.
--
-- `beverage_type` and `status` carry no CHECK, for the reason `0002_reference.sql`
-- gives for `kind` / `category` / `form`. `BeverageType` and `BatchStatus` in
-- `db::batch` are the enforcement, at the only write path.

CREATE TABLE batch (
  id                INTEGER PRIMARY KEY,
  -- Auto-suggested per beverage type (`SELTZER-018`), but typed in the end: UNIQUE
  -- is the enforcement, `suggest_batch_code` only the convenience.
  code              TEXT NOT NULL UNIQUE,
  -- Nullable: an experiment poured from nothing is still a batch, and forcing a
  -- throwaway recipe on it would put fiction in the one tier that is never
  -- reconstructible. `plan_vs_actual` reports every addition as unplanned instead.
  recipe_version_id INTEGER REFERENCES recipe_version(id),
  -- On the batch in its own right, not read through the recipe, because a batch may
  -- have no recipe. It decides which optional side tables apply, which is what keeps
  -- twenty-five always-NULL beer columns off this table.
  beverage_type     TEXT NOT NULL,        -- `BeverageType`
  vessel_id         INTEGER REFERENCES vessel(id),
  status            TEXT NOT NULL,        -- `BatchStatus`
  volume_l          REAL,
  og                REAL,
  fg                REAL,
  -- t=0 for every later comparison. Non-NULL is what lets a batch leave `planned`;
  -- 3d's `pitch` event writes this column rather than replacing the rule.
  pitched_at        INTEGER,
  -- A batch is never hard-deleted. Archiving is this column and not a `status`
  -- value: two places to record one fact is two places to disagree, and an archived
  -- batch that was mid-conditioning should still read as conditioning.
  archived_at       INTEGER,
  notes             TEXT
);

-- The actuals, against `recipe_item`'s plan. They diverge constantly — a lot runs
-- out, a nutrient addition slips six hours, the volume comes in short — and only
-- keeping both makes the log diagnosable *and* the recipe repeatable.
--
-- The lot, not the ingredient: "which batches used that suspect yeast lot" and
-- Phase 7's cost-per-batch are the same join, and neither is answerable without it.
--
-- Canonical column plus entry unit, the same shape and the same CHECK as
-- `recipe_item` and `ingredient_lot` (docs/BUILDOUT.md 1c). The 3c checklist's
-- `qty, unit` predates that rule: a bare pair makes SUM(qty) meaningless, which is
-- every cost and every planned-vs-actual delta.
--
-- ON DELETE CASCADE, as on `recipe_item` and for the same reason: an actual has no
-- meaning apart from its batch.
CREATE TABLE batch_ingredient (
  id                INTEGER PRIMARY KEY,
  batch_id          INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  ingredient_lot_id INTEGER NOT NULL REFERENCES ingredient_lot(id),
  qty_g             REAL,
  qty_l             REAL,
  qty_each          REAL,
  entry_qty         REAL NOT NULL,
  entry_unit        TEXT NOT NULL,
  stage             TEXT NOT NULL,        -- `Stage`, the same set `recipe_item` uses
  -- Absolute, not an offset from pitch: an actual is something that happened at a
  -- time, and a batch can take an addition before it is pitched.
  added_at          INTEGER,
  notes             TEXT,
  CHECK ((qty_g IS NOT NULL) + (qty_l IS NOT NULL) + (qty_each IS NOT NULL) = 1)
);

CREATE INDEX batch_recipe_version_idx   ON batch(recipe_version_id);
CREATE INDEX batch_ingredient_batch_idx ON batch_ingredient(batch_id);
CREATE INDEX batch_ingredient_lot_idx   ON batch_ingredient(ingredient_lot_id);
