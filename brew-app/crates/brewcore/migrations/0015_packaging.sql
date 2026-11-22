-- Phase 7a: packaging records. What left the vessel, in what, and how much.
--
-- One row per line packaged: two kegs and a case of bottles off one batch are three
-- rows. Each owns a `package` `batch_event`, which carries the time, so the timeline
-- stays the one record of when; deleting that event from the timeline deletes the line.
--
-- Loss is not a column: it is the batch's volume less what was packaged, and storing
-- it would be a second place for a corrected volume to disagree with.
--
-- `format` carries no CHECK, for the reason `0002_reference.sql` gives. `PackageFormat`
-- in `db::packaging` is the enforcement, at the only write path.

CREATE TABLE packaging_record (
  id               INTEGER PRIMARY KEY,
  batch_id         INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  event_id         INTEGER NOT NULL UNIQUE REFERENCES batch_event(id) ON DELETE CASCADE,
  format           TEXT NOT NULL,     -- `PackageFormat`
  container_size_l REAL CHECK (container_size_l IS NULL OR container_size_l > 0),
  container_count  INTEGER CHECK (container_count IS NULL OR container_count > 0),
  -- What actually went in, which a short last bottle makes less than size × count.
  volume_l         REAL NOT NULL CHECK (volume_l > 0),
  notes            TEXT
);

CREATE INDEX packaging_record_batch_idx ON packaging_record(batch_id);
