-- What a sensor measures, when that is not what its column name says.
--
-- A dialect maps a column to a metric by name: `brewd`'s `vessel_temp` is the
-- fermenter's temperature, wherever the probe actually hangs. When the user says a
-- stream measures something else, `metric` changes and `source_metric` keeps what the
-- dialect calls it, so the next sync finds the same stream instead of creating a new
-- one under the old metric. `NULL` means the two agree, as for every stream before
-- this migration.
--
-- Changing `metric` in place, rather than writing readings to a second stream, keeps
-- every `batch_stream` and `vessel_stream` binding and every past reading on it.

ALTER TABLE stream ADD COLUMN source_metric TEXT;
