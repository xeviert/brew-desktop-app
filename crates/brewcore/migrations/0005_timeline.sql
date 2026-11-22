-- Phase 3d: the timeline, and the join that binds a sensor to a batch.
--
-- Events are not readings. A reading is a number on a time series; an event is
-- something you *did*. They share a timeline in the UI and nothing else, which is
-- why `batch_event` carries a JSON payload instead of columns: a `temp_ramp` and a
-- `package` have nothing in common, and the alternative is twenty nullable columns
-- where eighteen are always NULL. See docs/BUILDOUT.md "Phase 3 in detail" §3.
--
-- `kind` and `channel` carry no CHECK, for the reason `0002_reference.sql` gives for
-- `category` / `form`. `EventKind` and `AnnotationChannel` in `db::timeline` are the
-- enforcement, at the only write path.

CREATE TABLE batch_event (
  id       INTEGER PRIMARY KEY,
  batch_id INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  ts       INTEGER NOT NULL,
  kind     TEXT NOT NULL,        -- `EventKind`
  -- Opaque JSON, as `source.config` and `ingredient.attrs` are. Kind-specific fields
  -- stay out of the schema; Phase 5 relies on `kind` being a closed set, not on this.
  payload  TEXT,
  note     TEXT
);

-- One pitch per batch. `batch.pitched_at` is the same fact denormalised — it is what
-- 3c's "cannot leave planned without a pitch time" reads — and a second pitch row
-- would be exactly the two-places-to-disagree that 3c refused for `archived_at`.
-- `db::timeline::record_pitch` writes both, in one transaction; this index is what
-- makes a second one impossible rather than merely discouraged.
CREATE UNIQUE INDEX batch_event_pitch_idx ON batch_event(batch_id)
  WHERE kind = 'pitch';

-- The join that makes everything else work: a physical sensor mapped to a batch for
-- a time window, so the same fermenter can host batch 17 and then batch 18 with the
-- curves staying separate.
--
-- No `ON DELETE CASCADE` on `stream_id`, unlike `batch_id`. `stream` rows outlive a
-- Rebuild precisely so a binding survives it — see the note on
-- `tier1::delete_readings_for_source`: a rebuild is not a reason to unbind a batch
-- from its curve.
--
-- The window is half-open, `[starts_at, ends_at)`, with NULL meaning "still bound".
-- Two batches in one fermenter touch at a boundary and must both be legal; SQLite has
-- no exclusion constraint, so the overlap rule is checked on write in `bind_stream`.
CREATE TABLE batch_stream (
  id        INTEGER PRIMARY KEY,
  batch_id  INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  stream_id INTEGER NOT NULL REFERENCES stream(id),
  starts_at INTEGER NOT NULL,
  ends_at   INTEGER,
  CHECK (ends_at IS NULL OR ends_at > starts_at)
);

-- Tier 1, not Tier 2: derived from the log, thrown away and re-synced with it. It
-- lands in this migration rather than in `0001_tier1.sql` because 3d is what reads
-- it, and migrations are append-only. Phase 2's commit path writes it in the same
-- transaction as the readings; Rebuild deletes it alongside them.
--
-- `channel`, not `beer_ann` / `fridge_ann`: the `schema_conventions` linter rejects
-- any name containing "beer", and Fermentrack's vocabulary stays quarantined inside
-- the `fermentrack` module either way.
--
-- `WITHOUT ROWID` and a composite primary key for the same reason `reading` has them:
-- the key *is* the row, and re-ingesting the same bytes becomes a free `DO NOTHING`.
CREATE TABLE annotation (
  source_id INTEGER NOT NULL REFERENCES source(id),
  ts        INTEGER NOT NULL,
  channel   TEXT NOT NULL,       -- `AnnotationChannel`
  text      TEXT NOT NULL,
  PRIMARY KEY (source_id, ts, channel)
) WITHOUT ROWID;

CREATE INDEX batch_event_batch_idx   ON batch_event(batch_id, ts);
CREATE INDEX batch_stream_batch_idx  ON batch_stream(batch_id);
CREATE INDEX batch_stream_stream_idx ON batch_stream(stream_id);
