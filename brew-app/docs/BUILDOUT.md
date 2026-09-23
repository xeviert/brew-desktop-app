# Buildout Plan — Option 1 (App on top of Fermentrack)

Target: a Tauri desktop app that reads Fermentrack's data, enriches it with batch /
recipe / production records in a local SQLite file, and becomes the brewery
operating system. Fermentrack stays the fermentation-control appliance and the
logger of record. See [../ARCHITECTURE.md](../ARCHITECTURE.md) for the why.

**Scope:** hard seltzer first, but beer and other alcoholic fermentables are in
scope from the start. That is a schema constraint, not a someday-feature — no
table, column, or type in our own code is named "beer". `beverage_type` is a
column on `batch`, and anything style-specific (mash, hops, boil, nutrient
schedules) hangs off optional per-type tables rather than being bolted onto a
seltzer-shaped core. Fermentrack's own `Beer` naming stays quarantined inside the
`fermentrack` module.

**MQTT is v2, not v1.** v1 talks HTTP to Fermentrack, which has no MQTT anyway.
v2 drops Fermentrack and speaks MQTT straight to the ESP32s. Nothing in v1 should
assume a broker exists; the traits in "Keeping the Option 2 door open" are where
MQTT lands later.

---

## Checklist

### Phase 0 — Decisions & spikes — **answered from source**
- [x] Which Fermentrack: legacy Django, `thorrak/fermentrack@1aea05b0`. Fermentrack 2 unreleased
- [x] Read the API surface from the code → [FERMENTRACK-API.md](FERMENTRACK-API.md)
- [x] MQTT: **does not exist in Fermentrack.** Zero hits repo-wide. Transport is HTTP
- [x] History transport: append-only CSV under `/data/`, incremental via HTTP `Range`
- [x] Auth: none. No API view is decorated; `/data/` is open. Base URL only, no token
- [x] Cadence: `data_point_log_interval` default 30 s (10 s…1 h). Keep everything
- [x] Control path: `brewpi-script` socket, cp437, `setBeer=<temp>` etc.
- [x] Lock the frontend stack (proposal below)
- [ ] **Blocked — no Fermentrack instance yet.** Confirm against a real box:
      `curl http://<host>/api/devices/`, list `/data/`, capture one real `_full.csv`
- [ ] Unblock the blocker: write a fixture generator instead (see Phase 2 detail).
      Phases 1–2 are fully buildable without Fermentrack ever running

### Phase 1 — Skeleton
**1a. Project shape**
- [x] `cargo create-tauri-app`, Tauri v2, Svelte + TypeScript
- [x] Split into a Cargo workspace: `crates/brewcore` (all logic, no Tauri dep) +
      `src-tauri` (thin shell). Non-negotiable — it's what makes logic testable
      with plain `cargo test` and what lets v2's controller reuse the same crate
- [x] `src-tauri` depends on `brewcore`; `brewcore` never depends on `tauri`
- [x] Module skeleton inside `brewcore`: `db/`, `fermentrack/`, `calc/`, `config.rs`, `error.rs`

**1b. Database**
- [x] `brewery.db` in Tauri's app-data dir; `BREWAPP_DB` env var overrides (tests, dev)
- [x] Connection setup applies pragmas every time: `journal_mode=WAL`,
      `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`
- [x] One writer connection behind a `Mutex` (an actor task buys nothing until
      Phase 2 wants batching); reads via an `r2d2_sqlite` pool
- [x] Migration runner: `migrations/NNNN_name.sql` embedded with `include_str!`,
      each applied inside a transaction, `PRAGMA user_version` bumped in the same txn
- [x] Migrations are append-only and never edited after commit
- [x] Migration test: empty db → head, and vN → head, both asserted
- [x] Migration `0001`: the Tier 1 tables (`source`, `stream`, `reading`, `sync_state`)

**1c. Conventions locked now, not later**
- [x] Timestamps: UTC epoch **seconds**, `INTEGER`, everywhere. Format only at the UI edge
- [x] Temps stored °C. Gravity stored as SG (`1.0374`), never Plato/Brix — convert on entry
- [x] Volumes stored litres, masses grams
- [x] Money stored as integer **minor units** + an ISO 4217 code, never float; mixing
      currencies is an error, and there is no FX conversion
- [x] A `units` module owning every conversion, with round-trip unit tests
- [x] No table or column in our schema named "beer" (see Scope above)
- [x] `tests/schema_conventions.rs`: a blocklist linter over `sqlite_master` /
      `PRAGMA table_info`, so migration `0002` fails CI the day it types `cost REAL`

**1d. Plumbing**
- [x] Config: `serde` + TOML in Tauri's config dir. Fields: `fermentrack.base_url`,
      `sync.poll_interval_secs`, `ui.temp_display` (`C`/`F`), `ui.gravity_display`
- [x] Config has working defaults; a missing file is not an error
- [x] `error.rs`: `thiserror` enum in core, converted at the Tauri boundary to a
      serializable `{code, message, detail}` — Tauri commands can't return opaque errors
- [x] `tracing` + `tracing-subscriber`, rolling file in Tauri's log dir + stderr in dev
- [x] Tauri command surface for this phase only: `get_config`, `set_config`,
      `db_status` (path, size, schema version, row counts)
- [x] Frontend: one route, a settings pane, a status footer. No chart yet
- [x] CI on `windows-latest`: `cargo fmt --check`, `cargo clippy -- -D warnings`,
      `cargo test --workspace`, `npm run check`

**Done when:** the app launches, creates `brewery.db` at the right path, reports
its schema version in the UI, and `cargo test --workspace` is green in CI.

### Phase 2 — Ingest
**2a. Test rig first — you have no Fermentrack** (`crates/fixtures`, package `brewfixture`)
- [x] `xtask gen-fixture`: emit a byte-accurate `_full.csv` (header + rows,
      `%Y/%m/%d %H:%M:%SZ`, plausible ferment curve, gaps, absent sensors, an
      annotation containing a comma and one containing a quote, a mid-file
      `temp_format` switch)
