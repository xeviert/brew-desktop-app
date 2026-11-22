-- Phase 5c: saved queries. A structured filter, never SQL text: stored SQL would
-- couple every saved query to this schema and break on the next migration.
-- See docs/BUILDOUT.md 5c.
--
-- `filter` is `db::query::BatchFilter` as JSON with a `"v"` beside its fields. The
-- version is inside the JSON and not a column because it describes the JSON, and an
-- unknown field or version on load is an error rather than a skipped criterion:
-- dropping one *widens* the result, and a wider result looks right.

CREATE TABLE saved_query (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  filter      TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  -- Never hard-deleted, like the rest of Tier 2.
  archived_at INTEGER
);

-- Unique among the active ones only, so an archived query's name can be reused.
CREATE UNIQUE INDEX saved_query_active_name_idx ON saved_query(name) WHERE archived_at IS NULL;
