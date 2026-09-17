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
- [ ] Timestamps: UTC epoch **seconds**, `INTEGER`, everywhere. Format only at the UI edge
- [ ] Temps stored °C. Gravity stored as SG (`1.0374`), never Plato/Brix — convert on entry
- [ ] Volumes stored litres, masses grams
- [ ] Money stored integer cents + an ISO currency code, never float
- [ ] A `units` module owning every conversion, with round-trip unit tests
- [ ] No table or column in our schema named "beer" (see Scope above)

**1d. Plumbing**
- [ ] Config: `serde` + TOML in Tauri's config dir. Fields: `fermentrack.base_url`,
      `sync.poll_interval_secs`, `ui.temp_display` (`C`/`F`), `ui.gravity_display`
- [ ] Config has working defaults; a missing file is not an error
- [ ] `error.rs`: `thiserror` enum in core, converted at the Tauri boundary to a
      serializable `{code, message, detail}` — Tauri commands can't return opaque errors
- [ ] `tracing` + `tracing-subscriber`, rolling file in Tauri's log dir + stderr in dev
- [ ] Tauri command surface for this phase only: `get_config`, `set_config`,
      `db_status` (path, size, schema version, row counts)
- [ ] Frontend: one route, a settings pane, a status footer. No chart yet
- [ ] CI on `windows-latest`: `cargo fmt --check`, `cargo clippy -- -D warnings`,
      `cargo test --workspace`, `npm run check`

**Done when:** the app launches, creates `brewery.db` at the right path, reports
its schema version in the UI, and `cargo test --workspace` is green in CI.

### Phase 2 — Ingest
**2a. Test rig first — you have no Fermentrack**
- [ ] `xtask gen-fixture`: emit a byte-accurate `_full.csv` (header + rows,
      `%Y/%m/%d %H:%M:%SZ`, plausible ferment curve, gaps, blank cells, an
      annotation containing a comma, a mid-file `temp_format` switch)
- [ ] Emit both variants: `gravity_enabled` on/off, `model_version` 1 and 2
- [ ] `wiremock` harness that honours `Range` and can be told to *ignore* it
- [ ] Growing-file test: serve N bytes, sync, serve N+M, sync again, assert no dupes
- [ ] Replace fixtures with a real capture once Fermentrack is up — same tests

**2b. Parser (`fermentrack/csv_log.rs`)**
- [ ] Parse the header row into a column map; unknown columns ignored, not fatal
- [ ] Reject a file whose header lacks `log_time` — wrong file or HTML error page
- [ ] `_full.csv` only. Refuse `_graph.csv` explicitly with a clear error
- [ ] Empty cell → `None`, never `0`
- [ ] Per-row temp conversion driven by that row's own `temp_format` cell
- [ ] `state` parsed to the 0–9 enum; out-of-range → `None` + a warning, not a panic
- [ ] `model_version` 1 (no `state`) handled by column presence, not a version check
- [ ] Unit tests per quirk, each with a 3-line inline CSV

**2c. Fetch (`fermentrack/client.rs`)**
- [ ] `GET` with `Range: bytes=<offset>-`
- [ ] `206` → parse body. `200` → server ignored Range; skip `offset` bytes locally
- [ ] `416`, or `Content-Range` total < `offset` → truncated/recreated; refetch from 0
- [ ] `404` → surface "log missing or renamed", keep the source, don't wipe readings
- [ ] Non-2xx/HTML body → error with the first 200 bytes in the message
- [ ] Timeouts + bounded retry with backoff; a down Fermentrack must not hang the UI

**2d. Offsets and partial lines**
- [ ] Keep a remainder buffer; a fetch ending mid-line carries the tail forward
- [ ] Advance `byte_offset` only past fully-parsed, committed lines
- [ ] Store the header text in `sync_state`; a changed header forces a full refetch
- [ ] Verify the first fetch of a resume starts on a line boundary; if not, refetch whole

