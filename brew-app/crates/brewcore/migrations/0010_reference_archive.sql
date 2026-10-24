-- Phase 3 UI pass, U1: `archived_at` on the two reference tables 0002 left without
-- one. See docs/BUILDOUT.md U1.
--
-- A discontinued strain and a used-up lot must leave the pickers while every batch
-- that used them still reads them — 3a's "never hard-delete" rule, which these two
-- tables were missed by. Until Phase 7's lot consumption, archiving is how a lot is
-- marked empty. "Active" is `archived_at IS NULL`, as for `vessel` and `ingredient`.

ALTER TABLE yeast_strain ADD COLUMN archived_at INTEGER;
ALTER TABLE ingredient_lot ADD COLUMN archived_at INTEGER;
