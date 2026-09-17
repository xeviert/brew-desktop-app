# Buildout Plan — Option 1 (App on top of Fermentrack)

Target: a Tauri desktop app that reads Fermentrack's data, enriches it with batch /
recipe / production records in a local SQLite file, and becomes the brewery
operating system. Fermentrack stays the fermentation-control appliance and the
logger of record. See [../ARCHITECTURE.md](../ARCHITECTURE.md) for the why.

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
- [ ] Confirm against your own box: `curl http://<host>/api/devices/` and list `/data/`
- [ ] Capture one real `_full.csv` as a test fixture
- [ ] Lock the frontend stack (proposal below)

### Phase 1 — Skeleton
- [ ] `cargo create-tauri-app`, Tauri v2, TypeScript frontend
- [ ] `brewery.db` path via Tauri app-data dir, overridable by env var
- [ ] Migration runner (versioned `.sql` + `PRAGMA user_version`)
- [ ] Data layer module: hand-written SQL, no ORM
- [ ] Config file (Fermentrack base URL; no credentials — there is no auth)
- [ ] Logging (`tracing`) to file + console
- [ ] CI: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, frontend typecheck

### Phase 2 — Ingest
- [ ] Schema: `source`, `stream`, `reading`, `sync_state` (+ `byte_offset`)
- [ ] `_full.csv` parser: read the header row, map columns by name, never by position
- [ ] Per-row temp conversion using that row's own `temp_format`
- [ ] Reject `_graph.csv` — it writes `0` for missing readings
- [ ] Incremental fetch: `Range: bytes=N-`, append, advance offset; full refetch if it shrank
- [ ] Idempotent upsert keyed on `(stream_id, ts)`
- [ ] Handle `Beer.model_version` 1 vs 2 (v1 has no `state` column)
- [ ] Re-resolve filenames each sync — a rename in Fermentrack moves the file
- [ ] Poll loop while the app is open (interval ≈ `data_point_log_interval`)
- [ ] Integration test against a captured `_full.csv` served by `wiremock`
- [ ] Manual "Resync" command in the UI
- [ ] *(optional, later)* HTTP listener for `GenericPushTarget` pushes

### Phase 3 — Batch & recipe core
- [ ] Schema: `recipe`, `recipe_version`, `ingredient`, `ingredient_lot`,
      `batch`, `batch_ingredient`, `batch_event`, `note`
- [ ] Bind a batch to a stream for a time window (`batch_stream`)
- [ ] CRUD UI: create batch from recipe version, edit, close
- [ ] Manual measurement entry (gravity, pH, temp) → `reading` with `source.kind='manual'`
- [ ] Calc module: OG/FG/ABV, apparent + real attenuation, unit tests with known values
- [ ] Event timeline: pitch, nutrient adds, flavoring, ramp, cold crash, package

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
| Shell | Tauri v2 | one binary, no server, Rust side owns MQTT/HTTP |
| Backend | Rust | already the Tauri side; controller logic lands here later |
| DB | SQLite via `rusqlite` (bundled) | one file, offline, full SQL |
| Migrations | versioned `.sql` + `PRAGMA user_version` | no framework lock-in |
| HTTP | `reqwest` | CSV sync + JSON API; supports `Range` |
| CSV | `csv` crate | header-driven parsing of `_full.csv` |
| Control | `std::net` / `tokio::net` | raw socket to brewpi-script, cp437 |
| Frontend | Svelte 5 + TypeScript | small, no virtual-DOM overhead on live charts |
| Charts | `uPlot` | handles 100k+ points; ECharts is heavier than this needs |
| Test | `cargo test`, `wiremock` for API fixtures | deterministic ingest tests |

Frontend framework is the one genuinely swappable choice — React works if you
prefer it. Everything else follows from ARCHITECTURE.md.

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
  last_ts     INTEGER,                -- newest row seen, for display
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

---

## Sync design

Fermentrack's log files are append-only CSV (see
[FERMENTRACK-API.md](FERMENTRACK-API.md)), which makes sync a byte-offset resume
rather than a paginated query.

Per source, on launch and then on a timer:

1. Re-resolve `remote_path` — the beer name is in the filename, so a rename in
   Fermentrack moves the file.
2. `GET` it with `Range: bytes=<byte_offset>-`.
   - `206 Partial Content` → parse the new bytes.
   - `416` or a `Content-Length` smaller than `byte_offset` → file was truncated
     or recreated; refetch whole from 0 and re-derive.
   - `404` → the beer was renamed or deleted; surface it, don't silently stop.
3. Parse rows against the header captured on the first full fetch. Map columns by
   name — the column count varies with `gravity_enabled` and `model_version`.
4. Convert each row's temps using that row's own `temp_format` value.
5. Fan one CSV row out to several `reading` rows (beer_temp, beer_set,
   fridge_temp, fridge_set, room_temp, gravity, grav_temp, state).
6. Upsert with `ON CONFLICT(stream_id, ts) DO NOTHING` — reruns are free.
7. Advance `byte_offset` only to the last **complete** line committed. A trailing
   partial line means Fermentrack was mid-write; drop it and pick it up next pass.

App closed for a week means step 2 fetches a week of bytes in one request. That is
exactly why Fermentrack stays the logger of record.

Always read `_full.csv`, never `_graph.csv`: the graph file substitutes `0` for
missing readings, which would land in our database as a real 0.0 °C.

Never write Tier 1 data outside the sync module, and never let a UI action delete
readings. Manual measurements go to a `source` row with `kind='manual'` so they
are visibly not Fermentrack's.

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

Worth knowing while planning: master's last commit is 2023-09-04 and Fermentrack 2
is unreleased. The CSV format is therefore stable to depend on, and Option 2 is a
question of when rather than if.

---

## Order of real work

Phase 0 → 1 → 2 gets an app that shows a live fermentation curve and survives
being closed. That is where the project proves itself. Do not start Phase 3
before Phase 2's gap handling actually works — every later feature assumes the
reading table is complete.

Phases 3–5 are the value. Phases 6–7 are worth deferring until enough batches
have run to know what you actually want alerted and recorded.