**2e. Write path**
- [ ] Row fan-out: one CSV row → up to 8 `reading` rows, one per metric column
- [ ] Auto-create `stream` rows on first sight of a column, inside the same txn
- [ ] `INSERT ... ON CONFLICT(stream_id, ts) DO NOTHING`; count and log ignores
- [ ] One transaction per fetched chunk, covering readings + offset + header. Crash
      mid-sync must leave the offset consistent with the rows
- [ ] Batch inserts via a prepared statement; do not commit per row

**2f. Discovery — no API lists the logs**
- [ ] v1: user pastes the log URL (or device id + beer id) once when creating a source
- [ ] Validate on save by fetching the first 2 KB and parsing the header
- [ ] Optional helper: scrape `/beer/list/` and `/gravity/logs/` to offer a picklist
- [ ] `GET /api/devices/` only to confirm the host is a Fermentrack at all

**2g. Scheduler and UI**
- [ ] Tokio task per source: sync on launch, then every `poll_interval_secs` (default 60)
- [ ] Jitter the interval; exponential backoff to a ceiling on repeated failure
- [ ] Emit a `sync://status` Tauri event: idle / syncing / error, last success, rows added
- [ ] Status footer showing per-source state and age of the newest reading
- [ ] "Resync" (incremental) and "Rebuild from scratch" (offset → 0) commands
- [ ] Sync never blocks a UI query — WAL readers must stay live during a write

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
- [ ] `ingredient_lot` (ingredient_id, supplier, lot_code, qty, unit, unit_cost_cents,
      currency, purchased_at, expires_at) — cost and traceability both start here
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
│       │   ├── units.rs
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
| Money | integer cents + ISO code | never float |

One `units` module owns every conversion, with round-trip tests. Display
preference is config, not storage.

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
  header      TEXT,                   -- header line as fetched; a change forces refetch
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
- blank cells where a sensor was absent
- an annotation containing a comma and one containing a quote
- a `temp_format` that switches `C`→`F` mid-file
- both `gravity_enabled` on and off, both `model_version` 1 and 2

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

Map by header name. The column set varies with `gravity_enabled` and
`model_version`, so positional indexing will eventually read gravity out of the
`state` column and put 4.0 in your SG series.

### The sync loop

Per source, on launch and then on a timer:

1. Re-resolve `remote_path` — the beer name is in the filename, so a rename in
   Fermentrack moves the file.
2. `GET` it with `Range: bytes=<byte_offset>-`.
   - `206 Partial Content` → parse the body as the continuation.
   - `200 OK` → the server ignored `Range`; skip `byte_offset` bytes locally.
   - `416`, or a `Content-Range` total smaller than `byte_offset` → truncated or
     recreated; reset to 0 and refetch whole.
   - `404` → renamed or deleted. Record it in `last_error`, keep the source and
     its existing readings, surface it in the UI. Do not silently stop.
   - HTML body or non-2xx → error carrying the first 200 bytes, so a misdirected
     URL is obvious in the log.
3. Prepend the remainder buffer from last time. Parse complete lines only.
4. Compare the header against `sync_state.header`; any change → full refetch.
5. Convert each row's temps using that row's own `temp_format` cell.
6. Fan out to `reading` rows, auto-creating `stream` rows on first sight.
7. `INSERT ... ON CONFLICT(stream_id, ts) DO NOTHING`, batched through one
   prepared statement.
8. Commit **readings, `byte_offset`, `header` and `last_ts` in one transaction**.
   That single boundary is what makes a crash mid-sync safe: the offset can never
   claim bytes whose rows didn't land.
9. Carry any trailing partial line forward — Fermentrack was mid-write.

App closed for a week means step 2 fetches a week of bytes in one request. That is
exactly why Fermentrack stays the logger of record.

### Discovery

No API enumerates beers or gravity logs, and the log id is in the filename, so
there is no way to guess a path. v1: the user pastes the log URL (or device id +
beer id) once when creating a source, and we validate it by fetching the first
2 KB and parsing the header. If that gets tedious, add a helper that scrapes
`/beer/list/` and `/gravity/logs/` into a picklist. `GET /api/devices/` is worth
one call purely to confirm the host is a Fermentrack.

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
