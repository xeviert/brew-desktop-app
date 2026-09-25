# Buildout Plan — v2 (own controller over MQTT, via a Raspberry Pi)

> **Direction change, 2026-09-25: straight to v2.** Phases 0–3 were built for
> Option 1 (the app on top of Fermentrack). Hardware for fermentation tracking
> isn't all here yet, so the plan now skips Fermentrack and goes straight to v2:
> ESP32s and an iSpindle publish over MQTT to an always-on Raspberry Pi, and the
> desktop app syncs from the Pi. What that changes, and why, is in
> [ARCHITECTURE.md — "Decision: straight to v2"](../ARCHITECTURE.md#decision-straight-to-v2).
> The new work is **Phase V**, between Phase 3 and Phase 4. It is lettered, not
> numbered, because code comments and frozen migrations already cite Phases 4–7
> by number. The `fermentrack` module stays in the tree, tested and unused: part
> of it moves into a shared module in Va, and the rest is the fallback if v2
> stalls.

Target: a Tauri desktop app that reads sensor data, enriches it with batch /
recipe / production records in a local SQLite file, and becomes the brewery
operating system. ~~Fermentrack stays the fermentation-control appliance and the
logger of record~~ — **v2:** the ESP32 firmware is the control appliance, and
`brewd` on the Pi is the logger of record. See [../ARCHITECTURE.md](../ARCHITECTURE.md)
for the why.

**Scope:** hard seltzer first, but beer and other alcoholic fermentables are in
scope from the start. That is a schema constraint, not a someday-feature — no
table, column, or type in our own code is named "beer". `beverage_type` is a
column on `batch`, and anything style-specific (mash, hops, boil, nutrient
schedules) hangs off optional per-type tables rather than being bolted onto a
seltzer-shaped core. Fermentrack's own `Beer` naming stays quarantined inside the
`fermentrack` module.

**MQTT belongs to the Pi, not the desktop app.** ~~Nothing in v1 should assume a
broker exists~~ — v2 needs one, and it runs on the Pi next to `brewd`. The desktop
app still takes no MQTT dependency. It syncs `brewd`'s journals over HTTP with the
same byte-offset resume Phase 2 built for Fermentrack. `brewd` is the only MQTT
client that ingests, and it also publishes setpoints (Phase 6).

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
- [ ] ~~**Blocked — no Fermentrack instance yet.** Confirm against a real box:
      `curl http://<host>/api/devices/`, list `/data/`, capture one real `_full.csv`~~
      — **obsolete under v2**: no Fermentrack box will exist
- [x] Unblock the blocker: write a fixture generator instead (see Phase 2 detail).
      Phases 1–2 are fully buildable without Fermentrack ever running

**v2 spikes** — these gate Phase V, and each ends in a committed capture, not a note
- [x] Pick the Raspberry Pi model: **Raspberry Pi Zero 2 W**, recorded in
      [HARDWARE.md](HARDWARE.md)
- [ ] Set up the Pi Zero 2 W (later): 64-bit Raspberry Pi OS Lite on a high-endurance
      microSD — `brewd` appends a journal line every few seconds, all year, and the
      Zero has no practical SSD boot. Wi-Fi (2.4 GHz only), SSH
- [ ] Mosquitto on the Pi: one listener on the LAN, one user for devices and one
      for `brewd`, and persistence off. `brewd`'s journal is the record, not the broker
- [ ] Reflash ESP32 #1 from BrewPi-ESP32 to ESPHome: `dallas_temp` for both probes
      and the `mqtt:` component (no Home Assistant API). No `thermostat` yet —
      temp control is deferred (see "Order of real work"). Capture every topic and
      payload it publishes from `mosquitto_sub -v -t '#'`
- [ ] iSpindle on the same broker — **arriving ~2026-10-09**. Until then Vd runs on
      generated fixtures. Capture one full wake cycle: the stock firmware
      publishes one topic per field (`ispindel/<name>/tilt`, `temperature`,
      `battery`, `gravity`, `interval`, `RSSI`, …). Confirm the field list against
      the capture, not against this line
- [ ] Record in config which unit the iSpindle's calibration polynomial outputs (SG
      or °P). No payload says so

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
> **v2:** Phase 2 is done, and most of it survives. The scheduler (2g), the Tier 1
> write path (2e) and the offset discipline (2c–2d) are what Phase V's journal
> sync reuses. What goes dormant is the Fermentrack-specific surface: the
> `_full.csv` dialect (2b), discovery (2f) and the real-capture validation. Va
> lists what moves.

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
- [ ] ~~Replace fixtures with a real capture once Fermentrack is up — same tests~~
      — **obsolete under v2**. Vd's journal fixtures get the real-capture treatment
      instead, from the Phase 0 v2 spikes

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
      rows Rebuild exists to fix) and `newest_reading` (the footer's data age).
      3d added `insert_annotations` / `delete_annotations_for_source` alongside
      them: `annotation` is Tier 1, derived from the same bytes, so it is written
      in the same transaction and deleted by the same Rebuild — re-deriving one
      and not the other leaves 3d's suggestions quoting a file the readings no
      longer agree with

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
- [ ] ~~Optional helper: scrape `/beer/list/` and `/gravity/logs/` to offer a picklist.~~
      **Obsolete under v2** — `brewd` can list its own journals (Vc).
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

**2h. Deferred** — both **obsolete under v2**, being Fermentrack features
- [ ] ~~`_annotations.almost_json` → parked; annotations are already in `_full.csv`~~
- [ ] ~~HTTP listener for `GenericPushTarget` — only if 60 s latency proves too slow~~

**Done when:** pointing the app at the fixture server and growing the file
produces a correct, gap-free, duplicate-free `reading` table across app restarts.

### Phase 3 — Batch & recipe core
**3a. Reference data** — `0002_reference.sql`, `db/reference.rs`
- [x] `vessel` (name, kind, capacity_l, notes, archived_at) — needed here, not in
      Phase 7. "Active" is `archived_at IS NULL`; nothing in Tier 2 is hard-deleted
- [x] `ingredient` (name, category, default_unit, yeast_strain_id, attrs JSON).
      Categories: fermentable, sugar, yeast, nutrient, acid, salt, flavoring,
      fining, gas, water. `default_unit`'s *dimension* is what decides which
      canonical column a lot fills, so it is not merely a display preference
- [x] `yeast_strain` (name, lab, form, temp_min_c, temp_max_c, attenuation_min/max,
      alcohol_tolerance) — the finish detector needs the expected-FG band.
      Attenuation and tolerance are **fractions, not percents**, with a `CHECK`:
      a `78` typed where `0.78` was meant reads as a perfectly plausible band
- [x] `ingredient_lot` (ingredient_id, supplier, lot_code, qty_g/qty_l/qty_each,
      entry_qty, entry_unit, total_cost_minor, currency, purchased_at, expires_at)
      — cost and traceability both start here. `_minor`, not `_cents`: 1c's linter
      rejects `_cents`, and the exponent belongs to the currency, not the column name
- [x] **`total_cost_minor`, not `unit_cost_minor`** — what the receipt says for the
      whole lot. Minor units *per canonical unit* is an integer that rounds to zero:
      €12.50 for 5 kg of sucrose is 0.25 cents/g, which truncates to 0 and makes
      every cost in Phase 7 read €0.00. Per-unit cost is an `f64` division done at
      display (`LotRow::cost_per_canonical_unit`)
- [x] `qty_each` beside `qty_g`/`qty_l`, and a `CHECK` that exactly one is set —
      a packet of yeast or a campden tablet has neither a mass nor a volume
- [x] No `CHECK` on `vessel.kind`, `ingredient.category`, `yeast_strain.form`, for
      the reason `0001` gives for `stream.metric`: SQLite cannot widen a `CHECK`
      without rebuilding the table and all three sets will grow. The Rust enums in
      `db::reference` enforce them at the only write path, as `Metric` does
- [x] `water_profile` — ions as `*_ppm`, never `*_mg_l`: 1c's linter reads a `_l`
      suffix as "stored in litres", so that spelling would pass the lint while
      meaning something else
- [ ] Seeding `water_profile` from `water-data/valenciennes/tap_brewing-params.csv`
      is **deferred**: the file's Calcium, Magnesium and Sulfate columns are empty,
      values are French-decimal (`"7,9"`) and some are censored (`"<0,05"`). An
      importer would mostly import NULLs. Manual entry until the source fills in

**3b. Recipes, versioned properly**
- [x] `recipe` (name, beverage_type, archived_at)
- [x] `recipe_version` (recipe_id, version_no, target_volume_l, target_og, target_fg,
      target_abv, locked_at, notes) — UNIQUE (recipe_id, version_no)
- [x] `recipe_item` (recipe_version_id, ingredient_id, qty_g/qty_l/qty_each,
      entry_qty, entry_unit, stage, offset_hours, notes) — stage: prep, pitch,
      fermentation, conditioning, packaging. Canonical columns plus the entry pair,
      not a bare `qty, unit`: 1c names `recipe_item` under that rule, and the same
      `CHECK` as `ingredient_lot` keeps exactly one of the three set
- [x] No `CHECK` on `recipe.beverage_type` or `recipe_item.stage`, for the reason
      3a gives for `vessel.kind`. `BeverageType` and `Stage` in `db::recipe` enforce
      them at the only write path
- [x] **A version referenced by any batch is immutable.** Editing it forks the next
      version number. Enforce in the data layer, with a test — `locked_at` non-NULL
      is the freeze; every mutation goes through `ensure_unlocked`, item edits
      resolving their version first, and `fork_recipe_version` is the way out.
      3c's `create_batch` calls the idempotent `lock_recipe_version` in its own txn
- [x] "Fork to new version" and "diff two versions" as data-layer functions —
      `fork_recipe_version` and `diff_recipe_versions`, the latter over a pure
      `diff_items` keyed on (ingredient, stage, occurrence) and *not* `offset_hours`,
      so a nutrient addition moved 24 h → 36 h reads as one change, not a re-add
- [ ] The same two in the UI, **deferred to the Phase 3 UI pass**: a recipe screen
      with no batch screen is half a feature, so it lands once 3c exists
- [x] Nutrient schedule as first-class `recipe_item` rows with `offset_hours` —
      for a sugar wash this is the process variable that decides the batch

**3c. Batches** — `0004_batch.sql`, `db/batch.rs`
- [x] `batch` (code, recipe_version_id, beverage_type, vessel_id, status,
      volume_l, og, fg, pitched_at, archived_at, notes). `recipe_version_id` is
      **nullable**: an experiment with no written recipe is still a batch, and a
      throwaway recipe to satisfy a foreign key would be fiction in the one tier
      that is never reconstructible. `plan_vs_actual` reports its every addition as
      unplanned instead
- [x] `create_batch` calls 3b's `lock_recipe_version` — the call 3b was written for.
      It first checks the version's recipe agrees with the batch's `beverage_type`,
      and refuses *before* the lock, so a typo leaves the version still editable
- [x] `batch.code` unique; next code auto-suggested per type (`SELTZER-018`).
      `suggest_batch_code` skips codes that don't fit the series rather than
      rejecting them — `UNIQUE` is the only enforcement here
- [x] Status machine: planned → fermenting → conditioning → packaged. Illegal
      transitions rejected in the data layer, not just greyed out in the UI; a
      no-op (`x → x`) is `Ok(false)`, the same idempotence rule as
      `lock_recipe_version`. ~~→ archived~~ — **`archived_at` is the archive**, not
      a status value: two places recording one fact is two places to disagree, and
      an archived batch should still read as whatever it was doing when it was put
      away
- [x] `pitched_at` non-NULL is the gate out of `planned`. 3d's `pitch` event will
      write that column rather than replace the rule — pitch is t=0 for the overlay,
      the finish detector and lag time alike, and discovering afterwards that half
      the batches have no t=0 is not a fix
- [x] `beverage_type` drives which optional side tables apply — no nullable
      beer-only columns on `batch`. It lives on `batch` in its own right and is not
      read through the recipe, because a batch may have none
- [x] `batch_ingredient` (batch_id, ingredient_lot_id, ~~qty, unit~~ **canonical
      qty + entry pair**, stage, added_at) — the *actuals*, against `recipe_item`'s
      plan. The `qty, unit` above predates 1c: a bare pair makes `SUM(qty)`
      meaningless, which is every cost and every planned-vs-actual delta, so this
      carries `qty_g`/`qty_l`/`qty_each` + `entry_qty`/`entry_unit` under the same
      `CHECK` as `recipe_item` and `ingredient_lot`. `canonical_for_lot` resolves
      the lot's ingredient and then calls 3a's `canonical_for_ingredient`, so the
      dimension check is one code path across all three tables
- [x] Planned-vs-actual view per batch — `plan_vs_actual`, keyed on
      (ingredient, stage, occurrence) exactly as `diff_items` is, and *not* on when
      the addition happened: a nutrient that slipped six hours is the row the batch
      is diagnosed from, and keying on the time splits it into an omission plus a
      surprise
- [x] Never hard-delete a batch; `archived_at` only. An archived batch takes no
      transition, no pitch time and no further additions

**3d. Timeline and stream binding** — `0005_timeline.sql`, `db/timeline.rs`
- [x] `batch_event` (batch_id, ts, kind, payload JSON, note). `payload` is opaque
      JSON at the data layer, like `source.config` and `ingredient.attrs` —
      every kind carries different fields and none of them is queried
- [x] Event kinds: pitch, nutrient_addition, flavoring_addition, temp_ramp,
      cold_crash, transfer, fining, carbonation, package, **note**. The last is
      beyond the list above on purpose: an imported annotation that maps to no
      process step needs a home, and free text with nowhere to go is where a
      closed set quietly stops being closed
- [x] `pitch` is required to leave `planned` — it is t=0 for every later comparison.
      **The event is the write path**: `record_pitch` inserts the row, sets
      `batch.pitched_at` and moves planned → fermenting in one call, and `add_event`
      refuses `EventKind::Pitch` so the column and the row cannot be written apart.
      A partial unique index (`WHERE kind = 'pitch'`) makes a second pitch an error
      rather than a silent second t=0. Undoing one is `undo_pitch`, the only
      backwards status move in the app — it deletes the row, NULLs the column and
      returns the batch to `planned` together, and is refused once the batch is
      past `fermenting`
- [x] `batch_stream` (batch_id, stream_id, starts_at, ends_at). No
      `ON DELETE CASCADE` on `stream_id`: `stream` rows outlive a Rebuild precisely
      so a binding survives one
- [x] Reject overlapping bindings of one stream to two batches (SQLite has no
      exclusion constraint — check on write, and test it). A `SELECT` before the
      `INSERT` in the caller's transaction, over half-open `[starts_at, ends_at)`
      so batch 18 may start exactly when 17 ends — that turnaround is what the
      table is for. Two windows on the *same* batch are not an overlap: a sensor
      unplugged and reconnected is one curve in two pieces
- [x] Import Fermentrack's `beer_ann` / `fridge_ann` cells as suggested events.
      **Stored at ingest, suggested on read**: 0005 adds a Tier 1 `annotation`
      table that 2e's commit path fills in its existing transaction, and
      `suggest_events_for_batch` reads it back through `batch_stream`. Writing
      `batch_event` rows at ingest was rejected — it would have the Phase 2 sync
      path writing Tier 2, which is the seam Option 2 exists to keep. A suggestion
      is a computed struct, not a row; `accept_suggestion` is what makes it an
      event, with `payload` recording that Fermentrack said it. The channels are
      `vessel`/`chamber`, not `beer_ann`/`fridge_ann` — the schema linter rejects
      any name containing "beer", and Fermentrack's vocabulary stays quarantined.
      **Dormant under v2**: no v2 source writes `annotation`, so
      `suggest_events_for_batch` returns nothing. The table and function stay; they
      cost nothing and would be revived by a Fermentrack fallback
- [ ] Timeline UI: events + manual measurements on one vertical list —
      **deferred to the Phase 3 UI pass**, with 3b's recipe screen and a batch
      screen. Half of what this list renders is 3e's manual measurements, so it
      lands once they exist

**3e. Manual measurements**
- [x] One `source` with `kind='manual'` per batch; its streams bound via `batch_stream`.
      Created on first use, one stream per metric, `external_id = manual/{batch}/{metric}`
      (MQTT-shaped, for v2's broker). Found through the `batch_stream` join and not
      through `source.config`, because Tier 1 must not reference Tier 2 — there is no
      `batch_id` column on `source` to look at. A binding starts at the earlier of the
      reading and the pitch, and `timeline::set_binding_start` widens it backwards when a
      backdated reading arrives: the first measurement *typed* is routinely not the
      earliest one *taken*, and a reading outside its own window is invisible to
      `batch_for_stream_at`
- [x] Quick-entry form: gravity, pH, temp, TA, taste note — two clicks from the batch.
      `record_quick_entry` is the whole call, at one `ts` so two readings from the same
      bench moment do not land a second apart and draw a trend. The **form** is
      **deferred to the Phase 3 UI pass** with 3b's recipe screen and 3d's timeline.
      The taste note goes to `batch_event` as an `EventKind::Note`, not to a column here:
      a note is something you observed, not a number on a time series
- [x] Gravity input accepts SG, °P or Brix, converted to SG on entry. `0006_manual.sql`'s
      `manual_measurement` keeps the entry record — 1c's "canonical column plus entry
      pair", so `value` is SG and `(entry_value, entry_unit)` is what was on the
      instrument — and fans the derived number out into a Tier 1 `reading` in the same
      transaction. `reading` stays `(stream_id, ts, value)`: widening it with five columns
      that are NULL on 99.99 % of rows would pay for manual entry out of the chart's
      budget, and a manual reading is never reconstructible from the SG it became
- [x] Hydrometer temperature correction, with the calibration temp configurable
      (`measurement.hydrometer_calibration_c`, default 20 °C; 60 °F instruments are
      common). Applied only when *both* temperatures are given — one alone does not say
      what to correct against. `calc::hydrometer` is the ratio of a water-density cubic at
      the two temperatures, **not** the difference form several calculator pages publish,
      which is wrong for this coefficient set
- [x] Refractometer readings post-pitch need alcohol correction — flag them as
      refractometer so the correction is applied, and do not silently mix the two.
      `Instrument` decides which correction runs and `derive_value` refuses every
      crossover: a wort correction factor on a hydrometer, a temperature on a
      refractometer (Terrill's cubic is fitted against reference-temperature hydrometer
      values and the instrument is ATC — both would double-count), a refractometer
      reading typed in SG, and a post-pitch reading with no `batch.og` to subtract
      against. Nothing defaults the wort factor either: it is 1.0 for a sugar wash, which
      is most of what this app ferments, and 1.04 there costs two gravity points.
      **A manual gravity does not write `batch.og`/`batch.fg`** — the post-pitch
      correction *reads* `batch.og`, so a reading that wrote it back would corrupt every
      later correction, silently and cumulatively. The UI pass gets an explicit
      "use this as the OG/FG" button

**3f. Calc module (`calc/`, pure functions, heavily tested)**
- [x] SG ↔ °P ↔ Brix — landed early with 3e, which cannot store a Brix entry without
      them. Cubic regressions on de Clerck's Plato tables, with the reverse direction by
      bisection over `Sg::PLAUSIBLE` rather than a second published polynomial, so both
      directions are the *same* fit and round-trip exactly. Out of range is
      `Error::CalcOutOfRange`, never extrapolation. `calc/hydrometer.rs` arrived with them
      and is not on this list: the temperature correction is a polynomial fit of measured
      data, and `units/mod.rs` says that is not a unit conversion
- [x] Apparent attenuation `(OG − FG) / (OG − 1)` — `calc/attenuation.rs`, a **fraction**
      like `yeast_strain.attenuation_*`, and deliberately **not clamped to 1.0**: a wash
      finishing at 0.998 attenuates past 100 %, and that is the number that says it went
      dry. One shared guard refuses a swapped pair and an `OG` within two gravity points
      of water — `Sg::PLAUSIBLE` starts at 0.900, so 1.0002 reaches the divide today
- [x] Real extract, real attenuation, ABW — Balling, the same file. `RE = 0.1808·OE +
      0.8192·AE` off 3f's own `sg_to_plato`, which is why these land after 3e rather than
      beside it. ABW is published in percent and divided by 100 here, said out loud in the
      doc: the first version of this shipped a percent through a fraction's signature and
      the textbook test caught it. Refused past 30 °P — Balling's denominator reaches zero
      at 193 °P and is fiction long before
- [x] ABV: `calc/abv.rs`, `simple` and `alternate`, and **no function called `abv`**.
      Neither is the default and neither is named for the quantity, so a caller has to
      name the one it is showing — "show both, label which is which" is unenforceable if
      one of them can be reached by accident. `alternate` is Hall, *Zymurgy* Summer 1995,
      coefficients verified against the source. They agree within 0.1 pp on a 1.050/1.010
      beer and differ by 0.4 pp on a 1.075 → 0.998 seltzer, which is the test that earns
      two functions
- [x] Refractometer correction (Terrill cubic + wort correction factor) —
      **verify the coefficients against the source when implementing**. Landed early with
      3e. Verified against Sean Terrill's 2011-04-07 post and cross-checked against
      Brewer's Friend's independent rendering and its worked example. Error band is
      documented rather than quietly removed: σ ≈ 0.0015 SG, mean bias ≈ +0.0007 SG,
      fitted on 12 beers at 73–91 % apparent attenuation. deLange's debiased variant
      differs only in the leading constant; Terrill pinned his to unity deliberately so
      the formula stays sane outside the interpolated range, and that is the one
      implemented
- [x] Sugar mass → OG for a sugar wash, from mass fraction rather than PPG —
      `calc/sugar.rs`. `°P = 100·sugar/(sugar + water)` is *exact* for sucrose, so the only
      fit in the path is `plato_to_sg`. Water is 0.99823 kg/L at 20 °C, stated rather than
      taken as 1.000: it is 0.04 °P on a 20 °P wash. `sugar_for_og` inverts it in closed
      form — the mass fraction is algebra, and re-inverting numerically would put
      `plato_to_sg`'s bisection in the loop for nothing
- [x] Dilution / blending: `blend` over any number of parts, `dilute` as its two-part
      case. Conserves **extract mass**, not gravity points, which makes the resulting
      gravity exact and independent of whether volumes add; the returned *volume* is the
      approximation, at under 0.5 % for what this app blends, and the doc says which half
      is which
- [x] Yeast pitch rate: `calc/pitch.rs`, `cells_needed` and `dry_yeast_grams`. **Pure —
      no migration.** `yeast_strain` has no cells-per-gram, viability or pack-size column
      and is not getting one in Phase 3; they are arguments with cited consts
      (`RATE_ALE`, `DRY_CELLS_PER_GRAM`). `YeastForm` is not matched on here either: a
      `calc` function reading a `db` enum would be the first thing in the module to import
      from the layer above it, so the form decides at the call site
- [x] YAN target and staggered-nutrient math — `calc/yan.rs`. Per-product nitrogen
      yield is an **argument**, with cited consts: DAP is stoichiometric (212 mg N/g,
      21.2 % by mass) and the only exact one; Fermaid O's supplier figure and TOSNA's
      differ fourfold and the doc says so rather than picking quietly. Nothing defaults the
      demand factor, same rule as the wort correction. `stagger` takes dose *fractions*,
      not a count, and refuses a set that does not sum to 1.0 — a set summing to 0.9
      under-feeds by a tenth and looks exactly like a nitrogen-limited ferment.
      Timing stays in 3b's `recipe_item.offset_hours`, where a schedule is actually stored
- [x] Cost per batch and per litre — split in two, because `calc` takes no `Connection`:
      `calc/cost.rs` folds priced lines, `db::batch::batch_cost` does the join. Its own
      `SELECT` rather than a wider `BatchIngredientRow`, which is on the command surface and
      whose business is not what a lot cost. The fold accumulates in `f64` minor units and
      **rounds once at the end**, for 3a's reason: €12.50 over 5 kg is 0.25 minor units per
      gram. An unpriced lot is `lines_without_cost`, never a zero, and no total at all is
      `None` — a batch nobody priced did not cost nothing. Mixed currencies are
      `money.currency_mismatch`. Volume falls back to the recipe version's target.
      Phase 7's cost report is now a call, not a rewrite
- [x] Every formula unit-tested against a known worked example with its source cited

**Done when:** a batch can be created from a recipe version, pitched, have manual
readings and events logged against it, and show a correct OG/FG/ABV and
planned-vs-actual — with the Fermentrack curve already attached via `batch_stream`.
**v2:** "the sensor curve", attached the same way — see Ve for how it gets bound.

### Phase V — v2 transport: Raspberry Pi, MQTT, `brewd`
Replaces Fermentrack as the path from sensor to `reading`. Shape:

```text
ESP32 (ESPHome) ─┐                      Raspberry Pi                    Desktop
                 ├─ MQTT ─► Mosquitto ─► brewd ─► journals ─ HTTP Range ─► brew-app
iSpindle ────────┘                        │                                (SQLite)
                                          └─ setpoints, profiles, alerts (Phase 6)
```

The control loop runs on the ESP32. `brewd` runs what must happen while the
laptop is shut. The desktop app does everything else.

**Va. Rework from phases 1–3**
- [x] Migration runner gets a table-rebuild mode. `apply_upto` runs every migration
      inside a transaction with `foreign_keys=ON`, and that pragma is a silent no-op
      inside a transaction. Rebuilding a table other tables reference needs
      `foreign_keys=OFF` *before* `BEGIN`, a `PRAGMA foreign_key_check` that must
      return no rows before `COMMIT`, and `foreign_keys=ON` after. Mark such
      migrations explicitly in `MIGRATIONS`; don't infer it from the SQL. Test with
      a populated `stream` pointing at the rebuilt table
- [x] Migration `0007`: rebuild `source` without the `kind` CHECK. 0001 closed it
      over the two Fermentrack kinds plus `manual` — the one closed-vocabulary column
      that did not follow its own `metric` reasoning. `SourceKind` becomes the
      enforcement, as `Metric` is. Add `brewd_journal`
- [x] Hoist the format-agnostic half of Phase 2 out of `fermentrack/` into a
      shared module (`journal/`): `split_complete_lines`,
      `MAX_PARTIAL_RECORD_BYTES`, the `Range` fetch and `FetchOutcome` classification,
      the record-boundary resume check, and commit-offset-with-readings. `fermentrack/`
      keeps only the `_full.csv` dialect. The existing tests move with the code and
      must pass unchanged — that is the proof the hoist changed nothing. The seam is
      one `journal::Dialect` trait; `fermentrack::sync::FullCsv` is the first impl
- [x] `sync_state` is reused as is. `remote_path` becomes the journal path on
      `brewd`, and `header` the journal's header line. No schema change
- [x] `Metric` gains `tilt_deg`, `battery_v`, `rssi_dbm`. No migration — `metric`
      has no CHECK, by 0001's design. `state` stays and goes unused unless the
      firmware publishes heat/cool action
- [x] Config: a `brewd` section (`base_url`) beside `fermentrack`, and a
      `gravity.ispindel_unit` (`sg` | `plato`) from the Phase 0 spike. MQTT broker
      settings are `brewd`'s config, not the desktop app's
- [x] Tier 1 is no longer "cheap to throw away" in the old sense. Rebuild still
      works, but only because `brewd`'s journals are the raw archive. Lose the Pi's
      disk and Tier 1 is gone. Cross-cutting backup now covers the journals

**Vb. Firmware and broker** (lands with the Phase 0 v2 spikes)
- [ ] ESPHome config committed to the repo (proposal: `firmware/esp32-1.yaml`),
      secrets in `secrets.yaml`, gitignored
- [ ] Probes addressed by OneWire address, never by bus order: vessel probe →
      `temp_c`, chamber probe → `fridge_temp_c`
- [ ] **Deferred with temp control:** `thermostat` climate with
      `min_cooling_off_time` / `min_cooling_run_time`. BrewPi protected the
      compressor. Nothing does now unless this is set
- [ ] **Deferred with temp control:** the ESP32 keeps controlling to its last
      setpoint when the Pi or the broker is down. Test it by stopping Mosquitto mid-run

**Vc. `brewd` — new crate `crates/brewd`, binary, depends on `brewcore`, no `tauri`**
- [ ] MQTT subscriber (`rumqttc`). **Timestamp at receipt**, UTC epoch seconds —
      iSpindle and ESPHome payloads carry no time, and Mosquitto adds none
- [ ] Group an iSpindle wake cycle into one timestamp. The stock firmware publishes
      one topic per field in a burst, and stamping each field separately draws a
      trend out of one reading. Same rule as 3e's `record_quick_entry`
- [ ] Append-only journal per device, CSV with a header row, `\n`-terminated,
      flushed per line. Keep the raw `tilt` beside `gravity`: gravity is a calibration
      polynomial applied on the device, and tilt is what lets SG be recomputed after
      a recalibration
- [ ] HTTP: `GET /journals/` lists journals (the listing Fermentrack never had);
      `GET /journals/<name>` honours `Range`. Read-only, LAN-only, no auth — same
      trust model Fermentrack had
- [ ] Runs under systemd, restarts on failure. Logs to journald
- [ ] Cross-compiled for the Pi Zero 2 W's `aarch64-unknown-linux-gnu` (64-bit OS),
      via `cargo xtask` or `cross`. Never built on the device: 512 MB RAM does not
      link a Rust binary of this size
- [ ] No SQLite on the Pi. `brewery.db` stays on the desktop; `brewd` owns only
      its journals and its config. Revisit only if a second machine needs to write

**Vd. Journal ingest** — a `ReadingSource` impl beside `fermentrack::source`
- [ ] Parser for the journal format. Rows map to metrics by header, like 2b
- [ ] Fixture generator in `crates/fixtures`: iSpindle bursts, a probe that drops
      out, a `brewd` restart mid-line, a journal rotated underneath a resume
- [ ] The growing-file test from 2a, rerun against journals under both `RangeMode`s
- [ ] Replace generated fixtures with the Phase 0 captures — same tests

**Ve. Desktop wiring**
- [ ] Sources pane: pick journals from `brewd`'s listing instead of pasting URLs
- [ ] v2 streams belong to a physical sensor and outlive batches, which is the case
      `batch_stream` was designed for. Add a vessel → default-streams mapping (Tier 2,
      referencing `stream.id`, never a topic) so `record_pitch` binds the vessel's
      sensors from pitch onward and a status move to `packaged` closes the window
- [ ] Footer: `brewd` reachability as its own state, distinct from "no new data"

**Done when:** the Pi logs both probes and the iSpindle with the laptop shut, and
opening the app backfills a gap-free curve bound to the pitched batch.

### Phase 4 — Dashboard & curves
- [ ] Batch detail view: the ARCHITECTURE.md status card (OG, current SG, est. ABV, pH, temp)
- [ ] Fermentation curve chart, multi-axis (SG / temp / pH), event markers
- [ ] Downsampling for long series so the chart stays interactive
- [ ] Smoothing for iSpindle SG — tilt-derived gravity jitters by a few points,
      and the stability detector must read the smoothed series, not the raw one
- [ ] "Fermentation stable for Nh" detector
- [ ] Home view: active batches, last reading age, stale-data warning
- [ ] iSpindle health on the home view: battery voltage and RSSI. A dead battery
      and a flat ferment look identical from the gravity series alone

### Phase 5 — Analytics
- [ ] Batch-to-batch comparison (overlay curves, time-aligned from pitch)
- [ ] Saved queries: "all batches, yeast X, OG 1.035–1.040, ferm temp 18°C"
- [ ] Derived per-batch metrics table (lag time, peak rate, days to terminal)
- [ ] Yeast/strain performance rollup
- [ ] CSV export of any result set

### Phase 6 — Alerts & automation rules
> **v2:** rules and profiles run in `brewd`, not the desktop app. An alert that
> only fires while the laptop is open doesn't fire at 3 am, and a 09:00 ramp can't
> depend on someone having the app open. The rule engine is `brewcore` code, so
> the move is about where it runs, not a rewrite.
>
> **Temp control is deferred** until everything else is built and running:
> relays, the ESPHome `thermostat`, the `Controller` trait and profiles. Alert
> rules need no relays and can land before them.

- [ ] Rule engine: condition on latest readings → action. Lives in `brewcore`,
      runs in `brewd`
- [ ] ~~Desktop notifications~~ — **v2:** push notifications from `brewd`
      (proposal: ntfy). The desktop app may mirror them but is not the channel
- [ ] Rule: fermentation finished (SG slope flat for N hours)
- [ ] Rule: temp out of band, sensor stale, no data. "No data" includes the
      iSpindle going silent — battery, or it sank
- [ ] ~~Optional write-path: `setBeer=`/`setFridge=`/`setActiveProfile=` over the
      brewpi-script socket, behind a confirm; mirror it in our own records because
      Fermentrack's DB won't see it~~ — **v2:** a `Controller` trait whose first
      impl publishes the setpoint to the ESPHome climate's command topic. Behind a
      confirm, and every setpoint change becomes a `temp_ramp` `batch_event`
- [ ] Temperature profiles (Fermentrack had these; now they're ours): a schedule
      of setpoints relative to pitch, executed by `brewd`. The desktop app edits
      them and pushes the definition to `brewd` over HTTP. `brewd` keeps it in a
      state file, since it has no database, and resumes it after a reboot. This is
      `brewd`'s first write endpoint, so it is where "LAN-only, no auth" gets
      revisited
- [ ] Rules evaluated on ingest, not on a UI timer — in v2, on `brewd`'s ingest

### Phase 7 — Production records
- [ ] Cleaning / sanitation log, per vessel
- [ ] Packaging records (volume, format, date, loss)
- [ ] Cost per batch from ingredient lots
- [ ] Ingredient inventory with lot consumption
- [ ] Batch report export (PDF or printable HTML)

### Cross-cutting
- [ ] `brewery.db` backup command (file copy + integrity check)
- [ ] Journal backup off the Pi. Under v2 the journals are the only raw copy of
      every reading, and a Pi's disk is the likeliest part to fail
- [ ] Schema migration test: old db → new version, no data loss
- [ ] All timestamps stored UTC epoch seconds, formatted at the edge
- [ ] Keep every Fermentrack call inside one `fermentrack` module — the seam for Option 2.
      Still holds under v2: the module is dormant, not deleted, and nothing new imports it

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
| Always-on host | Raspberry Pi Zero 2 W | v2: logger of record and profile runner while the desktop is off |
| Broker | Mosquitto | v2, on the Pi. Persistence off — `brewd`'s journal is the record |
| MQTT client | `rumqttc` | v2, `brewd` only. The desktop app never links it |
| Device firmware | ESPHome | v2: `dallas_temp` + `thermostat` + `mqtt`, control loop on the ESP32 |
| Gravity | iSpindle, stock firmware | v2: publishes over MQTT natively |

Frontend framework is the one genuinely swappable choice — React works if you
prefer it. Everything else follows from ARCHITECTURE.md.

### Crate layout

```text
brew-app/
├── crates/
│   ├── brewd/             # v2: headless Pi daemon — MQTT in, journals out, profiles
│   ├── fixtures/          # brewfixture: generated logs + wiremock harness
│   └── brewcore/          # no tauri dependency — plain `cargo test`
│       ├── src/
│       │   ├── db/        # connection, migrations, queries
│       │   ├── fermentrack/  # the `_full.csv` dialect: csv_log.rs, discovery.rs, sync.rs
│       │   ├── journal/   # format-agnostic sync: Range client, record split, offset + commit
│       │   ├── calc/      # ABV, attenuation, stability
│       │   ├── units/           # temperature, gravity, quantity, money, time
│       │   ├── config.rs
│       │   └── error.rs
│       └── migrations/    # 0001_tier1.sql, 0002_reference.sql, …
├── src-tauri/             # commands, app state, window setup. Thin.
├── src/                   # Svelte
├── firmware/              # v2: ESPHome YAML per device
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
    (2, include_str!("../migrations/0002_reference.sql")),
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
**v2:** derived from `brewd`'s journals, and re-syncable only while they exist.
`source.kind` gains `brewd_journal` in Va's `0007`, which also drops the CHECK
shown below.

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

-- Added by 0005, but Tier 1: the `beer_ann` / `fridge_ann` cells of the same rows
-- the readings come from, so a Rebuild deletes and re-derives both together.
-- 3d reads it back through `batch_stream` as suggested `batch_event`s.
CREATE TABLE annotation (
  source_id   INTEGER NOT NULL REFERENCES source(id),
  ts          INTEGER NOT NULL,
  channel     TEXT NOT NULL,          -- 'vessel' | 'chamber'
  text        TEXT NOT NULL,
  PRIMARY KEY (source_id, ts, channel)
) WITHOUT ROWID;
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
then batch 18 with the curves staying separate. Windows are half-open
`[starts_at, ends_at)` so that turnaround is not an overlap, and one stream may
not be bound to two batches at once — SQLite has no exclusion constraint, so
`bind_stream` checks before it inserts.

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
exactly why Fermentrack stays the logger of record. **v2:** the same reasoning is
why `brewd` exists, and why its journals are append-only files served with
`Range` rather than an API — so this loop applies to them nearly unchanged.

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

Which means the pitch *event* is the write path for `batch.pitched_at`, not a
second record of it: `record_pitch` writes the row, the column and the status
transition together, `add_event` refuses the kind outright, and a partial unique
index allows exactly one per batch. The column stays, because 3c's gate and every
later query read it — but it is a denormalisation of the event, and only one
function may write it. Undo is `undo_pitch`, deliberately a single named
operation rather than a generic delete, because putting a batch back to `planned`
is the only backwards move the status machine permits and it should be findable.

Fermentrack's own `beer_ann` / `fridge_ann` cells are the seed for this list, but
they arrive through Tier 1: the sync path stores them in `annotation` (same
transaction as the readings, same Rebuild), and `suggest_events_for_batch` joins
them to a batch through `batch_stream`. A suggestion is a computed struct with a
keyword guess at its kind — deliberately shallow, since it exists to pre-select a
dropdown and a cleverer classifier's mistakes would land in the one tier that is
never reconstructible. Nothing becomes a `batch_event` until `accept_suggestion`
is called, and what it writes records where it came from.

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

**v2:** with an iSpindle, the slope is computed over the smoothed series (Phase 4)
— raw tilt-derived gravity jitters enough to fake a slope in either direction.
The alerting copy of this detector runs in `brewd` (Phase 6); the dashboard copy
runs in the app. Same `brewcore` function, two callers.

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

### What actually happened (2026-09-25)

The answer to "when" was "now". The four rules held: the `fermentrack` module is
self-contained, `external_id` is opaque, no Tier 2 table names a Fermentrack id,
and `ReadingSource` is the scheduler's only view of a source. The `Controller`
trait was never built because Phase 6 hadn't started, so v2 builds it with MQTT as
its first impl.

What the plan above got wrong: it put the `ReadingSource` MQTT impl in the
desktop app. MQTT has no history. A desktop app that is closed misses every
message, and backfill on launch only worked because Fermentrack was an always-on
logger. So v2 adds a Raspberry Pi running `brewd` in Fermentrack's place, and
the desktop's new `ReadingSource` reads `brewd`'s journals over HTTP — much
closer to the existing Fermentrack impl than to an MQTT subscriber. Phase V has
the detail.

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

**v2 order (from 2026-09-25):** Phase 0's v2 spikes (Pi, Mosquitto, ESPHome,
iSpindle captures) → Va (migration runner, `0007`, the hoist — all testable with
no hardware) → Vc–Vd (`brewd` and journal ingest, against generated fixtures
until the captures exist) → Ve → the Phase 3 UI pass → Phase 4. Va comes first
among the code work because Vd builds on the hoisted module.

v2 starts as a **logger only**. Temp control (relays, `thermostat`, `Controller`,
profiles) comes after everything else is built and running. Until then, ferments
run without active temperature control, and the app records what happened rather
than steering it.

No hardware needed for Va, Vc, Vd (on generated fixtures), the Phase 3 UI pass,
or Phase 4 — the app can be built through all of them before the iSpindle
(~2026-10-09) arrives or the Pi Zero 2 W is set up.
