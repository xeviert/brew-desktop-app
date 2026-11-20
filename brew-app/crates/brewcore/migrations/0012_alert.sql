-- Phase 6: alerts. `brewd` judges the rules on the Pi; this is the desktop's copy of
-- what it said, and the per-batch settings it judges by.
-- See docs/BUILDOUT.md, Phase 6.

-- Per-batch overrides of the defaults `db::alert::build_watches` derives. One row per
-- batch that has any; no row is "all defaults". A NULL band end is "the yeast's", and
-- a band with neither end set and no yeast band is no temperature rule at all.
CREATE TABLE batch_alert (
  batch_id   INTEGER PRIMARY KEY REFERENCES batch(id) ON DELETE CASCADE,
  temp_min_c REAL,
  temp_max_c REAL,
  target_sg  REAL,
  -- Not watched: nothing is judged and nothing is pushed for this batch.
  muted      INTEGER NOT NULL DEFAULT 0 CHECK (muted IN (0, 1)),
  CHECK (temp_min_c IS NULL OR temp_max_c IS NULL OR temp_min_c <= temp_max_c)
);

-- One row per alert `brewd` raised, folded from its alert log. The id is `brewd`'s:
-- the log is applied in order and may be applied twice after a truncation, so every
-- write here is an upsert keyed on it. `rule` carries no CHECK, for the reason
-- `0002_reference.sql` gives; `alert::Rule` is the enforcement.
CREATE TABLE alert (
  id            INTEGER PRIMARY KEY,
  batch_id      INTEGER NOT NULL REFERENCES batch(id),
  rule          TEXT NOT NULL,
  device        TEXT,
  raised_at     INTEGER NOT NULL,
  detail        TEXT NOT NULL,
  cleared_at    INTEGER,
  -- Why `brewd` cleared it when the rule didn't: the rule no longer applies, or the
  -- batch is no longer watched.
  cleared_note  TEXT,
  acked_at      INTEGER,
  snoozed_until INTEGER,
  notified_at   INTEGER
);

CREATE INDEX alert_batch_idx ON alert(batch_id, raised_at);
CREATE INDEX alert_open_idx ON alert(raised_at) WHERE cleared_at IS NULL;

-- Where the alert log sync has read to, per `brewd`. Keyed on the base URL because
-- the log belongs to one `brewd`: pointing the app at another starts its log at 0.
CREATE TABLE alert_sync (
  base_url    TEXT PRIMARY KEY,
  byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
  synced_at   INTEGER NOT NULL
);
