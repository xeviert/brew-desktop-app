-- Phase Ve: the sensors a vessel carries, so a pitch binds them without being asked.
--
-- A fermenter with a probe in its thermowell and a fridge sensor beside it feeds the
-- same two streams to every batch it hosts. Without this, every pitch is followed by
-- binding those two streams by hand, and forgetting once leaves a batch with no curve.
-- `db::timeline::record_pitch` binds each row here from the pitch time; moving to
-- `packaged` closes them again.
--
-- A default, not a binding. Editing it changes the next pitch and nothing already
-- bound: `batch_stream` is the record of what happened, this is only what to assume.
--
-- No `ON DELETE CASCADE` on either side. A vessel is archived, never deleted, and
-- `stream` rows outlive a Rebuild on purpose — see `0005_timeline.sql`.
CREATE TABLE vessel_stream (
  vessel_id INTEGER NOT NULL REFERENCES vessel(id),
  stream_id INTEGER NOT NULL REFERENCES stream(id),
  PRIMARY KEY (vessel_id, stream_id)
) WITHOUT ROWID;

CREATE INDEX vessel_stream_stream_idx ON vessel_stream(stream_id);
