-- A batch's fermentation schedule: ordered steps, each with what "ready to move on"
-- means — a temperature band, a gravity to fall to, a minimum time.
--
-- On the batch, not the recipe version: a schedule gets tweaked mid-ferment, and a
-- batch with no recipe still needs one.
--
-- Done is `done_event_id`, not a flag. Ticking a step writes a `step_done`
-- `batch_event`, so the timeline stays the one record of what was done and when;
-- deleting that event un-ticks the step. A step starts when the one before it is
-- done, the first at the pitch, so no start column.

CREATE TABLE schedule_step (
  id            INTEGER PRIMARY KEY,
  batch_id      INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  position      INTEGER NOT NULL,
  name          TEXT NOT NULL,
  temp_min_c    REAL,
  temp_max_c    REAL,
  target_sg     REAL,              -- reached once gravity is at or below it
  min_hours     REAL,              -- since the step started
  note          TEXT,
  done_event_id INTEGER REFERENCES batch_event(id) ON DELETE SET NULL,
  UNIQUE (batch_id, position),
  CHECK (temp_min_c IS NULL OR temp_max_c IS NULL OR temp_min_c <= temp_max_c),
  CHECK (min_hours IS NULL OR min_hours >= 0)
);
