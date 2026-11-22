# Architecture

How the brew app is split between devices, an always-on Pi and the desktop, and
why. The project started on Option 1 below and switched to v2 on 2026-09-25; see
[Decision: straight to v2](#decision-straight-to-v2). The earlier reasoning is kept
because v2 reuses most of it.

## Goal

Make the seltzer process repeatable, and eventually commercially useful. Every
batch should leave a complete record that can be queried later:

```text
Batch 024
────────────────────────────
Recipe: Citron-Verveine
OG: 1.037
Yeast: EC-1118
Nutrients: ...
Pitch temp: 18.1°C

Fermentation:
0h    1.037   18.1°C   pH 3.42
12h   1.030   18.0°C   pH 3.18
24h   1.021   18.0°C   pH 3.08
...
Day 13  1.004  20.0°C  pH 3.01

Status: Ready for cold crash
```

After 20 batches the useful question is something like *all batches on EC-1118
with OG 1.035–1.040 fermented at 18°C*. That query, not live sensor display, is
what a generic brewing dashboard doesn't give.

Scope beyond fermentation tracking: batch records, ingredient lots, recipe
versions, OG/FG/ABV, yeast and nutrient schedules, flavouring additions,
fermentation curves, finished-fermentation detection, batch comparison, alerts,
production notes, cleaning and packaging records, cost per batch, and later
inventory.

## Options considered

### Option 1: the app on top of Fermentrack

```text
             Brew App
        ┌─────────────────────┐
        │ Batch management    │
        │ Recipes             │
        │ Analytics           │
        │ Alerts              │
        │ Automation rules    │
        │ Production records  │
        └──────────┬──────────┘
                   │
              API / CSV
                   │
             Fermentrack
                   │
                 ESP32
                   │
             Fridge / Heater
```

Fermentrack stays the fermentation-control appliance and does the low-level work:
talking to the controller, logging, temperature profiles. The app is the layer
above it, the brewery's record system.

### Option 2: replace Fermentrack

```text
                    Brew App
            ┌──────────────────────────┐
            │ Dashboard                │
            │ Batch management         │
            │ Recipes                  │
            │ Automation engine        │
            │ Sensor data              │
            │ Fermentation control     │
            │ Analytics                │
            │ Inventory                │
            └────────────┬─────────────┘
                         │
                        MQTT
                         │
               ┌─────────┴─────────┐
               │                   │
            ESP32 #1            ESP32 #2
               │                   │
           Fermenter 1         Fermenter 2
```

The app reads sensors, logs data and runs temperature profiles itself. More
control, but everything Fermentrack and BrewPi already solve becomes this
project's code to build and maintain.

### Original choice: Option 1, with a clean seam

Build the app as the layer above Fermentrack first, and keep every Fermentrack
call behind one module so the middle can be swapped later without touching the
rest:

```text
NOW    App → Fermentrack → BrewPi → ESP32
LATER  App → own controller → ESP32
```

That seam is `brewcore::fermentrack`. It is still in the tree, tested and unused.

## Desktop app, not web app

Single user, single site, no auth, nothing to deploy. A server running all the
time just to view one person's data buys nothing. Tauri gives the same web UI
stack without the server, ships as one binary, and puts the HTTP polling on the
Rust side. A future controller moves into the same process rather than a new
runtime.

The cost: when the app is closed, nothing polls. Something else has to be the
**logger of record**, and the app backfills from it on launch. The app's database
is derived and enriched, never the only copy of a reading.

## SQLite, not PostgreSQL

One sample a minute across two fermenters is about 1M rows a year, a few hundred
MB after a decade. Postgres buys concurrent writers, network access and roles,
none of which apply, and costs a daemon, backups and upgrades. SQLite is one
`brewery.db` file: copy it to back it up, works offline, full SQL. Queries live in
one data layer with no ORM, so moving to Postgres or libSQL later is mechanical.
Do it when a second person needs to write at the same time, not before.

## Decision: straight to v2

*2026-09-25. Supersedes Option 1.*

Phases 0–3 were built for Option 1. The fermentation hardware wasn't all here yet,
so there was time to build the controller side instead, and Fermentrack was never
installed. v2 hardware: the existing ESP32 with a DS18B20, and an iSpindel for
gravity.

```text
ESP32 (ESPHome) ─┐                    Raspberry Pi                         Desktop
DS18B20          ├─ MQTT ─► Mosquitto ─► brewd ─► journals ── HTTP Range ─► Brew App
iSpindel ────────┘                        │                                 SQLite
                                          └─ alerts · (later) setpoints, profiles
```

**What survives.** Everything above except the logger. The app stays desktop,
SQLite stays SQLite, raw readings (Tier 1) and batch records (Tier 2) stay split,
and the rest of the app doesn't change. Phases 1–3 needed targeted fixes, not a
rewrite; they're listed under Phase V in [docs/BUILDOUT.md](docs/BUILDOUT.md).

**What breaks: Fermentrack as logger of record.** The desktop decision depends on
something always on logging while the app is closed. MQTT keeps no history.
Mosquitto's queue for an offline client is capped (1000 messages by default), and
iSpindel and ESPHome payloads carry no timestamp, so a queued message would be
stamped with its delivery time, not its measurement time. Control has the same
problem: a 09:00 ramp or a 3 am temperature alarm can't depend on the laptop being
open.

**So the always-on part is a Raspberry Pi Zero 2 W.** It takes over Fermentrack's
role, and only that role:

- **Mosquitto**, the broker the devices publish to.
- **`brewd`**, a headless binary built on the same `brewcore` crate. It subscribes,
  timestamps each message on receipt, and appends to one journal file per device.
  It serves those journals over HTTP with `Range`, the way Fermentrack served its
  CSVs, so the desktop's Phase 2 sync works against it with only the dialect
  swapped. It also evaluates alert rules, and later runs temperature profiles,
  because both must run while the app is closed.
- **The control loop stays on the ESP32** (ESPHome's `thermostat`). If the Pi dies,
  the fridge holds its last setpoint. If the ESP32 dies, nothing else could have
  saved it anyway. Temperature control is deferred until everything else runs, so
  v2 starts as a logger.

The Pi deliberately does **not** own `brewery.db`. That would make the desktop app
a client of a server, the web-app shape rejected above, with auth and deploys to
follow. The journals are raw, append-only and dumb; the desktop keeps everything
that has meaning. Revisit if a second person needs to write.

**The costs.** Raw data is no longer backed by a box someone else maintains. The
journals are the only raw copy of every reading, so they need a backup too. And
BrewPi's compressor protection, profile runner and alerting are now this project's
code or config.

```text
NOW (v2)  App → brewd (Pi) → MQTT → ESP32 / iSpindel
```
