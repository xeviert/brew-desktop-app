-- Day-N reminders: checks on the calendar rather than on a reading ("taste on day
-- 10"), counted from the pitch. `brewd` raises one as due when its time comes.
--
-- Done is `done_event_id`, as for `schedule_step`: ticking a reminder writes a
-- `reminder_done` `batch_event`, and deleting that event un-ticks it. Unlike steps,
-- reminders aren't ordered against each other.

CREATE TABLE batch_reminder (
  id            INTEGER PRIMARY KEY,
  batch_id      INTEGER NOT NULL REFERENCES batch(id) ON DELETE CASCADE,
  after_hours   REAL NOT NULL CHECK (after_hours >= 0),   -- since the pitch
  text          TEXT NOT NULL,
  done_event_id INTEGER REFERENCES batch_event(id) ON DELETE SET NULL
);

CREATE INDEX batch_reminder_batch_idx ON batch_reminder(batch_id);
