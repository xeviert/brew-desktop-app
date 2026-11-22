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
      [README — Hardware](../../README.md#hardware)
- [x] Set up the Pi Zero 2 W: 64-bit Raspberry Pi OS Lite on a high-endurance
      microSD — `brewd` appends a journal line every few seconds, all year, and the
      Zero has no practical SSD boot. Wi-Fi (2.4 GHz only), SSH. Address and MAC in
      [README — Hardware](../../README.md#hardware)
- [x] Mosquitto on the Pi: one listener on the LAN, one user for devices and one
      for `brewd`, and persistence off. `brewd`'s journal is the record, not the broker
- [x] Reflash ESP32 #1 from BrewPi-ESP32 to ESPHome: `dallas_temp` ~~for both probes~~
      — **one probe**, DS18B20 #2 was dropped — and the `mqtt:` component (no Home
      Assistant API). No `thermostat` yet — temp control is deferred (see "Order of
      real work"). Capture every topic and payload it publishes from
      `mosquitto_sub -v -t '#'` → [captures/esp32-1-capture.txt](captures/esp32-1-capture.txt)
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
- [ ] ~~Seeding `water_profile` from local tap-water data~~
      — **dropped**, won't be built: the file's Calcium, Magnesium and Sulfate
      columns are empty, values are French-decimal (`"7,9"`) and some are censored
      (`"<0,05"`). An importer would mostly import NULLs. Water profiles are
      entered by hand

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
- [x] The same two in the UI, **deferred to the Phase 3 UI pass** (U2): a recipe screen
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
- [x] Timeline UI: events + manual measurements on one vertical list —
      **deferred to the Phase 3 UI pass**, with 3b's recipe screen and a batch
      screen. Half of what this list renders is 3e's manual measurements, so it
      lands once they exist. **Landed in U4**

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
                                          ├─ alerts → ntfy (Phase 6, built)
                                          └─ setpoints, profiles (Phase 6, deferred)
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
- [x] ESPHome config committed to the repo: `firmware/esp32-1.yaml`, secrets in
      `firmware/secrets.yaml`, gitignored (`secrets.example.yaml` committed)
- [x] Probes addressed by OneWire address, never by bus order: vessel probe →
      `temp_c`. ~~chamber probe → `fridge_temp_c`~~ — no chamber probe, DS18B20 #2
      was dropped
- [ ] **Deferred with temp control:** `thermostat` climate with
      `min_cooling_off_time` / `min_cooling_run_time`. BrewPi protected the
      compressor. Nothing does now unless this is set
- [ ] **Deferred with temp control:** the ESP32 keeps controlling to its last
      setpoint when the Pi or the broker is down. Test it by stopping Mosquitto mid-run

**Vc. `brewd` — new crate `crates/brewd`, binary, depends on `brewcore`, no `tauri`**
- [x] MQTT subscriber (`rumqttc`, no TLS). **Timestamp at receipt**, UTC epoch seconds —
      iSpindle and ESPHome payloads carry no time, and Mosquitto adds none. Retained
      messages are skipped: ESPHome retains state by default, and a replay on
      subscribe would journal a stale value as fresh. Topics are exact, no wildcards —
      each maps to one `(device, column)` in `brewd.toml`
- [x] Group an iSpindle wake cycle into one timestamp. The stock firmware publishes
      one topic per field in a burst, and stamping each field separately draws a
      trend out of one reading. Same rule as 3e's `record_quick_entry`. `burst.rs`:
      a device's first message opens a burst and fixes its time; it closes when
      every column has arrived, a column repeats, or `burst_window_ms` (2 s) runs out.
      One rule for the iSpindle and ESPHome's two probes
- [x] Append-only journal per device, CSV with a header row, `\n`-terminated,
      flushed per line. Keep the raw `tilt` beside `gravity`: gravity is a calibration
      polynomial applied on the device, and tilt is what lets SG be recomputed after
      a recalibration. Header is `time,<columns…>`; values are the raw payloads, no
      unit conversion — `gravity.ispindel_unit` is the desktop's to apply. On start, a
      half-written last line is trimmed; a changed header rotates the old journal to
      `<device>.<epoch>.csv`. `brewcore::journal::format` holds what both sides share
- [x] HTTP: `GET /journals/` lists journals (the listing Fermentrack never had);
      `GET /journals/<name>` honours `Range`. Read-only, LAN-only, no auth — same
      trust model Fermentrack had. `tower-http`'s `ServeFile`; tested end to end
      against `journal::Client`
- [x] Runs under systemd, restarts on failure. Logs to journald —
      `crates/brewd/deploy/brewd.service`, running on the Pi (README Setup,
      step 4)
- [x] Cross-compiled for the Pi Zero 2 W's `aarch64-unknown-linux-gnu` (64-bit OS),
      via `cargo xtask` or `cross`. Never built on the device: 512 MB RAM does not
      link a Rust binary of this size — `cargo xtask build-pi` wraps `cross`. On
      Windows it needs a one-off `--force-non-host` toolchain (README Setup, step 4)
- [x] No SQLite on the Pi. `brewery.db` stays on the desktop; `brewd` owns only
      its journals and its config. Revisit only if a second machine needs to write.
      (`brewd` still links rusqlite through `brewcore`; it opens no database)

**Vd. Journal ingest** — a `ReadingSource` impl beside `fermentrack::source`
- [x] Parser for the journal format. Rows map to metrics by header, like 2b —
      `brewd::dialect`, a fixed column table: `tilt`, `gravity`, `temperature`,
      `battery`, `rssi` for the iSpindle; `vessel_temp`, `chamber_temp`, `room_temp`
      for the ESP32; `interval` ignored, anything else reported as unknown. A header
      mapping two columns to one metric is refused, since streams key on metric.
      A `gravity` column with `gravity.ispindel_unit` unset fails the poll and
      consumes nothing, so setting the unit later ingests every row it held back
- [x] Rotation drained, not lost: when a resume finds the live journal truncated or
      mid-record, the source reads the rest out of the newest rotated sibling whose
      header matches, then restarts the live file at 0 with a
      `RestartReason::Rotated` that keeps the readings (`Dialect::deletes_on_restart`;
      Fermentrack's default is unchanged). Rebuild deletes, then rereads every
      rotated journal oldest first, then the live one. The base URL and gravity unit
      are read from the live config on every poll
- [x] Fixture generator in `crates/fixtures`: iSpindle bursts, a probe that drops
      out, a `brewd` restart mid-line, a journal rotated underneath a resume —
      `JournalScenario`, goldens in `tests/fixtures/journal/`,
      `cargo xtask gen-fixture --format journal`
- [x] The growing-file test from 2a, rerun against journals under both `RangeMode`s —
      `tests/journal_sync.rs`, plus every split point, a restart mid-line and the
      unit wedge; `tests/brewd_source.rs` for discovery, rotation and Rebuild
- [ ] Replace generated fixtures with the Phase 0 captures — same tests

**Ve. Desktop wiring**
- [x] Sources pane: pick journals from `brewd`'s listing instead of pasting URLs —
      Load journals → pick → probe shows the column mapping → Add. Already-added
      journals are hidden, rotated ones tagged. Settings has a brewd "Test
      connection" (`list_journals`). `start_polling` builds each source by `kind`
- [x] v2 streams belong to a physical sensor and outlive batches, which is the case
      `batch_stream` was designed for. Add a vessel → default-streams mapping (Tier 2,
      referencing `stream.id`, never a topic) so `record_pitch` binds the vessel's
      sensors from pitch onward and a status move to `packaged` closes the window —
      `0008_vessel_stream.sql`. A default stream still bound to another batch fails
      the pitch rather than closing that batch's window. `transition_batch` takes
      `at`; packaging closes non-manual bindings only, so a bottle gravity still
      lands. `undo_pitch` drops the non-manual bindings starting at the pitch.
      Minimal Vessels pane for the mapping; batch UI waits for the Phase 3 UI pass
- [x] Footer: `brewd` reachability as its own state, distinct from "no new data" —
      `SyncPhase::Unreachable` for a timeout or connect failure, and
      `SourceStatus.last_error_code` so the UI branches on codes, not text. The
      footer says "brewd unreachable" when every unreachable source is a journal

**Done when:** the Pi logs the probe and the iSpindle with the laptop shut, and
opening the app backfills a gap-free curve bound to the pitched batch.

### Phase 3 UI pass
> Deferred from 3b, 3d and 3e, and overtaken by Phases 4–5, which were built on
> `xtask demo-db` data. The data layer is done; what is missing is every Tier 2
> *write* on the command surface — today the app cannot create an ingredient, a
> recipe or a batch. Lettered `U1`–`U6` in dependency order. Every command gets an
> IPC test in `src-tauri/src/ipc_tests.rs` sending `api.ts`'s argument names, since
> a misspelt `Option` argument arrives as `None` rather than failing.

**U1. Reference data screens** — ingredients, yeast strains, lots
- [x] Migration `0010_reference_archive.sql`: `archived_at` on `yeast_strain` and
      `ingredient_lot`. `ADD COLUMN`, no rebuild. A discontinued strain and a used-up
      lot must leave the pickers while every batch that used them still reads them —
      3a's rule, which these two tables were missed by. Lot consumption is Phase 7;
      until then archiving is how a lot is marked empty
- [x] Edits in `db::reference`: `update_ingredient`, `update_yeast_strain`,
      `update_lot`, plus `restore_*` beside each `archive_*`. Without them a typo is
      permanent — `ingredient.name` is `UNIQUE` across archived rows too, so
      archive-and-recreate under the same name fails
- [x] `update_ingredient` refuses a `default_unit` of another **dimension** once
      any `ingredient_lot`, `recipe_item` or `batch_ingredient` references it — their
      canonical column was chosen by the old dimension, and every `SUM` would read
      grams as litres. Same dimension (`g` → `kg`) is free: the canonical column is
      unchanged. New error code, tested against all three tables
- [x] `update_lot` re-derives the canonical column from the new entry through
      `canonical_for_ingredient`, as `create_lot` does. Allowed on a used lot: cost
      and quantity are read at query time, so a corrected receipt fixes every
      `batch_cost` at once — which is the point of fixing it
- [x] Yeast ↔ strain: an ingredient of category `yeast` requires a
      `yeast_strain_id`, and any other category refuses one. Enforced at the write
      path, not the form: a yeast with no strain is a batch 5a/5d report as
      `no_yeast`, invisible to every strain query. Check the five existing
      `Category::Yeast` call sites in tests before landing
- [x] Strain fractions validated in Rust before the `CHECK` does it: each of
      `attenuation_min`/`max`, `alcohol_tolerance` in 0–1, and `min ≤ max`, with
      their own codes. Today a `78` comes back as a bare `db.sqlite`. The form types
      **percent** and divides by 100 at the edge; the field label says `%`.
      **Refined:** `units.not_a_fraction` and `units.range_inverted`; temps are
      range-checked too
- [x] `UNIQUE` violations map to `db.duplicate` naming the column, not `db.sqlite`.
      The forms say "an ingredient called X already exists" from the code, not by
      parsing the message. **Refined:** `UNIQUE (lab, name)` never fires with no lab
      — SQLite treats NULLs as distinct — so strains are checked in Rust with `IS`,
      and a blank lab is stored as NULL. Names are trimmed and required
      (`db.name_empty`); vessels get `db.duplicate` too
- [x] YAN without the frontend writing JSON: commands take `mg_yan_per_g:
      Option<f64>` and merge it into `attrs` in Rust under `YAN_ATTR`, keeping any
      other key. Shown for `nutrient` only, with `calc::yan`'s DAP / Fermaid K /
      Fermaid O consts offered as presets from a command — `api.ts` gets no copy of
      the numbers. **Refined:** `calc::yan::PRESETS`, Fermaid O twice and labelled —
      datasheet 160, TOSNA 40 — rather than one picked quietly. Edits merge into the
      stored `attrs` in one transaction, so a key the form can't show survives
- [x] Lot cost typed as text (`12,50`) plus a currency, parsed by `Money::parse` in
      the command — one parser, the one with the tests. New config
      `money.default_currency` (`EUR`), in Settings, prefilling the field. Dates as
      whole local days, reusing 5c's day handling in `query.ts`. Unit picker filtered
      to the ingredient's dimension; the server's `DimensionMismatch` stays the
      enforcement. **Added:** a lot's quantity must be positive — a lot of nothing
      has no cost per gram
- [x] Strain temp range typed in the display unit via `fromDisplayTemp`, stored °C
- [x] Commands: `list_ingredients(include_archived)` → row + strain name + YAN + lot
      count; `create_`/`update_`/`archive_`/`restore_ingredient`; the same four for
      yeast strains and lots; `list_lots(ingredient_id, include_archived)` → row +
      `cost_per_canonical_unit` + how many batches used it. That count links to
      `#/query` with the lot filter, 5c's traceability screen. **Refined:**
      `#/query/lot/<id>`, a fresh query preset to that lot. The list prices a lot per
      kg / L / unit, since per gram reads €0.00
- [x] Screen `#/ingredients` (nav: "Ingredients"), tabs Ingredients | Yeast
      strains (`#/ingredients/strains`). Ingredients grouped by category, each
      expanding to its lots and an add-lot form; edit in place; "show archived"
      toggle. A yeast ingredient's form offers "new strain" inline, so the
      requirement above costs no screen change. Vessels stay in Settings. Water
      profiles stay **deferred** — nothing references `water_profile` yet
- [x] Form → payload shaping (percent → fraction, display temp → °C, local day →
      epoch, YAN preset) pure in `lib/reference.ts`, with vitest

**Done when:** from an empty database, a yeast strain, a yeast ingredient linked to
it, sucrose in kg and DAP with its YAN can be created; each gets a lot with a cost
typed as `12,50`; a typo in each is fixed; a used-up lot is archived and leaves the
picker — and 5c's lot filter and 5a's YAN total see all of it.
**Status:** driven in the running app (WebView2 over CDP, Playwright) from an
empty database: strain, linked yeast, sucrose, DAP from its preset, two lots
(`12,50` → €12.50, €2.50/kg), an edit to `2,5` kg, an archive and a restore; each
refusal (150 %, yeast with no strain, duplicate name, kg → L on a used ingredient)
shown by its message. The lot-filter and YAN-total halves need a batch, so they are
covered by tests until U3 can create one from the UI.

**U2. Recipe screen** — list → versions → item editor, lock badge, fork, diff (3b)
- [x] Recipe names trimmed, required and unique (`db.name_empty`, `db.duplicate`),
      sharing U1's helpers, now `pub(crate)` in `db::reference`. `update_recipe`,
      `get_recipe`, `restore_recipe`. Renaming is always allowed; the **beverage type**
      is not once any version is locked (`db.recipe_type_locked`) — `create_batch`
      checked the batch's type against it when it locked the version
- [x] Version targets checked at the write path: OG/FG plausible (as `set_batch_og`),
      FG ≤ OG, ABV a fraction, volume positive. `Sg` deserialises with only a
      finiteness check, so a target typed as `1040` would otherwise reach 4c's expected FG
- [x] `delete_recipe_version`: the newest unlocked version only
      (`db.recipe_version_not_newest`), items by the schema's cascade — the way out of a
      mistaken fork, and numbers stay contiguous. `version_batch_counts` for the badge
- [x] Lines: quantity positive (U1's lot check, now `positive_canonical` for both) and
      `offset_hours` finite; negative is allowed — before pitch
- [x] Commands: `list_recipes`, `create_recipe` (recipe **and** an empty v1, one
      transaction), `update_`/`archive_`/`restore_recipe`, `get_recipe` (versions with
      batch counts), `get_recipe_version` (named lines, `abv_alternate` from the target
      OG/FG), `update_`/`fork_`/`delete_recipe_version`, `add_`/`update_`/
      `delete_recipe_item`, `diff_recipe_versions`. Target gravities cross in
      `ui.gravity_display`, converted by the new `calc::gravity::display_to_sg` — 4d's
      rule, no fit in TS
- [x] Screens: `#/recipes` (by beverage type, show archived), `#/recipe/<id>/v<n>`
      (version tabs, targets, lines by stage, add/edit/delete line, delete draft,
      compare), `#/recipe/<id>/diff/<a>,<b>`. A locked version is read-only with a badge
      "locked · used by N batches" → `#/query/version/<id>` and **Fork to v{N+1}**; a
      write refused as `db.recipe_version_locked` reloads into that state. Form shaping
      pure in `lib/recipe.ts`, with vitest. **Refined:** U1's lot link and this badge
      share one `QueryPreset` on the query route; U1's form styles moved to `app.css` as
      `.ref`
- [x] `xtask demo-db` brews its seltzers off "Citron-Verveine" v1 (sucrose, EC-1118,
      Fermaid O at 24 h and 48 h), so v1 is locked and there is something to fork
- [x] **Found while driving it:** in Tauri `window.confirm` returns a Promise, which
      is truthy, so `if (!confirm(...)) return` never returned — **Rebuild** in Sources
      deleted a source's readings without asking. Both call sites now use
      `lib/confirm.ts` (`plugin-dialog`'s `ask`), with `dialog:allow-ask` granted.
      Confirmed blocking in the running app in U3, on "Undo pitch"
- [x] Calculators (sugar for a target OG, YAN dose and stagger, pitch rate) — **deferred**
      to after U6, once real batches show which ones earn a place. **Landed in U7**

**Done when:** a recipe is created, its targets set in the display unit, lines added
and edited; a locked version refuses edits and forks; the diff of a moved nutrient reads
as one change. **Status:** driven in the running app (WebView2 over CDP) on a
`demo-db` file: each refusal (duplicate name, OG 2.5, FG above OG, zero quantity, type
change on a locked recipe) shown by its message; `4,2` kg; the badge's "used by 3
batches" opening the query preset to v1 (3 rows); fork → v2, 24 h → 36 h, and the diff
showing one *Changed* line plus the FG target; a draft v3 deleted.

**U3. Batch create and lifecycle** — from a version or none, pitch / undo pitch,
transitions, archive (3c, 3d)
- [x] `create_batch` checks before it locks: code trimmed and required
      (`db.name_empty`), a duplicate is `db.duplicate` (was a bare `db.sqlite`), OG
      plausible (`set_batch_og`'s rule), volume positive. A refused batch leaves its
      version editable
- [x] `update_batch` (code, vessel, volume, notes) replaces rather than patches, so a
      volume or a note can be cleared. The vessel is fixed once pitched
      (`db.batch_vessel_locked`): the pitch bound that vessel's sensors. Recipe version and
      beverage type are fixed at create — the version was locked against the type, and
      the way out of a wrong pick is archive and recreate. `restore_batch` beside
      `archive_batch`, which now names an unknown id instead of returning `false`
- [x] **Archiving closes the batch's open sensor bindings**, as packaging does. A batch
      abandoned mid-ferment otherwise held its vessel's sensors forever, and the next pitch
      into that vessel failed with `db.stream_already_bound`. Restore does not reopen
      them: the sensors may have moved on. A pitch backdated to before the archive time
      still overlaps and is still refused — that is the rule working
- [x] A second pitch is `db.batch_already_pitched`, checked before the partial unique
      index, which stays as the backstop. A transition dated before the pitch is
      `db.before_pitch` — `close_sensor_bindings` skips a binding starting after `at`, so
      packaging "before" the pitch would have left the sensors bound. A pitch or status
      time in the future is `db.in_future`, checked in the command, which is the only
      layer that knows what now is
- [x] Commands: `suggest_batch_code`, `create_batch` (OG in `ui.gravity_display`, the
      version lock in the same transaction), `update_`/`archive_`/`restore_batch`,
      `record_pitch(batch_id, at?, note)`, `undo_pitch`, `transition_batch(batch_id, to,
      at?)` — each multi-row write in one transaction. `list_batches` returns
      `BatchSummary`: the row plus recipe name, version number and vessel name, from
      `db::batch::list_batch_summaries`
- [x] Screens: `#/batches` (nav: "Batches"; grouped by status, show archived, archive /
      restore), `#/batches/new` and `#/batches/new/<versionId>` (recipe → version or "no
      recipe"; type from the recipe; code suggested per type until typed over; vessel with
      "the pitch binds N sensor streams"; volume from the version's target; a note when
      creating locks the version). The recipe screen links "Start a batch from vN" from
      every version. `BatchLifecycle.svelte` sits above `BatchView`'s card: status, recipe
      link, edit, pitch with a `datetime-local` and a note, undo pitch (asked), start
      conditioning, mark packaged at a time, archive (asked while fermenting or
      conditioning) / restore. A stale-state code reloads the panel. Form shaping pure in
      `lib/batch.ts`, with vitest; `reference.ts`'s `reader` is now exported for it
- [x] **Refined:** `BatchView` lists archived batches too, marked, so an archived batch
      opens and can be restored; the card and curve are keyed on pitch and status, so a
      pitch redraws them

**Done when:** a batch is created from a recipe version and from none; its version
locks; it is pitched, undone, re-pitched, conditioned, packaged, edited, archived and
restored from the UI; each refusal shows by its message. **Status:** driven in the
running app (WebView2 over CDP) on a `demo-db` file with a vessel mapped to stream 1:
Citron-Verveine forked to v2 and "Start a batch from v2" prefilled SELTZER-008, 20 L and
the lock note; refusals for OG 2.5, the duplicate SELTZER-001, a pitch tomorrow, a pitch
while SELTZER-001 still held the sensor, and packaging before the pitch. Archiving
SELTZER-001 freed the sensor and the pitch bound it; `1,045` stored as 1.045. The undo
dialog **does block** — the batch stayed fermenting with it open, Cancel left it so, OK
returned it to planned and dropped the binding — closing U2's open question. v2's badge
reads locked; CIDER-006 created with no recipe.

**U4. Timeline and quick-entry** — events and manual measurements in one list,
quick-entry form, "use as FG" (3d, 3e)
- [x] `set_batch_gravity(batch, og|fg, Option<Sg>)` sets or **clears** exactly —
      `set_batch_measurements` patches, so a mistaken OG could only ever be replaced.
      Plausible SG, FG ≤ OG either way round (`units.range_inverted`). **An OG that
      post-pitch refractometer readings were corrected against does not move**
      (`db.og_in_use`, naming how many): `derive_gravity` stored values computed from
      it and nothing would recompute them. Delete those readings first. The same OG
      again is not a change. `set_batch_og` goes through it now and takes `null`;
      `set_batch_fg` is new
- [x] `update_event` (time, kind, note), refusing the pitch in both directions — its
      time is `batch.pitched_at` too. `delete_event` and `delete_measurement` now refuse
      an archived batch, as every write before them did. A quick entry with no reading
      and no note is `db.entry_empty`
- [x] `measurement::batch_timeline`: events and manual measurements, oldest first; at
      one `ts` the readings before the events, so a quick entry reads as its numbers
      then its note. The `batch_timeline` command adds each gravity in
      `ui.gravity_display` beside the SG (4d's rule) and the pitch time.
      `BatchDashboard` gains `fg`
- [x] Commands: `batch_timeline`, `record_quick_entry(batch_id, entry)` (`ts` optional,
      a future one `db.in_future`, one transaction), `delete_measurement`,
      `add_`/`update_`/`delete_event` (an edit keeps the stored payload),
      `set_batch_og`/`set_batch_fg` with `null` to clear
- [x] `Timeline.svelte` under the curve: one entry per moment, newest first, with
      "t+N h" from pitch; each gravity says its instrument and, when it differs from the
      value shown, what was typed and which correction ran. Add reading, add event,
      edit event, delete (asked), and "Use as OG" / "Use as FG" on every gravity.
      `QuickEntry.svelte`: time, gravity with hydrometer (scale, sample and calibration
      temp in the display unit, calibration prefilled from config) or refractometer
      (Brix and the wort factor from config), pH, temp, TA, taste note.
      `EventEditor.svelte` for events. Shaping and wording pure in `lib/timeline.ts`,
      with vitest. The card gets an FG row with "Use … as FG" from the current gravity,
      and "clear" on OG and FG
- [x] **Refined:** a manual write raises no `sync://status`, so `BatchView` keys the
      card and curve on a revision counter every write from the screen bumps. The
      duplicate-reading message is reworded in the form — the backend's carries a raw
      epoch

**Done when:** a quick entry with gravity, pH and a note lands as one moment; a
refractometer reading after pitch is refused without an OG and corrected with one; an
OG it depends on cannot be changed until it is deleted; an event is added, corrected
and deleted; a gravity is used as OG and FG and the card shows both. **Status:** driven
in the running app (WebView2 over CDP) on a `demo-db` file: refractometer 6,0 °Bx
refused with no OG, "Use as OG" on the first hydrometer (1.0460), then accepted as
1.0115; "clear" on the OG refused naming the one reading; a quick entry of 1.002 at
25 °C (→ 1.0031), pH 3,2 and "dry, lemony" as one moment; the same minute again
refused in words; 1.0031 used as FG; a cold crash added, edited to fining (marker on
the curve) and the refractometer reading deleted through the dialog, after which the
OG cleared.

**U5. Actuals** — `batch_ingredient` from lots, `plan_vs_actual`, `batch_cost` (3c, 3f)
- [x] `add_batch_ingredient` gets the checks lots and recipe lines already had: a
      positive quantity (`positive_canonical`), and **no new addition from an archived
      lot** (`db.lot_archived`) — archiving is how a used-up lot is marked until Phase 7.
      An existing addition from a lot archived since stays editable; moving it off the
      lot and back is a new pick. A future `added_at` is `db.in_future`, checked in the
      command. `update_batch_ingredient` is new, under the same checks;
      `delete_batch_ingredient` refuses an archived batch and names an unknown id
      (`db.no_such_batch_ingredient`)
- [x] `reference::list_all_lots`, beside `list_lots_for`, for one picker across
      ingredients. `Unit::factor` is `pub` for the command's delta in the planned unit
- [x] Commands: `list_lot_choices` (active lots of active ingredients, with name,
      category and default unit), `batch_actuals` (3c's `plan_vs_actual` named, each
      actual with its lot label and line cost rounded once, `delta_entry` in the planned
      line's unit, and `batch_cost` — a currency mismatch comes back as `cost_error`
      beside the lines rather than failing the whole view),
      `add_`/`update_`/`delete_batch_ingredient`
- [x] `Actuals.svelte` between the curve and the timeline: the plan against the
      actuals by stage, each line planned / actual (lot, time) / delta ("+0.2 kg", "on
      plan") / cost, flagged *missing* or *unplanned*; **Record** on a missing line
      prefills its lot, quantity and stage; add an unplanned addition from any lot; edit
      and delete (asked). Unit picker filtered to the lot's dimension. Footer: total, per
      litre over the volume it used, and how many lines are unpriced. Shaping and wording
      pure in `lib/actuals.ts`, with vitest
- [x] **Found while driving it:** pairing with the plan goes by order within
      (ingredient, stage), and an untimed addition sorts first — so the second of two
      Fermaid O doses recorded with no time paired with the *first* planned dose, and the
      delta landed on the wrong line. **Record** now prefills the planned time, pitch
      plus the line's offset, capped at now
- [x] `xtask demo-db`: priced lots of sucrose (€12.50 / 5 kg) and Fermaid O
      (€18.90 / 250 g, 40 mg YAN/g), and SELTZER-001's actuals — 4.2 kg sucrose against
      the plan's 4, and the first Fermaid O dose only — so the table opens on a delta and
      one missing line

**Done when:** a planned addition is recorded from its line, an unplanned one added,
each refusal shown by its message, an edit moves the delta and the cost, and a deletion
takes both back. **Status:** driven in the running app (WebView2 over CDP) on a
`demo-db` file: the table opened on sucrose +0.2 kg (€10.50), EC-1118 on plan, one
Fermaid O on plan and one missing, cost €11.26 · €0.56/L over 20 L · 1 unpriced (the
yeast lot). 0 g refused; the missing dose recorded as 12 g at its planned +48 h (+2 g,
€0.91); 250 g sucrose at packaging as unplanned (€0.63); sucrose edited to 4 kg (on
plan, €10.00); the priming addition deleted through the dialog. With the sucrose lot
archived a new addition was `db.lot_archived`, while the existing line still edited to
4.1 kg with the lot shown "(archived)". 5a's YAN and the lot filter read
`batch_ingredient` and are covered by their tests; not re-checked on screen.

**U6. Batch screen** — status card, curve, timeline and actuals on one page,
replacing `BatchView`'s stand-in
- [x] `get_batch` returns `BatchSummary`, the list's row: `db::batch::get_batch_summary`
      shares one query with `list_batch_summaries`. The screen loads its one batch
      rather than every batch to find it; an unknown id is "No batch N" with a link back
- [x] `BatchHeader.svelte`: "← Batches", the code, the status, edit, archive (asked
      while fermenting or conditioning) / restore, export readings and "compare with…";
      under it the recipe vN link, vessel, volume, the day it is on while in the vessel
      and the pitch time (`headerFacts` in `lib/batch.ts`, with vitest). The picker is
      gone — the batches list and home are the way in. `BatchLifecycle` keeps only the
      status moves and renders nothing once there are none (packaged, archived)
- [x] Layout: wide, the curve and actuals in the main column beside the card, the move
      forward and the timeline — so "Add reading" sits under the numbers it changes;
      narrow (< 1100 px), one column in the order card, move, curve, timeline, actuals
- [x] **Refined:** the card and curve refetch in place on U4's revision counter instead
      of being remounted on it, so a reading, an event or an actuals edit keeps the
      curve's zoom. Remounted only when the batch changes
- [x] An archived batch is read-only everywhere: the card's OG/FG use and clear are
      hidden too (`readonly`), not only the lists'. A planned batch shows "Not pitched
      yet: the curve starts at the pitch" in place of an empty chart and its toolbar

**Done when:** a batch opens from home and from the list onto one page; a write from
anywhere on it updates the card, curve and lists without losing the zoom; a planned batch
is pitched from it and its curve starts; archived, it offers nothing to write; an unknown
id says so. **Status:** driven in the running app (WebView2 over CDP) on a `demo-db`
file: home's first card opened ALE-003 and the list SELTZER-001; at 1400 px the card,
move and timeline sat right of the curve and actuals, at 800 px they stacked in the
narrow order. Zoomed, then a pH reading and an actuals edit: each `batch_series` refetch
carried the zoomed range, double-click sent `null`, the card's pH moved to the new
reading. "Use as OG" on the first hydrometer set 1.0460 on the card. A new planned
SELTZER-U6 showed the pitch message and no toolbar; pitched, its header read "10 L · day
1 · pitched …" and the curve's toolbar appeared. SELTZER-001 archived through the dialog
left only Restore, Export and the curve's axis toggle; restored, every control came
back. `#/batch/999` read "No batch 999." No console errors.

**U7. Calculators** — sugar for a target OG, YAN dose and stagger, pitch rate (U2's
deferral, 3f's `calc`)
- [x] Five commands, none touching the database: `calc_defaults` (3f's demand factors,
      `TOSNA_DOSES`, `RATE_ALE`/`RATE_LAGER`, `DRY_CELLS_PER_GRAM` — `api.ts` holds no
      copy, the `yan_presets` rule), `calc_sugar_for_og`, `calc_og_from_sugar`,
      `calc_yan` (`target_ppm` → `total_mg` → `product_grams` → `stagger`) and
      `calc_pitch`. Gravities in `ui.gravity_display`, U2's rule. A typed SG outside
      `Sg::PLAUSIBLE` is `db.measurement_implausible`: `display_to_sg` passes an SG
      through unchecked, so `1075` typed for 1.075 would otherwise reach the sums
- [x] Nothing defaults what `calc` refuses to default: the demand factor and the nutrient
      yield are picked from presets or typed, and viability is typed. `calc_pitch`
      returns grams only when cells per gram **and** viability are given; a blank
      viability says "type a viability for grams", and the hint says to type 100 for an
      unknown packet age and read the answer as a minimum (`dry_yeast_grams`' doc)
- [x] Sugar is two directions, sugar for an OG and the OG of a weighed sugar, and the
      volume is labelled **water added** — `og_from_sugar`'s argument, not the finished
      volume. Doses are N equal fractions, TOSNA's four by default; their timing stays in
      the recipe's `offset_hours`. The nutrient picker lists the user's own nutrient
      ingredients with a YAN figure first, then `calc::yan::PRESETS`. A strain picked
      in the pitch panel decides dry or not by its `form`, nothing else
- [x] Screen `#/calc` (nav: "Calculators") and `#/calc/version/<id>`, prefilled from
      the version's target OG and volume; the recipe screen links "Calculators for vN"
      beside "Start a batch from vN". Each panel recomputes on input, a stale answer
      dropped by sequence number. Form shaping and wording pure in `lib/calc.ts`, with
      vitest
- [ ] Blend / dilute (`calc::blend`, `dilute`) and "add as recipe lines" — **deferred**
      until a batch needs them; the second is a write, and the screen is read-only

**Done when:** from a recipe version, the calculators open prefilled; each worked example
in 3f reads back on screen; a refused input shows its message; the display unit is
honoured both ways. **Status:** driven in the running app (WebView2 over CDP) on a
`demo-db` file: "Calculators for v1" on Citron-Verveine opened `#/calc/version/1`
prefilled 1.038 and 20 L; 1.075 / 20 L / high / DAP read 245 mg/L · 4.89 g N · 23.1 g,
four doses of 5.77 g; 1.048 / 20 L / ale read 179 billion cells, then 29.8 g at 100 %;
viability 0, a dose count of 2.5 and an SG of 1075 each refused by message; 4,2 kg in
20 L → OG 1.0714. In °P the prefill read 9.5098, 20 °P in 4 L of water 998.23 g, and
1 kg back to 20.0 °P (1.0831). Nav opened blank. No console errors.

**Phase 3 UI pass done**, calculators included.

### Phase 4 — Dashboard & curves
> Starts after the Phase 3 UI pass: the batch screen, recipe screen, timeline and
> quick-entry form deferred from 3b/3d/3e. Phase 4 adds curves and live state to
> that batch screen; it does not build a second one. No hardware needed — every
> item here runs on Vd's generated journal fixtures.

**4a. Series query** — `db/series.rs`, no Tauri
- [x] `batch_series(conn, batch_id, metrics, range, max_points)`: readings for every
      stream bound through `batch_stream`, clipped to each binding's half-open
      `[starts_at, ends_at)`. A reading outside every window is not the batch's,
      even from the same stream — that is what lets batch 17 and 18 share a
      fermenter. `range` defaults to `[pitched_at, now)`; a planned batch with no
      pitch has no t=0, so its default is its bindings' span (all bindings, not
      only the requested metrics', so two charts of one batch line up). No pitch
      and no bindings is `range: None`. `max_points` is per series; 0 is
      `units.not_positive`
- [x] One series per **stream**, not per metric. An iSpindle `sg` and a manual
      hydrometer `sg` on one batch are two series on one axis — merging them
      hides the drift between the two instruments that the chart exists to show.
      Each series carries `stream_id`, `metric`, `source.kind` and a label
      (`stream.label`, else the source's name). Columnar `ts` / `avg` / `min` /
      `max`, uPlot's own shape
- [x] Downsample in SQL: `GROUP BY ts / bucket` returning `MIN`, `MAX`, `AVG` and
      the bucket's first `ts`, where `bucket = span / max_points`. It rides
      `reading`'s clustered `(stream_id, ts)` scan, and the IPC payload is what
      costs, not uPlot. **Min/max per bucket, not LTTB**: LTTB picks points that
      look right and can drop the one spike that says a probe fell out of the
      thermowell. Below `max_points` raw rows, return them unbucketed — buckets are
      aligned to the range start, and one straddling two windows merges into one
      point. A test pins `EXPLAIN QUERY PLAN` to `SEARCH … USING PRIMARY KEY`. A
      60-day 30 s journal to 1000 points: ~56 ms (release, in-memory)
- [x] Gaps survive downsampling. An empty bucket is a `null`, never skipped, so
      uPlot breaks the line instead of drawing a straight ramp across a week the Pi
      was off. A gap is "no reading for > 3× the stream's median interval", the
      same rule 4f's staleness uses — **refined:** zoomed in past a device's
      interval most buckets are empty, so a run of empty buckets becomes one `null`
      only when the readings either side are further apart than the rule allows.
      The null takes an empty bucket's slot, so the count stays ≤ `max_points`. The
      median is over the batch's last 257 readings of the stream, not the zoomed
      range, so zooming doesn't move the gaps. `median_interval` is `pub` for 4f
- [x] Manual streams are never bucketed — a dozen points over a month is the whole
      record, and averaging two pH readings invents a third
- [x] Tests: extremes survive bucketing, point count ≤ `max_points`, a gap stays a
      gap, a reading one second past `ends_at` is excluded, two bindings of one
      stream to one batch (3d's unplugged-and-reconnected case) read as one series
      with a gap — `db::series::tests`, plus 17/18 turnaround, iSpindle + manual
      `sg`, default ranges, and an `#[ignore]`d 60-day timing

**4b. Smoothing** — `calc/smooth.rs`, pure, `&[(Timestamp, f64)]` in and out
- [x] Rolling **median** over a centred time window, `SG_WINDOW_DEFAULT` = 2 h,
      then nothing else. A median drops a single wild tilt (the iSpindle bumped, a
      CO₂ bubble on the hull) that a mean or EMA would smear across the window.
      Time-based, not count-based: the iSpindle's interval is a device setting and
      changes
- [x] Computed on read, never stored. Tier 1 stays the raw journal, so a better
      filter later reapplies to every past batch for free
- [x] Applied to `sg` from any sensor stream — `source.kind` isn't "iSpindle", and a
      Tilt through Fermentrack wants the same. Temperature from a DS18B20 doesn't
      jitter, and smoothing manual readings is 4a's invented-point problem
- [x] **Smoothed before bucketed.** A bucket's `AVG` already smears the tilt the
      median exists to drop, so smoothed streams are fetched raw and bucketed in
      Rust, and `Series.smoothed` is the per-bucket mean of smoothed values. Fetched
      half a window past each end of the range, so a zoom edge isn't a false
      `partial`; each run between binding edges and gaps is smoothed on its own. A
      60-day 60 s iSpindle to 1000 points: ~27 ms (release, in-memory)
- [x] Data shape for 4d: `smoothed` and `smoothed_partial` beside the raw columns.
      The chart draws smoothed as the line and raw as faint points behind it —
      hiding raw makes a smoothing bug invisible
- [x] Tests: a single outlier is removed, two adjacent too; a real step (a dry-hop, a
      dilution) survives past half a window; edges with less than half a window are
      marked `partial`, not extrapolated. **Refined:** a median of the ~8 readings a
      15 min iSpindle gives in 2 h rejects outliers but barely averages jitter —
      uniform ±0.002 comes out ~0.0006 RMS, so "flat within 0.0005" can't hold. The
      test is ±0.002 at 5 min: RMS ≤ 0.0005, worst ≤ 0.0015. Averaging jitter down is
      4c's 24 h least-squares fit. `calc::smooth::tests`, plus `db::series::tests`
      for the wiring: tilt in `max` but not `smoothed`, zoom edges vs binding and gap
      edges, and a smoothed series in the 60-day timing

**4c. Stability detector** — `calc/stability.rs`, the function Phase 6's alert calls
- [x] Implements *Fermentation-finished detection* below: slope of a least-squares
      fit over a trailing window of the **smoothed** series, below a threshold,
      **and** SG within a band of expected FG. Returns `Active`, `Stable { since }`,
      `Stalled { since }` (flat but above the band) or `Insufficient { reason }`.
      **Refined:** the band is compared against the fit's value at the newest point,
      not the newest reading — the fit is what averages jitter. One-sided: finishing
      drier than expected is still finished. A rising SG (sugar in) is `Active`
- [x] Expected FG, in order: `recipe_version.target_fg`, else `batch.og` and the
      yeast strain's `attenuation_min`/`attenuation_max`, else none. None is
      `Stable` with `fg_unchecked` set, shown as such — a batch with no recipe
      still finishes, it just can't be told apart from a stall. The strain is from
      **actuals** (5a's rule), else the recipe's; two strains take the widest band,
      and one without a band makes it none. `db::stability::expected_fg`
- [x] `Insufficient` when the window is under-filled: ≥ 6 points **and**
      spanning ≥ 75 % of the window. A fit through three points after a gap is
      noise with a slope. **Added:** `NotStarted` until the smoothed SG is 0.002
      below OG (else the first reading) — 5a's end of lag, latched
- [x] "Stable for N h" is the time since the slope last crossed the threshold,
      found by walking the history — not the time since the app noticed.
      **Refined — hysteresis:** going flat takes the threshold, leaving it takes 4×
      (`LEAVE_FLAT_FACTOR`). A day's fit through 4b's median of ±0.002 at 5 min
      still wanders 0.00036 SG/day RMS and for hours at a time: over 32 seeds × 8
      flat days it passed 1× for up to 13 h and peaked at 0.0017. At 15 min it peaks
      at 0.0022, past 4× — the 0.0005 default is under what that iSpindle resolves,
      so the first batches should set it
- [x] Config: `stability.window_hours` (24), `stability.slope_sg_per_day` (0.0005),
      `stability.fg_band` (0.003), validated like 3e's `measurement` section —
      bounds 6–168 h, 0.0001–0.01, 0.0005–0.020. `Stability::params()` hands them to
      `calc`, which never reads the config
- [x] "Log every evaluation" becomes **replay**: the detector is pure over history,
      so `stability_history(series, config)` evaluates it at every point and the
      chart shades the stable spans. Retuning a threshold shows at once where it
      would have fired on every past batch, which a log of past evaluations can't.
      `db::stability::batch_stability` feeds it the newest sensor `sg` stream, raw
      and smoothed (4a with `max_points` unbounded); manual-only is `Empty`. An
      O(1)-per-point sliding fit, pinned to a direct one over 60 days at 30 s
- [x] Tests on generated curves: lag → active → plateau reads `Stable` at the right
      hour; a plateau 10 points above expected FG reads `Stalled`; a jittered
      plateau never flips to `Active`; the lag phase before attenuation starts is
      not `Stable`, which is the case a slope-only detector gets wrong —
      `calc::stability::tests`, plus `since` asked late, refilling after a gap, and
      `db::stability::tests` for the joins and a stalled batch end to end

**4d. Chart** — `src/lib/components/Curve.svelte`
- [x] Add `uPlot`, pinned like every other dependency. No install script, so no
      `allowScripts` entry. 1.6.32
- [x] Axes: gravity left, temperature right, pH a third axis shown only when the
      batch has pH readings. Temp covers `temp_c`, `fridge_temp_c`, `room_temp_c`,
      `gravity_temp_c` and `setpoint_c` — one scale, since they're compared against
      each other. `battery_v`, `rssi_dbm`, `tilt_deg`, `state` stay off this chart.
      Sensor `sg` draws 4b's smoothed line over faint raw points; a bucketed series
      adds its min/max band, so a spike survives downsampling. Manual readings are
      dashed with points, never banded. The shaping is pure, in `lib/chart.ts`.
      `fridge_setpoint_c` isn't charted
- [x] Gravity display unit: SG → °P / Brix is a `calc` fit, and `units.ts`
      deliberately has no copy. **Refined:** converted in the `batch_series`
      *command* via `BatchSeries::in_gravity_display`, not in `db::series` —
      4c and 5e read that in SG. It returns `gravity_unit` for the axis label.
      Temperature converts client-side through `toDisplayTemp`, pulled out of
      `formatTemp`, so ticks land on round °F
- [x] Event markers: every `batch_event` as a vertical line with its kind; pitch
      drawn as t=0. Hover shows the note. **Added:** 4c's stable and stalled spans
      shaded behind the series. `batch_stability` returns `flat_spans(history)`,
      not the per-reading history, which is too big for IPC
- [x] X axis toggles absolute time ↔ hours since pitch. The second is Phase 5's
      overlay axis, built here so the two agree (`toX`/`toAbs`)
- [x] Zoom refetches at the new range with the same `max_points` — the bucket
      shrinks, detail appears, the payload doesn't grow. **Refined:** driven from
      uPlot's `setSelect` with `drag.setScale` off, and the range set explicitly
      after each `setData` — an x `range` function is reapplied on every x change,
      a drag's included. Double-click resets to the default range
- [x] Live: listen on `sync://status` and refetch when `rows_added > 0` for a
      source bound to this batch. No UI timer — sync already is the timer.
      **Refined:** `Series` carries `source_id` to match on. One fetch in flight; a
      burst of events is one refetch after it
- [x] **Added:** `list_batches`, `get_batch`, `batch_series`, `list_batch_events`,
      `batch_stability` commands. `BatchView.svelte` is a stand-in host (picker +
      curve) until Phase 3's batch screen. `cargo xtask demo-db [path]` writes a
      60-day batch — 30 s ESP32 temps, 15 min iSpindle with spikes, a 3-day gap, a
      cold crash, manual SG/pH, events — for `BREWAPP_DB=<path> npm run tauri dev`

**4e. Status card and batch dashboard**
- [x] `batch_dashboard(batch_id)` in `brewcore`, one command, one struct: the
      ARCHITECTURE.md card. Computed in Rust so Phase 6's `brewd` alert and the
      screen read the same numbers. **Refined:** `db::stability::judge` returns the
      judged sensor series too, so stability and current SG come from one fetch and
      can't disagree. `Sg` fields stay SG; `gravity_in(display)` gives a separate
      `display` object, since the "use as OG" write-back needs the SG
- [x] OG from `batch.og`. Missing → "no OG", with 3e's "use this as the OG" button
      next to the earliest gravity. Never inferred from the first iSpindle point:
      the lag phase and a stuck-on-the-wall iSpindle both read as OG. **Refined:**
      the candidate is the first manual gravity, else the first raw sensor reading,
      labelled so the user sees what they accept. **Added:** `set_batch_og` command
- [x] Current SG: latest smoothed iSpindle value, else latest manual. Labelled with
      its instrument and age — a hydrometer from yesterday and an iSpindle from
      two minutes ago are different claims
- [x] Est. ABV and apparent attenuation from OG and current SG, via
      `calc::abv::alternate` and labelled "alternate" (3f's rule: no unlabelled
      ABV). Refused below OG − 0.002 so the lag phase doesn't show 0.1 %.
      **Refined:** an `Estimate` enum — `estimated` or why not (`no_og`,
      `no_gravity`, `not_started`, `og_too_low`) — so the card always says which
- [x] pH and temp: latest reading of each, with age. pH is manual and days old is
      normal; temp older than the staleness rule is shown struck through.
      **Refined:** temp prefers a `temp_c` sensor, then `gravity_temp_c`, then
      manual. **Added:** `series::stale_after`, `latest_readings` and
      `STALE_FLOOR_SECS` for 4f to reuse. The card refetches on every
      `sync://status` for its sources, not only `rows_added > 0`: a poll that adds
      nothing is when temp goes stale
- [x] Stability from 4c, with `since` and the expected-FG source
- [x] Tests: each field against a fixture batch, including a batch with no OG, no
      recipe, no bindings, and one with only manual readings. **Added:**
      `StatusCard.svelte` above the curve in `BatchView`; wording in
      `lib/dashboard.ts` with vitest

**4f. Home view**
- [x] Route: the app has one screen. Hash routing (`#/`, `#/batch/17`,
      `#/settings`) in a few lines, no router dependency, unless the Phase 3 UI
      pass already added one. **Added:** `lib/route.ts` with vitest; unknown hashes
      fall back to home. `BatchView`'s picker navigates instead of holding state
- [x] `home_summary()`: one card per batch that is `fermenting` or `conditioning`
      and not archived — code, vessel, current SG, temp, stability, newest reading
      age. Click through to the batch. **Refined:** each card carries the batch's
      `dashboard_at`, so home and the batch card can't disagree. The screen
      refetches on every `sync://status`, like 4e's card. Quiet streams are listed
      one line per device, naming the streams only when some are still reporting
- [x] Stale per **stream**: no reading for > 3× its median interval over the last
      day, floor 10 min. Derived, not configured — a 30 s ESP32 and a 15 min
      iSpindle can't share a threshold, and the interval is a device setting.
      **Refined:** `series::stream_freshness` only judges open bindings — a closed
      binding is history, not silence. A stream's newest reading is its own, not
      the binding window's: the device is what went quiet
- [x] Stale is not unreachable. Ve's `SyncPhase::Unreachable` says the Pi is down;
      a stale stream with `brewd` reachable says the *sensor* is silent. The card
      says which, from `last_error_code`, not from text. **Refined:** from `phase`,
      not `last_error_code` — a refused connection and a 500 both carry
      `fermentrack.http`, and `phase` already made the call. A third state,
      `sync_error`: with the sync failing the app never saw the readings, so it
      won't call the sensor silent either. Judged against the last settled status,
      so a retry's `syncing` doesn't flicker it
- [x] Device health per journal source, not per batch — an iSpindle is a device:
      battery voltage with a warning threshold (proposal 3.6 V, config), RSSI
      (proposal warn below −80 dBm) and a 7-day battery sparkline. Shown on the
      batch card too when the device is bound to it. **Added:** `[health]` config
      (`battery_warn_v` 3.0–4.2, `rssi_warn_dbm` −100…−50) in Settings;
      `db::health`; the sparkline is ≤ 84 bucketed points via
      `series::stream_buckets`
- [x] A silent iSpindle with a last battery near the threshold says "battery
      likely"; one with healthy battery and falling RSSI says "signal". Two cheap
      heuristics, labelled as guesses — the gravity series alone can't tell a dead
      device from a finished ferment. **Refined:** `calc::health::guess` — "near"
      is within 0.1 V above the threshold; "falling" is the latest RSSI below the
      warning or ≥ 10 dB under its 7-day median. Battery wins when both hold.
      `xtask demo-db` now runs the iSpindle's battery down and silences it for the
      last 12 h, so the path is visible

**Done when:** opening the app shows every active batch with current SG, temp and
stability; clicking one shows a responsive multi-axis curve over the full ferment
with events marked; and a 60-day, 30 s journal stays interactive when zoomed.

### Phase 5 — Analytics
> Every metric here is computed from 4a–4c on read, not stored. With tens of
> batches a full recompute is milliseconds, and a changed definition (what counts
> as lag) then applies to every past batch at once instead of leaving a table of
> numbers computed under an old rule. A `batch_metrics` cache is the fallback if
> that stops being true, not the plan.

**5a. Per-batch metrics** — `calc/batch_metrics.rs` (pure) + `db/analytics.rs` (joins)
- [x] Lag time: pitch → first time the smoothed SG is 0.002 below OG. Needs OG and
      pitch; `None` with a reason otherwise. **Refined:** read off the stream 4c
      judges (`db::stability::judge`), so lag, peak and terminal can't disagree
      with the batch card
- [x] Peak rate: steepest least-squares slope over any 6 h window, SG/day, and the
      hour it happened. **Refined:** a window counts only under 4c's fill rule
      (≥ 6 points spanning ≥ 75 %), so a fit across a gap isn't a false peak; the
      hour is the midpoint of the window's points, where a least-squares slope
      applies. 4c's sliding fit moves to `calc::fit::Sums` for both
- [x] Days to terminal: pitch → the `since` of 4c's first `Stable`. A batch never
      stable has `None`, not "still running" — archived batches are compared too.
      `fg_unchecked` carries through
- [x] Apparent attenuation and ABV from `batch.og`/`batch.fg` (not the live SG), so
      finished batches compare on their recorded numbers. The field is
      `abv_alternate`, per 3f's labelling rule
- [x] Fermentation temp: **median** `temp_c` over `[pitch, terminal)`, plus min and
      max. Median so the cold crash at the end doesn't drag it; the range shows a
      controller that swung. **Refined:** a sensor `temp_c`, else `gravity_temp_c`,
      never manual — spot checks aren't a ferment temperature; of several, the
      stream with most readings. Never stable → up to now, with `to_terminal: false`
- [x] Nutrient total (YAN mg/L from `batch_ingredient`), cost per litre from 3f's
      `batch_cost`, yeast strain from **actuals** (`batch_ingredient` → lot →
      `ingredient.yeast_strain_id`), not the recipe — a substituted yeast is the
      exact case the analysis is for. **Refined:** a nutrient's yield is the
      `ingredient.attrs` key `mg_yan_per_g` (`reference::YAN_ATTR`), not a column
      — `0002` reserves `attrs` for exactly this. One addition without a yield
      makes the total unknown, not a partial sum: an understated total reads as
      underfeeding. YAN and cost share `batch::target_or_batch_volume`. Strains
      come from `stability::pitched_strains`, shared with 4c's expected FG
- [x] Every metric is `Option` plus a reason: "manual readings only" (too sparse
      for lag or peak rate), "no pitch", "no OG". A table that prints 0 for
      "couldn't compute" poisons every average in 5d. **Refined:** `Computed<T>`,
      `{"value":…}` or `{"missing":"no_og"}`. **Added:** `mixed_currency` — a
      mistyped lot currency is a reason, not an error that sinks the whole table.
      `db::analytics::{batch_metrics, list_batch_metrics}`; no command yet, 5c's
      results table is the first screen
- [x] Tests: each metric against a generated curve with a known lag, peak and
      plateau; a manual-only batch returns reasons, not numbers. **Fixed:**
      `batch_series` with `max_points` unbounded overflowed on a non-`sg` stream

**5b. Batch comparison overlay**
- [x] Pick 2–8 batches; x = hours since pitch, from 4d. Unpitched batches are
      listed as excluded with the reason rather than silently missing. **Refined:**
      counted after duplicates are dropped, `compare.batch_count` outside 2–8;
      kept in pick order, and colour follows the batch (a newcomer takes the
      lowest free slot), so removing one never repaints the rest
- [x] Y mode: raw SG, or **fraction of apparent attenuation** `(OG − SG)/(OG − 1)`.
      Raw SG is unreadable across a 1.030 and a 1.050 wash; the fraction puts both
      on 0 → ~1. Needs OG; batches without it drop to raw with a note. **Refined:**
      computed in Rust on SG, before the display conversion
      (`calc::attenuation::apparent_progress`). Not clamped and not refused above
      the OG — lag-phase jitter reads slightly negative, a seltzer past 100 %. The
      transform is decreasing, so a bucket's `min`/`max` swap. An OG under 1.002
      is `implausible`, not `no_og`
- [x] Optional temp overlay on the right axis, same batches. **Refined:** its own
      panel, not a right axis — attenuation, SG and °C are different measures, so
      each is a stacked chart on one shared x with a synced cursor and zoom. A
      batch left raw in fraction mode gets the SG panel the same way. One stream
      per batch, by 5a's ferment-temperature rule (`ferm_temp_series`)
- [x] Reuses `batch_series` per batch with the same `max_points`, and 4b's
      smoothing. Nothing new server-side except the alignment. **Refined:** the
      alignment is one range in *seconds since pitch* (`RelRange`), queried per
      batch at `[pitch + start, pitch + end)`; `ts` stays absolute and the client
      maps it with 4d's `toX`. Default span is the data — the latest binding end
      (open → now) minus pitch — not pitch to now, or an archived batch would be
      a year bucketed at 9 h. Drawn: smoothed line and manual points only, no raw
      points or min/max bands — eight of each is noise; the batch screen keeps
      them. `db::compare::compare_batches`, command `compare_batches`
- [x] Entry points: from the batch screen ("compare with…") and from any 5c result.
      **Done:** the batch screen, `#/compare/<ids>`; 5c's "overlay these" navigates
      there with the ticked rows

**5c. Saved queries** — `0009_saved_query.sql`, Tier 2
- [x] A structured `BatchFilter`, **not SQL text**: beverage type, recipe, recipe
      version, yeast strain (actuals), ingredient lot, OG range, FG range, ferm
      temp range (5a's median), pitched between, status, include archived. Stored
      SQL would couple saved queries to the schema and break on the next migration.
      **Refined:** canonical units only — SG, °C, epoch seconds — so a saved query
      means the same after a display-unit change; the form types temperature in
      the display unit and pitch dates as whole local days. Ranges are inclusive,
      either end open; `min > max` is `query.invalid_filter`. A list criterion is
      "any of", and a mixed-strain batch matches each of its strains
- [x] `saved_query (id, name, filter JSON, created_at, archived_at)`. The JSON
      carries `"v": 1`. An unknown or retired field on load is an error — silently
      dropping one criterion *widens* the result, and a wider result looks right.
      **Refined:** a missing field is refused too (no `serde(default)`), and a
      version other than 1 is `query.filter_version`. Names are unique among
      active queries (partial index), so an archived name is free again. The list
      returns summaries without parsing filters: one unreadable query fails when
      opened, not the whole list. Update replaces name and filter in place
- [x] Filtering runs in Rust over 5a's computed metrics, after a SQL prefilter on
      the columns that exist (type, status, dates, OG). The ARCHITECTURE.md
      example — "EC-1118, OG 1.035–1.040, 18 °C" — is the acceptance test.
      **Refined:** the prefilter also takes recipe, version, lot and FG. A NULL
      OG / FG / pitch passes it on purpose: a batch the filter can't judge is
      listed as `unevaluated` with the criterion and the reason (**added**
      `no_yeast`), never dropped and never matched. `db::query::run_query`,
      commands `run_batch_query` / `run_saved_query`
- [x] Result is the 5a table, sortable, with "overlay these" (5b) and "export" (5e).
      **Refined:** missing values sort last in either direction and show "—" with
      the reason on hover. A result of ≤ 8 starts with its pitched batches ticked.
      OG / FG are converted to the display unit in Rust (`display_og`/`_fg`).
      Export is a disabled button until 5e. Screen `#/query`, `#/query/<id>`
- [x] Lot filter answers "which batches used that suspect yeast lot" — 3a's
      traceability question, finally a screen. **Refined:** the picker
      (`query_options`) offers only lots some batch used, with the count, and
      recipes by their locked versions only

**5d. Yeast / strain rollup** — `db/rollup.rs`, no migration
- [x] Group 5a's metrics by actual strain: n, median and range of attenuation,
      lag, days to terminal, ferm temp. Median, not mean — at n = 4 one stuck
      batch owns the mean. **Refined:** each metric is a `calc::batch_metrics::Spread`
      over the batches that have it, so its own `n` shows how many of the row's
      did; none is `null`, never 0. Ferm temp is the median of each batch's
      median. Rows by n, then name. `db::rollup::{group, strain_rollup}`
- [x] Measured attenuation beside the strain's own `attenuation_min`/`max` band,
      flagged when the median falls outside it — either the strain record is
      wrong or the process is. **Refined:** `band_check` is `below`/`within`/
      `above`, both directions flagged; only with both ends recorded
      (`attenuation_band`), as 4c's expected FG. A seltzer past a wine-yeast band
      is the finding, not noise
- [x] A batch with two strains goes to a "mixed" row, never counted twice.
      **Refined:** one mixed row for all of them, listing every strain involved,
      with no band. A matched batch with no yeast recorded is `no_yeast`, listed
      under the table, not dropped
- [x] n < 3 is shown greyed out, not hidden. **Refined:** `thin` is computed in
      Rust (`MIN_ROLLUP_N`), so 5e's export carries it
- [x] Filterable by the same `BatchFilter` as 5c, so "EC-1118 on seltzer only" is
      the same code path. **Refined:** the rollup rides on 5c's result —
      `QueryView.rollup`, computed in the same read from `run_query`'s rows — so
      the two views can't disagree and 5c's unevaluated batches aren't counted.
      A "Batches | By strain" toggle on `#/query`, no new command or route

**5e. CSV export** — `brewcore::export`, `csv` crate, one writer
- [x] Export is the **same query** as the screen plus a writer, never a scrape of
      the rendered table: metrics table, rollup, overlay series, and a batch's raw
      readings (4a with `max_points` unbounded). **Refined:** series are long —
      one row per point, a `metric` column — since two streams of one metric or
      unaligned timestamps break a wide layout. The overlay export is 5b's
      `compare_batches` with the screen's ids, y mode, temp toggle and zoom, every
      point; excluded batches are rows carrying their reason. A batch's readings
      are the whole bound record (`binding_span`), every metric, not the chart's
      zoom — pre-pitch readings get a negative `secs_from_pitch`. The metrics
      file appends 5c's unevaluated batches (`match`, criterion, reason) and the
      rollup its `no_yeast` row: nothing the screen shows is dropped. Lists are
      joined with ` | `. Rows in Rust's order, not the UI's sort. Commands
      `export_query` / `export_comparison` / `export_batch_readings`
- [x] Values in canonical units with the unit in the header (`temp_c`, `sg`), both
      `ts` (epoch) and `time_utc` (ISO 8601) columns. Display units are for eyes;
      a file gets re-imported. **Refined:** a metric that couldn't be computed is
      an empty cell plus a `*_missing` column with the reason, never 0. Numbers
      never use an exponent; non-finite is empty. Cost is `Money::decimal` with
      the currency in its own column
- [x] Dialect: RFC 4180 by default, and an `excel_fr` option — `;` separator, `,`
      decimal, UTF-8 BOM — since French Excel opens a comma CSV as one column.
      **Refined:** `export.dialect` in config (`rfc4180` / `excel_fr`), set in
      Settings, no per-export picker. The decimal comma applies to number cells
      only; a note's `.` is untouched. CRLF in both
- [x] Save dialog via `tauri-plugin-dialog`; the path goes to a command that
      writes it. The frontend never touches the filesystem. **Refined:** only
      `dialog:allow-save` is granted. Cancel writes nothing. The write is
      flushed and fsync'd; failure is `export.write` naming the path. Export on
      `#/query` (either view), `#/compare`, and the batch page
- [x] Tests: round-trip through the `csv` reader for each dialect; a note with a
      `;`, a `,` and a quote survives both. **Refined:** plus a newline and a
      non-ASCII name; IPC test writes all four exports in `excel_fr`

**Done when:** "all EC-1118 seltzers, OG 1.035–1.040, fermented at 18 °C" is a
saved query that returns the right batches with lag, peak rate and days to
terminal, overlays them aligned on pitch, and exports to a CSV that opens
correctly in French Excel.

### Phase 6 — Alerts & automation rules
> **v2:** rules and profiles run in `brewd`, not the desktop app. An alert that
> only fires while the laptop is open doesn't fire at 3 am, and a 09:00 ramp can't
> depend on someone having the app open. The rule engine is `brewcore` code, so
> the move is about where it runs, not a rewrite.
>
> **Temp control is deferred** until everything else is built and running:
> relays, the ESPHome `thermostat`, the `Controller` trait and profiles. Alert
> rules need no relays and can land before them.

**Built (2026-11-20): the alerts slice — 6a engine, 6b `brewd`, 6c desktop, 6d UI.**
Everything that needs no relays and no new recipe model. Temp control stays below,
unchecked. Schedule-driven alerts followed on 2026-11-22.

- [x] Rule engine: condition on latest readings → action. Lives in `brewcore`,
      runs in `brewd`. **Refined:** `brewcore::alert`, pure. `brewd` has no
      database, so the desktop pushes **watches** — per active batch: its journal
      feeds (device + column + bound-since), OG, expected FG, band, target, and the
      stability and health params — as one versioned `WatchSet` (`"v": 1`, unknown
      or missing fields refused, as 5c's filters). `brewd` keeps a `DeviceBuffer`
      per journal (a week plus margins, preloaded from live and rotated journals on
      start) and judges the watches that read a device on every row it journals,
      plus a minute tick for what changes with no row. A rule answers raise, clear
      or can't tell; can't tell holds the alert as it is
- [x] ~~Desktop notifications~~ — **v2:** push notifications from `brewd`
      (proposal: ntfy). The desktop app may mirror them but is not the channel.
      **Done:** ntfy, published as JSON to the server root (headers are ASCII; "°C"
      and "⅓" are not). `[notify]` in `brewd.toml`; without it alerts are logged and
      served, not pushed. Stalled, temp and silent are priority 4, the rest 3. A
      failed push stays owed and retries after 5 min; ingest never waits on HTTP.
      Settings → "Send a test notification" goes end to end through `brewd`
- [x] Rule: fermentation finished (SG slope flat for N hours). **Done:** 4c's
      `stability_history` over 4b's smoothing (`calc::smooth::smooth_runs`, now
      shared with `db::series`) of the gravity feed, Plato converted first. A watch
      only gets a gravity feed once `gravity.ispindel_unit` is set
- [x] Rule: temp out of band, sensor stale, no data. "No data" includes the
      iSpindle going silent — battery, or it sank. **Refined:** out of band raises
      once the trailing run outside it spans 10 min (`TEMP_CONFIRM_SECS`), so a
      lifted probe doesn't; it clears 0.3 °C back inside (`TEMP_CLEAR_MARGIN_C`).
      Band: the batch's own ends, else the pitched yeasts' overlap (highest min,
      lowest max) from actuals; a set end that would cross the yeast's other end
      stands alone; no band, no rule. Silence is per device by 4f's rule, now pure
      as `calc::health::stale_after_secs`, with 4f's battery/signal guess in the
      detail. The alert waits at least an hour (`SILENT_ALERT_FLOOR_SECS`): a 5 min
      iSpindle at -88 dBm dropped a send or two most hours and pushed each time. The
      Home card still calls it silent at 4f's threshold
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
- [x] Rules evaluated on ingest, not on a UI timer — in v2, on `brewd`'s ingest.
      **Done:** from `mqtt::Ingest::write`, after the journal append (and even when
      it fails — the reading happened)

**Added (2026-11-20):** gaps found against what the alerts are actually for.

- [x] Rule: gravity reached — an absolute SG, a % of apparent attenuation, or the
      ⅓ sugar break (computed from OG and expected FG). Over the smoothed series,
      for the same reason the finished detector is. **Done:** the ⅓ break (against
      the expected FG band's middle) and a per-batch target SG, both latched — a
      wobble back above isn't news. % of attenuation not built: the target covers it
- [x] Rule: stalled, as its own alert. 4c's `Stalled { since }` already tells it
      apart from finished; the finished rule must not swallow it. **Done:** two
      rules from one evaluation; each clears when the other raises
- [x] Rule: temp rate of change — more than X °C in Y hours, which catches a
      stuck relay or an open fridge door while still inside the band. **Done:**
      `temp_rate`, `[alerts]` `temp_rate_delta_c` (2 °C) within
      `temp_rate_window_hours` (3 h), over a 10 min rolling median so a lifted probe
      doesn't count, and only since the reading came into the band, so a ramp into a
      new one doesn't either. Out of the band it says nothing; the band rules do.
      Clears 0.3 °C under the threshold
- [x] Rule: day N after pitch, for checks that are calendar-driven ("taste on
      day 10"). **Done:** `batch_reminder` (`0014_reminder.sql`), raised by the
      due / overdue rules below. Ticked by a `reminder_done` event, as steps are
- [x] Schedule steps beyond additions. `recipe_item.offset_hours` only schedules
      ingredients, and only by time. Needs a step that can be a task (rack, degas,
      cold crash, take a reading, ramp) as well as an addition, and a trigger that
      is either an offset from pitch or a reading condition. The TOSNA ⅓-break
      dose is the case `offset_hours` cannot express today. **Done (2026-11-21):**
      `schedule_step` (`0013_schedule.sql`), on the batch: ordered steps, each with
      a temperature band, a gravity to fall to and a minimum time; a step starts
      when the one before it is done, the first at the pitch. Ticked by a
      `step_done` event, so the timeline is the record. A W-34/70 lager template.
      **Refined:** the current step's band is the alert band, ahead of the batch's
      own and the yeast's, and a new step's band waits until the beer gets into it
      (or a day passes), so a free-rise or a crash doesn't raise on the way. The
      TOSNA ⅓-break dose is a step with a gravity target
- [x] Rule: schedule step due, and overdue (due + grace, nothing logged). "Done"
      is the matching `batch_ingredient` or `batch_event`, not a checkbox — the
      log stays the one record. **Done:** `due` / `overdue`, keyed per subject in
      the alert's device slot (`step:<id>`, `reminder:<id>`,
      `addition:<recipe_item_id>`), so the log and `alert` are unchanged. A step is
      due once its minimum time and its gravity target (first smoothed crossing)
      are both met; a reminder at its day; a recipe addition timed after the pitch
      at its offset, until the additions table pairs an actual with it. Overdue
      `[alerts].overdue_grace_hours` (12 h) later, taking over from due. Done drops
      the subject from the watch, which clears the alert. Watch v2
- [x] ~~Per-batch schedule overrides — shift or skip a step without forking the
      recipe version~~, and a schedule for a batch with no recipe. **Refined:** the
      schedule lives on the batch, so it is edited in place and a batch with no
      recipe has one like any other; there is no recipe-level schedule to override
- [ ] A recipe version's schedule template, copied into a batch at creation.
      Wait for a second recipe that wants one; the lager template covers the first
- [ ] Rule scope: global defaults (temp band per beverage type, stale interval),
      overridable per recipe version and per batch. Thresholds are config, as the
      finished detector already says. **Partly:** per batch only — `batch_alert`
      (`0012_alert.sql`: band ends, target SG, mute) over the yeast's band;
      stability and health thresholds are the global config. Per beverage type and
      per recipe version are not built; wait for batches to show they're needed
- [x] Alert state, not just a message: one alert per rule × batch while the
      condition holds, hysteresis on the way out so a reading hovering at the
      edge doesn't flap, acknowledge and snooze. Without this every ingest re-fires.
      **Done:** `alert::AlertBook`, keyed per batch × rule × device, persisted by
      `brewd` (`alerts.json`, atomic replace). Ids start at the book's creation in
      ms, so a lost state file never reuses an id the desktop holds. A rule the
      watch stops producing, or a batch dropped from the watches (packaged,
      archived, muted), is cleared with the reason. Pushed once; again once when a
      snooze runs out; never after an ack. Ack and snooze go through `brewd`
- [x] Alerts reach the desktop the way readings do: `brewd` writes an alert
      journal, the app ingests it over the same `Range` fetch. The app shows an
      inbox per batch and on Home, and marks alerts on the curve like events.
      **Done:** `<state_dir>/alerts.csv`, written through `Journal` (same trim and
      rotate promises), served at `GET /alerts/log` — outside `journal_dir`, so
      it never lists as a device. The desktop folds it into `alert` keyed on
      `brewd`'s ids, offset per base URL in `alert_sync`, moved in the same
      transaction; a truncated log is reread from 0, and every write is an upsert.
      Its own Tauri loop on the poll interval, not a `ReadingSource` — a refused
      token must not back the readings off — woken early by a settings save or an
      ack. Watches are pushed only when `GET /watches` differs, so a `brewd` that
      lost its state gets them back next round. `alerts://changed` refreshes Home,
      the batch screen and the curve (▼ where raised, a bar while it held)
- [x] Every evaluation logged, fired or not — the finished-detection section asks
      for this to tune thresholds against history. **Refined:** not every
      evaluation — a row a second per probe would be most of the Pi's writes. 4c's
      replay already re-evaluates every past batch under any threshold, and the
      alert log records every transition (raised, cleared, acked, snoozed,
      notified) with what the rule saw
- [x] **Added:** `brewd`'s first write endpoints — `PUT /watches`,
      `POST /alerts/<id>/ack`, `POST /alerts/<id>/snooze`, `POST /notify/test` —
      need `Authorization: Bearer <http.token>` (≥ 16 chars, compared in constant
      time). No token set refuses every write with 403, so an upgrade never opens
      writes to the LAN by default. Reads stay open. The desktop's copy is
      `brewd.token` in Settings; without it alerts are read, watches not pushed

**Done when (alerts slice):** with the laptop shut, a batch whose vessel probe runs
past its band for ten minutes buzzes the phone once, and opening the app shows the
alert on Home, on the batch and on its curve, where acknowledging it stops any
repeat. Verified 2026-11-20 against a local `brewd` and a stand-in ntfy; not yet
against the Pi's broker or a real phone.

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
