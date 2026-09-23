-- Phase 3b: recipes, versioned properly. What a batch points at in 3c, and what
-- makes "every batch of this recipe" a question with an answer.
-- See docs/BUILDOUT.md "Phase 3 in detail" §1.
--
-- `beverage_type` and `stage` carry no CHECK, for the reason `0002_reference.sql`
-- gives for `kind` / `category` / `form`: SQLite cannot widen a CHECK without
-- rebuilding the table, and both sets will grow. The Rust enums in `db::recipe` are
-- the enforcement, at the only write path.

CREATE TABLE recipe (
  id            INTEGER PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  -- Decides which optional side tables apply in 3c, so `batch` never grows the
  -- twenty-five always-NULL beer columns that multi-beverage scope otherwise buys.
  beverage_type TEXT NOT NULL,          -- `BeverageType`
  notes         TEXT,
  archived_at   INTEGER
);

-- The unit everything else references. A batch points at a *version*, never at a
-- recipe, and the moment it does the version is frozen: edits fork version_no + 1.
--
-- That freeze is what makes a frozen version its own snapshot — deduplicated and
-- diffable — instead of a copy of the recipe pasted onto every batch row.
--
-- No `created_at`: neither 0001 nor 0002 carries one, and `version_no` already
-- orders a recipe's history.
CREATE TABLE recipe_version (
  id              INTEGER PRIMARY KEY,
  recipe_id       INTEGER NOT NULL REFERENCES recipe(id),
  version_no      INTEGER NOT NULL,
  target_volume_l REAL,
  target_og       REAL,
  target_fg       REAL,
  -- A fraction, not a percent, like `yeast_strain.attenuation_*`.
  target_abv      REAL,
  -- Set the first time a batch references this version. Non-NULL means frozen.
  -- 3c's create_batch sets it inside its own transaction.
  locked_at       INTEGER,
  notes           TEXT,
  UNIQUE (recipe_id, version_no)
);

-- What you meant to add, against `batch_ingredient`'s record of what you actually
-- did. Recording only the plan makes the log useless for diagnosis; recording only
-- the actuals makes the recipe unrepeatable.
--
-- Canonical column plus entry unit, the same shape as `ingredient_lot` and for the
-- same reason (docs/BUILDOUT.md 1c): a bare `qty` + `unit` pair makes SUM(qty)
-- meaningless. The dimension comes from the ingredient's `default_unit`, never from
-- the caller.
--
-- ON DELETE CASCADE, alone in this schema: an item has no meaning apart from its
-- version, and a version is only ever deleted before it is locked.
CREATE TABLE recipe_item (
  id                INTEGER PRIMARY KEY,
  recipe_version_id INTEGER NOT NULL REFERENCES recipe_version(id) ON DELETE CASCADE,
  ingredient_id     INTEGER NOT NULL REFERENCES ingredient(id),
  qty_g             REAL,
  qty_l             REAL,
  qty_each          REAL,
  entry_qty         REAL NOT NULL,
  entry_unit        TEXT NOT NULL,
  stage             TEXT NOT NULL,      -- `Stage`
  -- Hours from pitch. For a sugar wash the nutrient schedule is the process
  -- variable most likely to explain a batch going wrong, and Phase 5 has to be able
  -- to query it — so it is a column and not a line in `notes`.
  offset_hours      REAL,
  notes             TEXT,
  CHECK ((qty_g IS NOT NULL) + (qty_l IS NOT NULL) + (qty_each IS NOT NULL) = 1)
);

CREATE INDEX recipe_version_recipe_idx ON recipe_version(recipe_id);
CREATE INDEX recipe_item_version_idx   ON recipe_item(recipe_version_id);
