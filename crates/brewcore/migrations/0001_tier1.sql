-- Tier 1: everything derived from Fermentrack. Cheap to throw away and re-sync;
-- never hand-edited, never written outside the sync module.
-- See docs/BUILDOUT.md "Data model sketch".

-- A remote log file, or the local pseudo-source that manual measurements go to.
CREATE TABLE source (
  id     INTEGER PRIMARY KEY,
  kind   TEXT NOT NULL CHECK (
           kind IN ('fermentrack_beer_log', 'fermentrack_gravity_log', 'manual')
         ),
  name   TEXT NOT NULL,
  config TEXT                            -- JSON: base_url, device_id, beer_id/log_id
);

-- One metric series. `external_id` is opaque above the data layer: today it is a
-- Fermentrack device/beer pair, in v2 it will be an MQTT topic, and nothing outside
-- the `fermentrack` module is allowed to parse it.
--
-- `metric` is a closed vocabulary, but it is enforced by the Rust `Metric` enum at
-- the only write path rather than by a CHECK: SQLite cannot widen a CHECK without
-- rebuilding the table, and Phase 3 adds manual metrics (ph, ta) to this set.
CREATE TABLE stream (
  id          INTEGER PRIMARY KEY,
  source_id   INTEGER NOT NULL REFERENCES source(id),
  external_id TEXT NOT NULL,
  metric      TEXT NOT NULL,
  label       TEXT,
  UNIQUE (source_id, external_id, metric)
);

CREATE INDEX stream_source_idx ON stream(source_id);

-- WITHOUT ROWID + this composite PK makes `WHERE stream_id = ? AND ts BETWEEN ? AND ?`
-- a clustered range scan, which is the query every chart runs.
CREATE TABLE reading (
  stream_id INTEGER NOT NULL REFERENCES stream(id),
  ts        INTEGER NOT NULL,            -- UTC epoch seconds
  value     REAL NOT NULL,
  PRIMARY KEY (stream_id, ts)
) WITHOUT ROWID;

-- One row per remote file, not per stream: one CSV feeds up to eight streams.
CREATE TABLE sync_state (
  source_id   INTEGER PRIMARY KEY REFERENCES source(id),
  remote_path TEXT NOT NULL,
  byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
  header      TEXT,                      -- header line as fetched; a change forces a refetch
  last_ts     INTEGER,
  last_error  TEXT,
  synced_at   INTEGER NOT NULL
);
