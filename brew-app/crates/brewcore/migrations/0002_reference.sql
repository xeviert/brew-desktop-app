-- Phase 3a: reference data. The first Tier 2 migration — typed in once, never
-- reconstructible from Fermentrack, and what every later Phase 3 table points at.
-- See docs/BUILDOUT.md "Phase 3 in detail".
--
-- `kind`, `category` and `form` carry no CHECK, for the reason `0001_tier1.sql`
-- gives for `stream.metric`: SQLite cannot widen a CHECK without rebuilding the
-- table, and all three of these sets will grow. The Rust enums in `db::reference`
-- are the enforcement, at the only write path. `source.kind` keeps its CHECK —
-- migrations are append-only, not re-litigated.

-- Fermenters, brites, kegs. Needed in Phase 3a and not Phase 7 because `batch`
-- references it in 3c.
CREATE TABLE vessel (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  kind        TEXT NOT NULL,             -- `VesselKind`
  capacity_l  REAL,
  notes       TEXT,
  -- Never hard-delete: a vessel is referenced by every batch that ever sat in it.
  -- "Active" is `archived_at IS NULL`.
  archived_at INTEGER
);

-- Separate from `ingredient` rather than folded into its `attrs` JSON: Phase 6's
-- finish detector needs the expected-FG band as a queryable number, and Phase 5's
-- strain rollup groups by it.
--
-- Attenuation and alcohol tolerance are FRACTIONS (0.78, not 78). The CHECK is what
-- stops a percent being typed in, which would otherwise read as a plausible band.
CREATE TABLE yeast_strain (
  id                INTEGER PRIMARY KEY,
  name              TEXT NOT NULL,
  lab               TEXT,
  form              TEXT NOT NULL,       -- `YeastForm`
  temp_min_c        REAL,
  temp_max_c        REAL,
  attenuation_min   REAL CHECK (attenuation_min IS NULL OR attenuation_min BETWEEN 0 AND 1),
  attenuation_max   REAL CHECK (attenuation_max IS NULL OR attenuation_max BETWEEN 0 AND 1),
  alcohol_tolerance REAL CHECK (alcohol_tolerance IS NULL OR alcohol_tolerance BETWEEN 0 AND 1),
  notes             TEXT,
  -- A lab's catalogue is unique by name; two labs may both sell an "EC-1118".
  UNIQUE (lab, name)
);

-- What a recipe calls for. `attrs` is opaque JSON for the category-specific fields
-- that do not deserve a column (alpha acid, ppm contribution, tablet strength).
CREATE TABLE ingredient (
  id              INTEGER PRIMARY KEY,
  name            TEXT NOT NULL UNIQUE,
  category        TEXT NOT NULL,         -- `Category`
  -- A `Unit::symbol`. Its dimension decides which canonical column a lot fills, so
  -- this is not merely a display preference.
  default_unit    TEXT NOT NULL,
  yeast_strain_id INTEGER REFERENCES yeast_strain(id),
  attrs           TEXT,
  notes           TEXT,
  archived_at     INTEGER
);

-- What you actually bought. Cost and traceability both start here: Phase 7's
-- cost-per-batch and "which batches used that suspect yeast lot" are the same join.
--
-- `total_cost_minor` is what the receipt says for the whole lot, NOT a per-gram
-- price. Minor units per canonical unit would be an integer that rounds to zero:
-- EUR 12.50 for 5 kg of sucrose is 0.25 cents/g, which truncates to 0 and makes
-- every cost in Phase 7 read EUR 0.00. Per-unit cost is a division done in f64 at
-- the point of display (`db::reference::lot_cost_per_canonical_unit`).
CREATE TABLE ingredient_lot (
  id               INTEGER PRIMARY KEY,
  ingredient_id    INTEGER NOT NULL REFERENCES ingredient(id),
  supplier         TEXT,
  lot_code         TEXT,
  -- Canonical, by the ingredient's dimension. Exactly one is set.
  qty_g            REAL,
  qty_l            REAL,
  qty_each         REAL,
  -- The same amount as it was typed, so the lot prints back the way it was entered.
  -- Both NOT NULL: a canonical value with no record of what was typed is how
  -- "2.5 kg" comes back as "2500 g" and nobody recognises their own inventory.
  entry_qty        REAL NOT NULL,
  entry_unit       TEXT NOT NULL,
  total_cost_minor INTEGER,
  currency         TEXT,
  purchased_at     INTEGER,
  expires_at       INTEGER,
  notes            TEXT,
  CHECK ((qty_g IS NOT NULL) + (qty_l IS NOT NULL) + (qty_each IS NOT NULL) = 1),
  -- An amount with no currency is a number nobody is allowed to add up.
  CHECK ((total_cost_minor IS NULL) = (currency IS NULL))
);

CREATE INDEX ingredient_lot_ingredient_idx ON ingredient_lot(ingredient_id);

-- Ions in ppm, which for water is mg/L by definition. NOT `*_mg_l`: the Phase 1c
-- linter reads a `_l` suffix as "this value is stored in litres", so that spelling
-- would pass the lint while meaning something else entirely.
CREATE TABLE water_profile (
  id              INTEGER PRIMARY KEY,
  name            TEXT NOT NULL UNIQUE,
  calcium_ppm     REAL,
  magnesium_ppm   REAL,
  sodium_ppm      REAL,
  chloride_ppm    REAL,
  sulfate_ppm     REAL,
  bicarbonate_ppm REAL,
  ph              REAL,
  notes           TEXT
);
