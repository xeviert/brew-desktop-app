-- Phase 7b: the cleaning and sanitation log, per vessel.
--
-- On the vessel, not the batch: a fermenter is cleaned between batches, and the
-- question the log answers is "was FV1 sanitised since the last batch left it", which
-- `db::cleaning::vessel_hygiene` asks at the pitch.
--
-- No `ON DELETE CASCADE`: a vessel is archived, never deleted. `kind` carries no CHECK,
-- for the reason `0002_reference.sql` gives; `CleaningKind` is the enforcement.

CREATE TABLE vessel_cleaning (
  id         INTEGER PRIMARY KEY,
  vessel_id  INTEGER NOT NULL REFERENCES vessel(id),
  cleaned_at INTEGER NOT NULL,
  kind       TEXT NOT NULL,      -- `CleaningKind`
  -- What was used and how strong: "PBW 1 %", "Star San 1.5 mL/L".
  chemical   TEXT,
  notes      TEXT
);

CREATE INDEX vessel_cleaning_vessel_idx ON vessel_cleaning(vessel_id, cleaned_at);
