-- Phase 3e: manual measurements. The entry record, in Tier 2, fanning out into a
-- Tier 1 `reading` in the same transaction.
--
-- Why this table exists at all. A `reading` is (stream_id, ts, value) and nothing
-- else — that shape, with `WITHOUT ROWID`, is what makes "a week of backfill for one
-- stream" a clustered range scan, and widening it with five columns that are NULL on
-- 99.99 % of rows would pay for manual entry out of the chart's budget. But a manual
-- reading is typed once by a human and is never reconstructible: "12.5 °Bx on a
-- refractometer at 24 °C, wort factor 1.04" cannot be recovered from the 1.048 it
-- became. Tier 1 is the tier we are allowed to throw away and re-sync; this is not
-- that. So the entry record lands here, in the tier that gets backed up and is never
-- rebuilt, and the *derived* number goes to `reading` — where the chart,
-- `batch_stream` and Phase 4 already know how to find it, with no second read path
-- to write and keep in step.
--
-- `value` is the canonical column and `entry_value` / `entry_unit` are what was
-- typed: the same rule 1c settled for `ingredient_lot`, `recipe_item` and
-- `batch_ingredient`, for the same reason. A bare (value, unit) pair makes every
-- query that reads gravity mean three different things at once. It is also why there
-- is no `value_plato` or `value_brix` column — `tests/schema_conventions.rs` rejects
-- any name ending in `_plato` or `_brix`, and a scale belongs in a value, not a name.
--
-- `metric`, `entry_unit` and `instrument` carry no CHECK, for the reason
-- `0002_reference.sql` gives for `category` / `form` and `0005_timeline.sql` gives
-- for `kind`: SQLite cannot widen a CHECK without rebuilding the table, and `Metric`,
-- `EntryUnit` and `Instrument` in `db::measurement` are the enforcement at the only
-- write path.
--
-- `stream_id` is stored rather than re-derived. It could be found again through the
-- batch's manual source, but the fan-out target is a fact about *this row* and the
-- delete path has to be exactly right: a measurement removed without its reading
-- leaves Tier 1 asserting a number Tier 2 has disavowed, with nothing to detect it.
-- No `ON DELETE CASCADE` on it, unlike `batch_id`, for the reason
-- `0005_timeline.sql` gives for `batch_stream.stream_id`: `stream` rows outlive a
-- Rebuild on purpose.
--
-- The three correction columns record *what was applied*, not a request to apply
-- anything. `sample_temp_c` and `calibration_temp_c` both non-NULL means the
-- hydrometer temperature correction is already in `value`; `wort_correction`
-- non-NULL means the refractometer reading was divided by it before Terrill's cubic.
-- Storing them is what makes `value` auditable in three years, when the coefficients
-- in `calc` have been revised and the question is which readings moved.
CREATE TABLE manual_measurement (
  id                 INTEGER PRIMARY KEY,
  batch_id           INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  -- The Tier 1 row this fanned out to; (stream_id, ts) is that row's primary key.
  stream_id          INTEGER NOT NULL REFERENCES stream(id),
  ts                 INTEGER NOT NULL,
  metric             TEXT NOT NULL,   -- `Metric`, the same vocabulary `stream.metric` uses
  -- Canonical and already corrected: SG, °C, pH or g/L as tartaric acid, by `metric`.
  value              REAL NOT NULL,
  entry_value        REAL NOT NULL,
  entry_unit         TEXT NOT NULL,   -- `EntryUnit`
  instrument         TEXT,            -- `Instrument`; NULL where no device is being claimed
  sample_temp_c      REAL,
  calibration_temp_c REAL,
  wort_correction    REAL
);

-- One measurement of one metric per second, matching `reading`'s own primary key.
-- Without it, a second entry at the same second would hit `insert_readings`'
-- `ON CONFLICT DO NOTHING` and leave a Tier 2 row pointing at a Tier 1 value that is
-- somebody else's. `db::measurement` sees that case in the insert count and returns
-- `Error::MeasurementExists`; this index is what makes the state unreachable rather
-- than merely unreached.
CREATE UNIQUE INDEX manual_measurement_point_idx ON manual_measurement(stream_id, ts);

CREATE INDEX manual_measurement_batch_idx  ON manual_measurement(batch_id, ts);
CREATE INDEX manual_measurement_stream_idx ON manual_measurement(stream_id);