- [x] Emit both variants: `gravity_enabled` on/off, `model_version` 1 and 2
- [x] `start_hours_in` so a short fixture can sit mid-ferment: a five-row log at
      t=0 is all lag phase — flat OG, IDLE on every row — which proves nothing.
      The `active` scenario is the one where gravity falls and the controller works
- [x] Committed goldens under `crates/fixtures/tests/fixtures/`, byte-compared in
      `tests/golden.rs`. Regenerate deliberately:
      `cargo xtask gen-fixture --scenario all --rows 5 --seed 1 --out crates/fixtures/tests/fixtures`
- [x] `wiremock` harness that honours `Range` and can be told to *ignore* it, plus
      `set_fault` for 404 / HTML / 500, and `append` / `truncate_to` so the body can
      grow between requests
- [x] Growing-file test: serve N bytes, sync, serve N+M, sync again, assert no dupes
      — landed with 2e as `tests/sync_write.rs`, run under both `RangeMode`s and
      asserted to reach the same database either way. 2c has the transport half:
      `a_resume_fetches_only_the_bytes_appended_since_the_last_sync`
- [ ] Replace fixtures with a real capture once Fermentrack is up — same tests

**2b. Parser (`fermentrack/csv_log.rs`)**
- [x] Parse the header row into a column map; unknown columns ignored, not fatal
- [x] Reject a file whose header lacks `log_time` — wrong file or HTML error page
- [x] `_full.csv` only. Classify by column, three ways: `beer_temp`+`beer_set`
      +`temp_format` is the file we want; the same without `temp_format` is a
      `_graph.csv`; no controller columns but a `gravity` one is a gravity-sensor
      log (`fermentrack.gravity_log`). Anything else is "not a Fermentrack log"
      with the header preview — never a guess at which file it was
- [x] A bare `0` (no decimal point) in a temp or gravity column → `None`. Fermentrack
      writes `x or 0`, so a missing reading is an integer `0` while a real one always
      carries a decimal point. `state`, `temp_format` and `associated_beer_id` are
      genuine integers and are exempt. Empty cells (`beer_ann`, `fridge_ann`) → `None`
- [x] Per-row temp conversion driven by that row's own `temp_format` cell
- [x] `state` parsed to the 0–9 enum; out-of-range → `None` + a warning, not a panic
- [x] Column presence, never a version check: `model_version` only decides whether
      `_graph.csv` carries `state`, and `_full.csv` has it at index 8 in every version
- [x] `ParseReport` carries `bytes_consumed` and `stopped_early`. A `csv::Error`
      mid-chunk stops the read, and 2d must advance by what was *parsed*, not by
      what was fetched
- [x] Unit tests per quirk, each with a 3-line inline CSV

**2c. Fetch (`fermentrack/client.rs`)**
- [x] `GET` with `Range: bytes=<offset>-`
- [x] `206` → parse body. `200` → server ignored Range; skip `offset` bytes locally
- [x] `416` → read `Content-Range: bytes */N`. **`N == offset` is the steady state**
      — nothing new since the last sync, so it is a no-op, not a reason to refetch.
      `N < offset` (or no `Content-Range` at all) → truncated/recreated; reset to 0
- [x] `206` whose `Content-Range` total is < `offset` → truncated; reset to 0
- [x] A `200` shorter than `offset` is the *likelier* truncation — it is what a
      Range-ignoring proxy in front of a rotated log produces. Equal length is
      `Unchanged`, not an empty chunk
- [x] A `206` starting somewhere other than `offset` is refused, not trusted.
      Storing its rows as though they began at `offset` is the one corruption 2d
      can never detect afterwards
- [x] `416` with `N > offset` is an error, not truncation: resetting to 0 gets the
      same `416` forever. Wedge visibly, same principle as `stopped_early`
- [x] `404` → surface "log missing or renamed", keep the source, don't wipe readings
- [x] Non-2xx/HTML body → error with the first 200 bytes in the message. HTML
      arrives with a **200**, so the content check runs *before* the offset
      arithmetic — a short dashboard at a large offset would otherwise classify as
      truncation, reset the offset, and be ingested on the next poll
- [x] Timeouts + bounded retry with backoff; a down Fermentrack must not hang the UI.
      Connect 5 s and read 30 s, but no whole-request timeout — "app closed for a
      week" is one legitimately huge request. The retry here is in-fetch and in
      milliseconds (3 attempts, 250 ms ×4); 2g's per-source backoff is a separate,
      minutes-scale mechanism between polls
- [x] `remote_path` is stored **decoded**, the way Fermentrack names the file, and
      percent-encoded once at the URL boundary in `log_url`. An already-absolute
      `remote_path` is passed through verbatim so a pasted browser URL doesn't
      become `%2520`

**2d. Offsets and partial lines**
- [x] **Split before you parse**, at every offset including 0. The `csv` crate treats
      EOF as a record terminator, so a half-written row parses as a *complete* one:
      a cut at `...,21.0,0,C,1` has the right 11 cells and a valid `log_time` and
      commits as a real reading, and Rust's `f64` parser accepts `"18."` and stores
      18.0 where the file says 18.06. `split_complete_lines` first removes the
      possibility. No remainder buffer: `byte_offset` only ever advances past
      complete terminated records, so the next `Range: bytes=<offset>-` re-requests
      the partial tail for free — no cross-restart state and no schema column
- [x] Advance `byte_offset` only past fully-parsed, committed lines — by
      `ParseReport::bytes_consumed`, never by the length of the fetched chunk.
      All arithmetic derives from `FetchOutcome::Fresh { offset }`, never from
      `sync_state.byte_offset`: after a restart the caller fetches from 0 while the
      stored offset is still stale
- [x] `stopped_early` → commit what parsed, stop at `bytes_consumed`, write
      `last_error`. The next poll refetches the faulty bytes and fails the same way,
      so the source wedges **visibly** instead of skipping rows nobody notices are
      missing. "Rebuild from scratch" (2g) is the escape hatch
