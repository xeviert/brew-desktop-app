-- Phase Va. `source.kind` loses its CHECK.
--
-- 0001 closed it over the two Fermentrack kinds plus 'manual' — the one
-- closed-vocabulary column that did not follow 0001's own reasoning for
-- `stream.metric`: SQLite cannot widen a CHECK without rebuilding the table, and
-- the set was always going to grow. v2 adds 'brewd_journal' today. The Rust
-- `SourceKind` enum in `db::tier1` is the enforcement from here on, at the only
-- write path, as `Metric` is.
--
-- A rebuild of a table that `stream`, `sync_state` and `annotation` reference, so
-- the runner applies it with foreign_keys=OFF from before BEGIN and requires an
-- empty `PRAGMA foreign_key_check` before COMMIT (`Migration::rebuilds_tables`).
-- References are by name, so they bind to the new table once it takes the old
-- one's name. `source` has no indexes, triggers or views to recreate.

CREATE TABLE source_new (
  id     INTEGER PRIMARY KEY,
  kind   TEXT NOT NULL,
  name   TEXT NOT NULL,
  config TEXT                            -- JSON, shape decided by `kind`
);

INSERT INTO source_new (id, kind, name, config)
  SELECT id, kind, name, config FROM source;

DROP TABLE source;

ALTER TABLE source_new RENAME TO source;