- [x] Store the header text in `sync_state`, without its terminator — a CRLF→LF
      change is not a column change. A full refetch *reveals* a header change; it is
      not caused by one, because a resume never re-fetches the header. The only case
      that matters is a deleted-and-recreated log, and both halves of it already end
      at offset 0, where the header is compared
- [x] Verify a resume starts on a record boundary — the first record of the chunk
      must re-parse to exactly one row. This check **is** the recreated-file
      detector for the case truncation cannot see: a new file at or past our offset
      leaves the offset pointing inside a record of a *different* file. Zero extra
      requests, so 2d needs no prefix probe. A restart clears `byte_offset`,
      `header` **and** `last_ts` — a recreated file is a different brew whose rows
      can predate everything we have
- [x] `MAX_PARTIAL_RECORD_BYTES` (64 KB): one stray `"` puts the splitter inside a
      quoted field for the rest of the file, and every poll then finds no complete
      record, writes nothing and reports nothing. Silent stalls are the one outcome
      forbidden here, so a partial record past the cap becomes a `last_error`. Real
      records are 60–120 bytes; at a 30 s cadence a stuck offset crosses the cap in
      about four hours
- [x] `rows_parsed == 0 && rows_skipped > 0` → `last_error`. Toggling
      `gravity_enabled` on a live Beer appends rows wider than the header the file
      was created with: 100 % row loss, otherwise invisible
- [x] A restart is a `SyncStep` variant, not an `Err` — same rule as `FetchOutcome`:
      a persisted-state change is a variant, anything leaving state alone except
      `last_error` is an error

**2e. Write path**
- [x] Row fan-out: one CSV row → up to 8 `reading` rows, one per metric column.
      `LogRow::readings()` yields `(column, metric, value)` so the CSV name and our
      `Metric` stay paired in one table literal
- [x] `stream.external_id` is the **CSV column name** (`beer_temp`), not `B17.beer_temp`:
      the `source` row already identifies the file, so a beer id there would duplicate
      `source.config` and break on a Fermentrack rename
- [x] Auto-create `stream` rows on first sight of a column, inside the same txn — and
      only for a metric that actually carried a value, so an absent room probe never
      grows an empty series
- [x] `INSERT ... ON CONFLICT(stream_id, ts) DO NOTHING`; ignores counted in
      `CommitReport`, which is what 2g's `sync://status` reports
- [x] One transaction per fetched chunk, covering readings + offset + header. The
      in-memory `SyncState` is replaced **only after** `tx.commit()` returns: an offset
      that moved while the txn rolled back is the same permanent hole, seen from the
      other side
- [x] An `UPDATE sync_state` matching no row is `Ok(0)`, not an error — checked
      explicitly, or readings would commit under an offset stored nowhere
- [x] Batch inserts via a prepared statement; do not commit per row
- [x] Split by ownership: `db/tier1.rs` (source / stream / reading, **no `fermentrack`
      import** — Phase 3e and v2's MQTT write through it unchanged) under
      `fermentrack/store.rs` (offsets, headers, the `SyncState` row)
- [x] 2g's two primitives land here, unwired: `delete_readings_for_source` (Rebuild —
      a restart deletes before refetching, or `DO NOTHING` preserves exactly the bad
      rows Rebuild exists to fix) and `newest_reading` (the footer's data age)

**2f. Discovery — no API lists the logs**
- [x] v1: user pastes the log URL, or a path relative to the configured base, once
      when creating a source. ~~or device id + beer id~~ — **not buildable**: the
      filename is `Device {d} - B{id} - {beer_name}_full.csv`, so the *name* is in the
      path too and no API returns it. The ids are read back *out* of a pasted path for
      display and are never used to construct one
- [x] `Client::fetch_prefix(url, len)` returns **bytes, never a `FetchOutcome`** — a
      capped body would otherwise reach `ingest` as `Fresh { offset: 0 }`,
      indistinguishable from a genuine complete first fetch
- [x] Validate on save by fetching the first 2 KB and parsing the header. Surface
      `fermentrack.graph_log` and `fermentrack.gravity_log` as distinct messages —
      a pasted gravity-log URL is a different mistake from a pasted `_graph.csv`
- [x] `csv_log::inspect_header` classifies from the header alone. Not
      `parse_full_log` on the prefix: the `csv` crate treats EOF as a terminator, so
      the half-row a 2 KB cut ends on parses as a complete reading
- [x] `probe_log` asserts `log_url(base_url, remote_path) == url` before returning.
      `source.config` holds `base_url` + ids; the path lives only in `sync_state`,
      where the `ON CONFLICT` that resets `byte_offset` on a repoint can see it change
- [ ] Optional helper: scrape `/beer/list/` and `/gravity/logs/` to offer a picklist.
      **Deferred**: paste-a-URL covers one or two fermenters, and scraping needs an
      HTML parser dependency plus a golden of Fermentrack's list page that cannot be
      captured without a live box. Revisit after the Phase 0 validation run
- [x] `GET /api/devices/` only to confirm the host is a Fermentrack at all

**2g. Scheduler and UI**
- [x] Tokio task per source: sync on launch, then every `poll_interval_secs` (default 60).
      `sync::SyncManager`; the interval is re-read inside the loop, so a change in
      settings takes effect on the next tick rather than the next launch
- [x] Jitter the interval; exponential backoff to a ceiling on repeated failure.
      `sync::backoff` — ±20 %, 15 min ceiling, seeded xorshift rather than a `rand`
      dependency
- [x] Back off a source whose `last_error` is set, not only one whose request
      failed. A restart loop against a lying proxy costs a full re-download per
      poll and never raises a network error. `PollReport::fault` carries the
      committed error out of the commit, and the loop treats it exactly like an `Err`
- [x] Emit a `sync://status` Tauri event: idle / syncing / error, last success, rows added.
      `StatusSink` keeps `brewcore` free of `tauri`; `src-tauri/src/sync_events.rs` is
      the only file that knows the event name
- [x] Status footer showing per-source state and age of the newest reading
- [x] "Resync" (incremental) and "Rebuild from scratch" (offset → 0) commands. Rebuild
      goes through `RestartReason::Requested`, so it reuses 2e's delete-and-refetch
      path; it is the one restart reason that clears `last_error` instead of writing one
- [x] Sync never blocks a UI query — WAL readers must stay live during a write. Every
      database step of a poll runs on `spawn_blocking`, and a panic there becomes
      `Error::BackgroundTask` rather than a task that silently stops existing
- [x] `ReadingSource` is `dyn`-safe (`async-trait`) and lives in `sync/`, which imports
      nothing from `fermentrack`. v2's MQTT source is a new impl, not a new scheduler

**2h. Deferred**
- [ ] `_annotations.almost_json` → parked; annotations are already in `_full.csv`
- [ ] HTTP listener for `GenericPushTarget` — only if 60 s latency proves too slow

**Done when:** pointing the app at the fixture server and growing the file
produces a correct, gap-free, duplicate-free `reading` table across app restarts.

### Phase 3 — Batch & recipe core
**3a. Reference data**
- [ ] `vessel` (name, kind, capacity_l, active) — needed here, not in Phase 7
- [ ] `ingredient` (name, category, default_unit, attrs JSON). Categories:
      fermentable, sugar, yeast, nutrient, acid, salt, flavoring, fining, gas, water
- [ ] `yeast_strain` (name, lab, form, temp_min_c, temp_max_c, attenuation_min/max,
      alcohol_tolerance) — the finish detector needs the expected-FG band
- [ ] `ingredient_lot` (ingredient_id, supplier, lot_code, qty_g/qty_l, entry_qty,
      entry_unit, unit_cost_minor, currency, purchased_at, expires_at) — cost and
      traceability both start here. `_minor`, not `_cents`: 1c's linter rejects
      `_cents`, and the exponent belongs to the currency, not the column name
- [ ] `water_profile` — seed from `water-data/valenciennes/tap_brewing-params.csv`

**3b. Recipes, versioned properly**
- [ ] `recipe` (name, beverage_type, archived_at)
- [ ] `recipe_version` (recipe_id, version_no, target_volume_l, target_og, target_fg,
      target_abv, locked_at, notes) — UNIQUE (recipe_id, version_no)
- [ ] `recipe_item` (recipe_version_id, ingredient_id, qty, unit, stage,
      offset_hours, notes) — stage: prep, pitch, fermentation, conditioning, packaging
- [ ] **A version referenced by any batch is immutable.** Editing it forks the next
      version number. Enforce in the data layer, with a test
- [ ] "Fork to new version" and "diff two versions" in the UI
- [ ] Nutrient schedule as first-class `recipe_item` rows with `offset_hours` —
      for a sugar wash this is the process variable that decides the batch

**3c. Batches**
- [ ] `batch` (code, recipe_version_id, beverage_type, vessel_id, status,
      volume_l, og, fg, pitched_at, archived_at, notes)
- [ ] `batch.code` unique; next code auto-suggested per type (`SELTZER-018`)
- [ ] Status machine: planned → fermenting → conditioning → packaged → archived.
      Illegal transitions rejected in the data layer, not just greyed out in the UI
- [ ] `beverage_type` drives which optional side tables apply — no nullable
      beer-only columns on `batch`
- [ ] `batch_ingredient` (batch_id, ingredient_lot_id, qty, unit, stage, added_at)
      — the *actuals*, against `recipe_item`'s plan
- [ ] Planned-vs-actual view per batch
- [ ] Never hard-delete a batch; `archived_at` only

**3d. Timeline and stream binding**
- [ ] `batch_event` (batch_id, ts, kind, payload JSON, note)
- [ ] Event kinds: pitch, nutrient_addition, flavoring_addition, temp_ramp,
      cold_crash, transfer, fining, carbonation, package
- [ ] `pitch` is required to leave `planned` — it is t=0 for every later comparison
- [ ] `batch_stream` (batch_id, stream_id, starts_at, ends_at)
- [ ] Reject overlapping bindings of one stream to two batches (SQLite has no
      exclusion constraint — check on write, and test it)
- [ ] Import Fermentrack's `beer_ann` / `fridge_ann` cells as suggested events
- [ ] Timeline UI: events + manual measurements on one vertical list

**3e. Manual measurements**
- [ ] One `source` with `kind='manual'` per batch; its streams bound via `batch_stream`
- [ ] Quick-entry form: gravity, pH, temp, TA, taste note — two clicks from the batch
- [ ] Gravity input accepts SG, °P or Brix, converted to SG on entry
- [ ] Hydrometer temperature correction, with the calibration temp configurable
- [ ] Refractometer readings post-pitch need alcohol correction — flag them as
      refractometer so the correction is applied, and do not silently mix the two

**3f. Calc module (`calc/`, pure functions, heavily tested)**
- [ ] SG ↔ °P ↔ Brix
- [ ] Apparent attenuation `(OG − FG) / (OG − 1)`
- [ ] Real extract, real attenuation, ABW
- [ ] ABV: simple `(OG − FG) × 131.25` **and** the alternate high-attenuation
      formula — seltzer finishing at 1.000 is exactly where the simple one drifts.
      Show both, label which is which
- [ ] Refractometer correction (Terrill cubic + wort correction factor) —
      **verify the coefficients against the source when implementing**
- [ ] Sugar mass → OG for a sugar wash, computed from mass fraction rather than
      PPG, since seltzer base is effectively pure sucrose
- [ ] Dilution / blending: target OG from a volume of wash plus water
- [ ] Yeast pitch rate: cells from rate × volume × °P; dry-yeast grams
- [ ] YAN target and staggered-nutrient math for zero-nitrogen sugar bases
- [ ] Cost per batch and per litre from `batch_ingredient` × lot unit cost
- [ ] Every formula unit-tested against a known worked example with its source cited

**Done when:** a batch can be created from a recipe version, pitched, have manual
readings and events logged against it, and show a correct OG/FG/ABV and
planned-vs-actual — with the Fermentrack curve already attached via `batch_stream`.

### Phase 4 — Dashboard & curves
- [ ] Batch detail view: the ARCHITECTURE.md status card (OG, current SG, est. ABV, pH, temp)
- [ ] Fermentation curve chart, multi-axis (SG / temp / pH), event markers
- [ ] Downsampling for long series so the chart stays interactive
- [ ] "Fermentation stable for Nh" detector
- [ ] Home view: active batches, last reading age, stale-data warning

### Phase 5 — Analytics
- [ ] Batch-to-batch comparison (overlay curves, time-aligned from pitch)
- [ ] Saved queries: "all batches, yeast X, OG 1.035–1.040, ferm temp 18°C"
- [ ] Derived per-batch metrics table (lag time, peak rate, days to terminal)
- [ ] Yeast/strain performance rollup
- [ ] CSV export of any result set

### Phase 6 — Alerts & automation rules
- [ ] Rule engine: condition on latest readings → action
- [ ] Desktop notifications
- [ ] Rule: fermentation finished (SG slope flat for N hours)
- [ ] Rule: temp out of band, sensor stale, no data
- [ ] Optional write-path: `setBeer=`/`setFridge=`/`setActiveProfile=` over the
      brewpi-script socket, behind a confirm; mirror it in our own records because
      Fermentrack's DB won't see it
- [ ] Rules evaluated on ingest, not on a UI timer

### Phase 7 — Production records
- [ ] Cleaning / sanitation log, per vessel
- [ ] Packaging records (volume, format, date, loss)
- [ ] Cost per batch from ingredient lots
- [ ] Ingredient inventory with lot consumption
- [ ] Batch report export (PDF or printable HTML)

### Cross-cutting
- [ ] `brewery.db` backup command (file copy + integrity check)
- [ ] Schema migration test: old db → new version, no data loss
- [ ] All timestamps stored UTC epoch seconds, formatted at the edge
- [ ] Keep every Fermentrack call inside one `fermentrack` module — the seam for Option 2

---

## Stack

| Layer | Choice | Reason |
|---|---|---|
| Shell | Tauri v2 | one binary, no server, Rust side owns the I/O |
| Backend | Rust | already the Tauri side; v2's controller lands in the same crate |
| DB | SQLite via `rusqlite` (`bundled`) | one file, offline, full SQL |
| Pool | `r2d2_sqlite` | WAL readers while the sync task writes |
| Migrations | versioned `.sql` + `PRAGMA user_version` | no framework lock-in |
| Async | `tokio` | Tauri v2 already runs on it |
| HTTP | `reqwest` | CSV sync + JSON API; `Range` support |
| CSV | `csv` | header-driven parsing, correct quoting for `beer_ann` |
| Time | `chrono` | parse `%Y/%m/%d %H:%M:%SZ`, stay in UTC |
| Errors | `thiserror` | core errors; mapped at the Tauri boundary |
| Logs | `tracing` + `tracing-appender` | rolling file, structured |
| Frontend | Svelte 5 + TypeScript | small, no virtual-DOM overhead on live charts |
| Charts | `uPlot` | handles 100k+ points; ECharts is heavier than this needs |
| Test | `cargo test` + `wiremock` | deterministic ingest tests, no live Fermentrack |

Frontend framework is the one genuinely swappable choice — React works if you
prefer it. Everything else follows from ARCHITECTURE.md.

### Crate layout

```text
brew-app/
├── crates/
│   └── brewcore/          # no tauri dependency — plain `cargo test`
│       ├── src/
│       │   ├── db/        # connection, migrations, queries
│       │   ├── fermentrack/  # client.rs, csv_log.rs, sync.rs, types.rs
│       │   ├── calc/      # ABV, attenuation, stability
│       │   ├── units/           # temperature, gravity, quantity, money, time
│       │   ├── config.rs
│       │   └── error.rs
│       └── migrations/    # 0001_tier1.sql, 0002_batches.sql, …
├── src-tauri/             # commands, app state, window setup. Thin.
├── src/                   # Svelte
└── xtask/                 # fixture generator, dev helpers
```

The workspace split is the load-bearing decision. Everything interesting is a
library that a test can drive without a window, and v2's controller is a new
module in `brewcore`, not a new project.

---

## Phase 1 in detail

### Connections and pragmas

Pragmas are per-connection, not per-database — `journal_mode` persists in the
file, the rest do not. Apply them in one place that every connection goes
through, or you will get `SQLITE_BUSY` and silently-ignored foreign keys.

```rust
fn apply_pragmas(conn: &Connection) -> rusqlite::Result<String> {
    // `PRAGMA journal_mode` RETURNS A ROW, so it cannot go through
    // `pragma_update` — that uses `execute` and fails with
    // `ExecuteReturnedResults`. Query it and check what you actually got.
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?; // per-connection
    conn.pragma_update(None, "foreign_keys", "ON")?;    // per-connection, OFF by default
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(mode)  // only `journal_mode` persists in the file
}
```

Neither `journal_mode` nor `foreign_keys` can be changed inside a transaction — the
latter silently does nothing — so this runs immediately on open, before migrations.

WAL is what lets a chart query run while the sync task is inserting. One writer
at a time is still the rule — funnel writes through a single owned connection
rather than letting every caller open one and fight over the lock.

### Migrations

```rust
const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("../migrations/0001_tier1.sql")),
    (2, include_str!("../migrations/0002_batches.sql")),
];
```

Read `PRAGMA user_version`, apply every migration above it, each inside a
transaction that also bumps `user_version`. If the process dies mid-migration the
file is either at N or N+1, never between.

Rules: migrations are append-only, never edited once committed; no migration
depends on application code, because the code will have moved on by the time it
runs against an old database. Test both empty→head and every vN→head.

### Units, decided once

Fermentrack hands us Fahrenheit or Celsius per row, and gravity gets discussed in
SG, Plato and Brix depending on whether the conversation is about seltzer, beer or
wine. Multi-beverage scope makes that worse, not better. So: **store canonical,
convert at the edges.**

| Quantity | Stored as | Notes |
|---|---|---|
| Temperature | °C, `REAL` | convert on ingest using the row's `temp_format` |
| Gravity | SG, `REAL` | Plato/Brix are input and display formats only |
| Volume | litres, `REAL` | |
| Mass | grams, `REAL` | |
| Time | UTC epoch seconds, `INTEGER` | local time exists only in the UI |
| Money | integer minor units + ISO 4217 code | never float; mixing currencies is an error, and there is no FX |

One `units` module owns every conversion, with round-trip tests. Display
preference is config, not storage.

**Canonical column plus entry unit.** Where a table records something bought or
weighed — `ingredient_lot`, `recipe_item`, `batch_ingredient` — a bare `qty` +
`unit` pair makes `SUM(qty)` meaningless. Those tables store `qty_g` *or* `qty_l`
(by the ingredient's dimension) **plus** `entry_qty REAL` and `entry_unit TEXT`.
Every query, sum and cost reads the canonical column; the UI prints the unit you
typed. Settled in 1c so 3a does not re-litigate it.

**Definitional here, empirical in `calc`.** `units/` holds conversions exact by
definition (°C↔°F, g↔lb, L↔US gal, minor units↔decimal), which is why its tests
assert near-machine-epsilon tolerances. Polynomial fits of measured data — SG↔Plato,
refractometer correction, ABV — live in `calc/` with a citation and an error band. A
conversion that needs a literature reference is not a unit conversion.

### The Tauri boundary

`brewcore` returns `thiserror` enums. Tauri commands must return something
`Serialize`, so the boundary converts to `{ code, message, detail }` — a stable
`code` the frontend can branch on, a message for humans, `detail` for the log.
Doing this from day one stops error handling leaking `tauri::Error` into core.

---

## Data model sketch

Two tiers, deliberately separated.

**Tier 1 — derived from Fermentrack.** Cheap to throw away and re-sync.

```sql
CREATE TABLE source (
  id          INTEGER PRIMARY KEY,
  kind        TEXT NOT NULL,          -- 'fermentrack_beer_log' | 'fermentrack_gravity_log' | 'manual'
  name        TEXT NOT NULL,
  config      TEXT                    -- JSON: base_url, device_id, beer_id/log_id
);

CREATE TABLE stream (
  id          INTEGER PRIMARY KEY,
  source_id   INTEGER NOT NULL REFERENCES source(id),
  external_id TEXT NOT NULL,          -- opaque: e.g. 'B17.beer_temp'
  metric      TEXT NOT NULL,          -- 'temp_c' | 'sg' | 'ph' | 'setpoint_c' | 'state'
  label       TEXT,
  UNIQUE (source_id, external_id, metric)
);

CREATE TABLE reading (
  stream_id   INTEGER NOT NULL REFERENCES stream(id),
  ts          INTEGER NOT NULL,       -- UTC epoch seconds
  value       REAL NOT NULL,
  PRIMARY KEY (stream_id, ts)
) WITHOUT ROWID;

-- One row per remote CSV file, not per stream: one file feeds several streams.
CREATE TABLE sync_state (
  source_id   INTEGER PRIMARY KEY REFERENCES source(id),
  remote_path TEXT NOT NULL,          -- '/data/Device 1 - B17 - Seltzer 17_full.csv'
  byte_offset INTEGER NOT NULL,       -- resume point for Range requests
  header      TEXT,                   -- header line, no terminator; compared at offset 0
  last_ts     INTEGER,                -- newest row seen, for display
  last_error  TEXT,                   -- surfaced in the status footer
  synced_at   INTEGER NOT NULL
);
```

`WITHOUT ROWID` plus the composite PK makes `WHERE stream_id = ? AND ts BETWEEN ?
AND ?` a clustered range scan — the query every chart runs.

**Tier 2 — yours only.** Not reconstructible from Fermentrack; this is what gets
backed up and what the project exists for.

```text
recipe ──< recipe_version ──< recipe_item >── ingredient
                  │
                  └──< batch ──< batch_ingredient >── ingredient_lot
                         ├──< batch_event      (pitch, addition, ramp, crash, package)
                         ├──< batch_stream     (stream_id, starts_at, ends_at)
                         ├──< note
                         ├──< cleaning_record
                         └──< packaging_record
```

`batch_stream` is the join that makes everything else work: it maps a physical
sensor to a batch for a time window, so the same fermenter can host batch 17 and
then batch 18 with the curves staying separate.

Fuller Tier 2 shape, plus the reasoning behind it, in *Phase 3 in detail* below.

---

## Phase 2 in detail

Fermentrack's log files are append-only CSV (see
[FERMENTRACK-API.md](FERMENTRACK-API.md)), which makes sync a byte-offset resume
rather than a paginated query.

### Build the fixture generator first

You have no Fermentrack instance, and waiting for one would block the only two
phases that matter right now. It isn't needed: the CSV format is fully specified
in FERMENTRACK-API.md and the repo is dormant, so a generator can produce
byte-accurate files today.

`xtask gen-fixture` should emit, deliberately including the ugly cases:

- header + rows at a 30 s cadence, `%Y/%m/%d %H:%M:%SZ`, UTC
- a plausible curve: lag, fast attenuation, terminal plateau
- a multi-hour gap (Fermentrack was down)
- a bare `0` where a sensor was absent (Fermentrack's `x or 0`, not a blank cell)
- an annotation containing a comma and one containing a quote
- a `temp_format` that switches `C`→`F` mid-file
- both `gravity_enabled` on and off, both `model_version` 1 and 2 — the latter
  changes `_graph.csv` only, and generating v1 is how we prove that

Serve it with `wiremock` in two modes: honouring `Range`, and ignoring it
(returning `200` with the whole body). Both happen in the wild and the client
must handle each. The growing-file test — serve N bytes, sync, serve N+M, sync
again, assert no duplicates and no gaps — is the single most valuable test in the
project. Swap in a real capture later; the tests don't change.

### Column → stream mapping

One CSV row fans out to up to eight `reading` rows:

| CSV column | metric | conversion |
|---|---|---|
| `beer_temp` | `temp_c` | per-row `temp_format` |
| `beer_set` | `setpoint_c` | per-row `temp_format` |
| `fridge_temp` | `fridge_temp_c` | per-row `temp_format` |
| `fridge_set` | `fridge_setpoint_c` | per-row `temp_format` |
| `room_temp` | `room_temp_c` | per-row `temp_format` |
| `gravity` | `sg` | none |
| `grav_temp` | `gravity_temp_c` | per-row `temp_format` |
| `state` | `state` | enum 0–9, stored as its integer |
| `beer_ann`, `fridge_ann` | — | annotation, not a reading |

Map by header name. The column set varies with `gravity_enabled`, and the gravity
columns sit after `associated_beer_id` in `_full.csv` but before `state` in
`_graph.csv`, so positional indexing will eventually read gravity out of the
`state` column and put 4.0 in your SG series.

### The sync loop

Per source, on launch and then on a timer:

1. Re-resolve `remote_path` — the beer name is in the filename, so a rename in
   Fermentrack moves the file.
2. `GET` it with `Range: bytes=<byte_offset>-`. `remote_path` is held decoded and
   percent-encoded here, at the one boundary that builds the URL.
   - `206 Partial Content` → parse the body as the continuation, but only if its
     `Content-Range` starts at `byte_offset`. A `206` that starts anywhere else is
     refused rather than trusted: its rows would be stored as though they began at
     an offset they didn't, and that is the one corruption step 8 can never detect.
   - `200 OK` → the server ignored `Range`; skip `byte_offset` bytes locally. A body
     exactly `byte_offset` long is a no-op; a *shorter* one is truncation — and the
     likelier of the two truncation cases, since it is what a Range-ignoring proxy
     in front of a rotated log produces.
   - `416` → look at `Content-Range: bytes */N`. `N == byte_offset` means the file
     has not grown since the last sync: a no-op, and what every idle poll returns
     once the backfill is done. Only `N < byte_offset`, or a missing
     `Content-Range`, means truncated or recreated — reset to 0 and refetch whole.
     `N > byte_offset` is neither, and is an error: resetting to 0 would ask for a
     range the server has already called unsatisfiable and get the same `416`
     forever, so it wedges visibly instead.
   - A `206` whose `Content-Range` total is smaller than `byte_offset` → the same
     truncation, seen from the other side; reset to 0.
   - `404` → renamed or deleted. Record it in `last_error`, keep the source and
     its existing readings, surface it in the UI. Do not silently stop.
   - HTML body or non-2xx → error carrying the first 200 bytes, so a misdirected
     URL is obvious in the log. Fermentrack's dashboard answers **200**, so the
     content check has to run *before* any of the offset arithmetic above: a
     400-byte page fetched at a 50 000 offset is shorter than that offset, would be
     classified as truncation, would reset `byte_offset` to 0, and the next poll
     would ingest the page. Ordering, not the check itself, is what prevents that.
   - A failed request is retried up to three times inside the fetch, 250 ms then
     1 s, for timeouts, connection failures and 5xx only — a wrong URL is wrong
     three times too. That is a different mechanism from the per-source backoff
     under *Scheduling*, which runs in minutes and sits between polls.
3. Split complete records off the chunk and parse only those. The partial tail is
   discarded, not buffered — the offset stops at the last terminator, so the next
   `Range` asks for those bytes again.
4. At offset 0, lift the header line off the front and compare it against
   `sync_state.header`; on a resume, check the first record parses as exactly one
   row and restart from 0 if it doesn't. That check is what catches a recreated
   file, and reaching 0 is what then reveals the header change.
5. Convert each row's temps using that row's own `temp_format` cell.
6. Fan out to `reading` rows, auto-creating `stream` rows on first sight.
7. `INSERT ... ON CONFLICT(stream_id, ts) DO NOTHING`, batched through one
   prepared statement.
8. Commit **readings, `byte_offset`, `header` and `last_ts` in one transaction**.
   That single boundary is what makes a crash mid-sync safe: the offset can never
   claim bytes whose rows didn't land.
9. Leave any trailing partial line where it is — Fermentrack was mid-write, and the
   offset committed in step 8 already points at its first byte.

If the parser stopped early (`ParseReport::stopped_early`), step 8 commits the rows
that parsed and advances `byte_offset` by `bytes_consumed` — the last record that
read cleanly — and nothing further. The next poll refetches the faulty bytes and
fails identically, so the source stops with an error in the footer. That is
deliberate: a sync that skipped the bad record would leave a hole in `reading` that
nothing downstream could ever detect.

App closed for a week means step 2 fetches a week of bytes in one request. That is
exactly why Fermentrack stays the logger of record.

### Discovery

No API enumerates beers or gravity logs, and the log id *and the beer name* are
both in the filename, so there is no way to guess or construct a path — which
rules out the "device id + beer id" alternative this section used to offer. v1:
the user pastes the log URL once when creating a source, and we validate it by
fetching the first 2 KB and parsing the header. If that gets tedious, add a helper
that scrapes `/beer/list/` and `/gravity/logs/` into a picklist. `GET /api/devices/`
is worth one call purely to confirm the host is a Fermentrack.

### Scheduling

A tokio task per source: sync on launch, then every `poll_interval_secs` (default
60, jittered). Exponential backoff to a ceiling on repeated failure, so a
Fermentrack that is off for a weekend doesn't hammer the network or spam the log.
Emit `sync://status` events — idle / syncing / error, last success, rows added —
and render them in a status footer. Sync must never block a UI query; that's what
WAL buys.

Two explicit commands: **Resync** (incremental, now) and **Rebuild** (offset → 0,
re-derive everything). Rebuild is the escape hatch when a parser bug has written
bad rows, and it's only safe because Tier 1 is derived data.

### Invariants

Always read `_full.csv`, never `_graph.csv`: the graph file substitutes `0` for
missing readings, which would land in the database as a real 0.0 °C and quietly
poison every average you ever compute.

Never write Tier 1 data outside the sync module, and never let a UI action delete
readings. Manual measurements go to a `source` row with `kind='manual'` so they
are visibly not Fermentrack's.

---

## Phase 3 in detail

This is the phase the project exists for. Tier 1 is replaceable; everything here
is typed in once and never recoverable. Three decisions carry the weight.

### 1. Recipe versions are immutable once used

```sql
CREATE TABLE recipe_version (
  id          INTEGER PRIMARY KEY,
  recipe_id   INTEGER NOT NULL REFERENCES recipe(id),
  version_no  INTEGER NOT NULL,
  locked_at   INTEGER,              -- set the first time a batch references it
  ...
  UNIQUE (recipe_id, version_no)
);
```

The moment a batch points at a version, that version is frozen; edits fork
`version_no + 1`. Without this, "show me every batch of Citron-Verveine v3" is a
lie — v3 would mean whatever the recipe happened to say when you last opened it,
and the twenty batches you were going to compare would be comparing nothing.

It also removes the need to snapshot the recipe onto the batch. A frozen version
*is* the snapshot, and it stays deduplicated and diffable.

### 2. Plan and actual are different tables

`recipe_item` is what you meant to do. `batch_ingredient` is what you actually
put in, which lot it came from, and when. They diverge constantly — a lot runs
out, a nutrient addition slips six hours, the volume comes in short.

Recording only the plan makes the log useless for diagnosis; recording only the
actuals makes the recipe unrepeatable. Keep both and diff them. `ingredient_lot`
on the actual side is also what makes cost-per-batch and "which batches used that
suspect yeast lot" answerable at all.

### 3. Events are not readings

A reading is a number on a time series; an event is something you *did*. They
share a timeline in the UI and nothing else.

`batch_event(batch_id, ts, kind, payload JSON, note)` — the JSON payload keeps
kind-specific fields out of the schema, since a `temp_ramp` and a `package` have
nothing in common. Keep `kind` a closed set in code so the analytics in Phase 5
can rely on it.

`pitch` is special: it is t=0. Batch-to-batch overlay in Phase 5 aligns on it, the
finish detector measures from it, and lag time is derived from it. Require it to
leave `planned` rather than discovering later that half the batches can't be
compared.

### Tier 2 shape after this phase

```text
vessel ──< batch
yeast_strain ──< ingredient
ingredient ──< ingredient_lot ──< batch_ingredient >── batch
     │                                                   │
     └──< recipe_item >── recipe_version ──< batch ──────┘
                │                             ├──< batch_event
           recipe                             ├──< batch_stream >── stream
                                              └──< note
```

### Seltzer-specific, because it's what you actually brew

A sugar wash has no nitrogen, so yeast health is entirely a nutrient-schedule
problem, and the schedule is timed relative to pitch. Model it as ordinary
`recipe_item` rows with `offset_hours` rather than a free-text note — it is the
variable most likely to explain a batch going wrong, and it needs to be queryable
in Phase 5.

Sugar-wash OG is also worth computing from mass fraction rather than borrowing
beer's PPG tables: the base is effectively pure sucrose, so `°P = sugar / (sugar
+ water)` by mass is exact, and PPG is an approximation of an approximation.

And at FG ≈ 1.000 the `(OG − FG) × 131.25` shortcut drifts noticeably. Implement
the alternate formula alongside it and label which number is on screen.

### Multi-beverage without a nullable mess

`batch.beverage_type` decides which optional side tables apply. Beer gets a
`batch_mash` / `batch_boil` row; cider and wine get their own. What it must not
become is thirty nullable columns on `batch` where twenty-five are always NULL
for seltzer. The core `batch` row holds only what every fermentable has: code,
recipe version, vessel, status, volume, OG, FG, timestamps.

---

## Fermentation-finished detection

Two signals, both required, so a stall does not read as a finish:

- Gravity slope over a rolling window (proposal: 24h) below a threshold
  (proposal: 0.0005 SG/day), **and**
- Current SG within a plausible band of the recipe version's expected FG.

Make both thresholds config, not constants — the first ten batches will tell you
the real numbers. Log every evaluation so you can tune against history instead of
guessing.

---

## Keeping the Option 2 door open

ARCHITECTURE.md's migration is `Your App → Fermentrack → …` becoming `Your App →
Your Controller → …`. That stays cheap only if:

- Every Fermentrack HTTP call and CSV parse lives in one `fermentrack` module
  behind a trait.
- Nothing above the data layer knows what a Fermentrack device id looks like —
  `stream.external_id` is opaque everywhere else.
- Control actions (setpoint, profile) go through one `Controller` trait from day
  one, even while the only impl is the brewpi-script socket.
- Tier 2 tables never reference a Fermentrack id directly.

Do those four and Option 2 is a new impl of two traits, not a rewrite.

Concretely, v2 is: a `ReadingSource` impl that subscribes to MQTT topics from the
ESP32s instead of parsing CSV, and a `Controller` impl that publishes setpoints
instead of writing to the brewpi-script socket. Both land in `brewcore` next to
the ones they replace. Tier 1's `source`/`stream`/`reading` schema doesn't change
— an MQTT topic is just another opaque `external_id`. No MQTT crate enters the
dependency list until then.

Worth knowing while planning: master's last commit is 2023-09-04 and Fermentrack 2
is unreleased. The CSV format is therefore stable to depend on, and Option 2 is a
question of when rather than if.

---

## Order of real work

Phase 0 → 1 → 2 gets an app that shows a live fermentation curve and survives
being closed. That is where the project proves itself. Do not start Phase 3
before Phase 2's gap handling actually works — every later feature assumes the
reading table is complete.

Phase 0's remaining items need a running Fermentrack, which doesn't exist yet.
That blocks nothing: Phase 1 never touches the network, and Phase 2 is built and
tested against generated fixtures. Standing Fermentrack up becomes a validation
step at the end of Phase 2 — point the finished sync at a real box and confirm
the fixtures were honest — rather than a prerequisite at the start.

Phases 3–5 are the value. Phases 6–7 are worth deferring until enough batches
have run to know what you actually want alerted and recorded.
