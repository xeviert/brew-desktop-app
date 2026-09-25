> **2026-09-25: the project went straight to v2** — see
> [Decision: straight to v2](#decision-straight-to-v2) at the end. Everything
> before it is the original reasoning, kept because v2 reuses most of it.

### Option 1: Your software on top of Fermentrack — what I'd recommend initially

You let the existing brewing stack do the low-level, annoying stuff:

```text
             Your Brew App
        ┌─────────────────────┐
        │ Batch management    │
        │ Recipes              │
        │ Analytics            │
        │ Alerts               │
        │ Automation rules     │
        │ Production records   │
        └──────────┬──────────┘
                   │
              API / MQTT
                   │
             Fermentrack
                   │
                 ESP32
                   │
             Fridge / Heater
```

Your software could add things Fermentrack isn't specifically designed to be a full brewery management system for:

* Batch records
* Ingredient lots
* Recipe versions
* OG/FG/ABV calculations
* Yeast/nutrient schedules
* Flavoring additions and timings
* Fermentation curves
* Automatic "fermentation finished" detection
* Batch-to-batch comparison
* Alerts
* Production notes
* Cleaning/sanitation records
* Packaging records
* Cost per batch
* Eventually inventory
* Eventually sales/production tracking

For example, your dashboard could say:

> **Seltzer #017**
> OG 1.037
> Current SG 1.004
> 4.3% estimated ABV
> pH 3.01
> 18.0°C
> Fermentation stable for 31h
> → Raise to 20°C tomorrow at 09:00
> → Cold crash when stable

Fermentrack continues being the **fermentation-control appliance**, while your application becomes the **brewery operating system**.

---

## Option 2: Build your own replacement for Fermentrack

Eventually you could do this:

```text
                    Your Brewery OS
            ┌──────────────────────────┐
            │ Dashboard                │
            │ Batch Management         │
            │ Recipes                  │
            │ Automation Engine        │
            │ Sensor Data              │
            │ Fermentation Control     │
            │ Analytics               │
            │ Inventory               │
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

At that point, Fermentrack disappears.

Your own system would directly control the ESP32s, read sensors, log data, execute temperature profiles, etc.

That gives you **much more control**, but you're now responsible for building and maintaining all the brewing-controller functionality that Fermentrack/BrewPi already solved.

---

## The architecture I'd use for you

I'd deliberately make the system **modular enough that you can migrate later**:

```text
                     ┌──────────────────┐
                     │ Your Brew App    │
                     │ Tauri (desktop)  │
                     │ SQLite           │
                     └────────┬─────────┘
                              │
                         MQTT / API
                              │
                ┌─────────────┴─────────────┐
                │                           │
          Fermentrack                  Sensors
                │
             BrewPi
                │
             ESP32
                │
          ┌─────┴─────┐
          ↓           ↓
       Fridge       Heater
```

Initially, **your app consumes Fermentrack's data**.

Desktop app, not web app: single user, single site, no auth, no deploy target — a
server running 24/7 just to view your own data buys nothing. Tauri gives the same
web UI stack without the server, ships as one binary, and puts MQTT/HTTP polling in
the Rust side. When Fermentrack is replaced later, the controller logic moves into
that same process instead of a new runtime.

Consequence of going desktop: app closed means nobody is polling. So Fermentrack
stays the **logger of record**, and your app backfills its history from Fermentrack's
API on launch. Your database is derived and enriched, never the sole source.

SQLite, not PostgreSQL: 1 sample/min across 2 fermenters is ~1M rows/year — a few
hundred MB after a decade. Postgres buys concurrent writers, network access and
roles, none of which apply here, and costs a daemon, backups and version upgrades.
SQLite is one `brewery.db` file: copy it to back it up, works offline, full SQL. The
batch-comparison queries are identical. Keep queries in a single data layer (no ORM
lock-in) and swapping to Postgres or libSQL later is mechanical — do it when a second
human needs concurrent writes, not before.

Later, you can replace that middle section:

```text
             NOW
Your App → Fermentrack → BrewPi → ESP32

             LATER
Your App → Your Controller → ESP32
```

The rest of your application doesn't have to change.

### Why I especially like this for your project

You have a legitimate reason to build the software beyond just "because I can." You're trying to make your seltzer process **repeatable and eventually commercially useful**.

You could end up with a system where every batch automatically produces a complete digital record:

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

Then after 20 batches, you can actually query:

> Show me all batches using EC-1118 with OG 1.035–1.040 and fermentation at 18°C.

That's where your software becomes substantially more valuable than a generic brewing dashboard.

**So I would not build a Fermentrack replacement first. I'd build your brewery software as the layer above it, while keeping the interface between the two clean enough that you can replace Fermentrack later.**

---

## Decision: straight to v2

*2026-09-25. Supersedes the recommendation above; the reasoning above is kept
because v2 reuses most of it.*

Phases 0–3 were built for Option 1. The fermentation hardware isn't all here yet,
so there is time to build the controller, and Fermentrack never gets installed.
The hardware v2 targets: the existing ESP32 with a second DS18B20, and an iSpindle
for gravity.

```text
ESP32 (ESPHome) ─┐                    Raspberry Pi                         Desktop
2× DS18B20       ├─ MQTT ─► Mosquitto ─► brewd ─► journals ── HTTP Range ─► Brew App
iSpindle ────────┘                        │                                 SQLite
                                          └─ setpoints · profiles · alerts
```

**What survives.** Everything above holds except the logger. The desktop app
stays desktop, SQLite stays SQLite, Tier 1 and Tier 2 stay split, and the rest of
your application really doesn't have to change. Phases 1–3 need a handful of
targeted fixes, not a rewrite. They're listed in
[docs/BUILDOUT.md](docs/BUILDOUT.md) under Phase V.

**What breaks: "Fermentrack stays the logger of record."** The desktop decision
depended on something always-on logging while the app is closed, so the app
could backfill on launch. MQTT keeps no history. Mosquitto's queue for an
offline client is capped (1000 messages by default), and iSpindle and ESPHome
payloads carry no timestamp, so a queued message would be stamped with its
delivery time, not its measurement time. Control has the same problem: a 09:00
ramp or a 3 am temperature alarm can't depend on the laptop being open.

**So something has to be always on: a Raspberry Pi Zero 2 W** (set up later). It takes over
Fermentrack's role, and only that role:

- **Mosquitto**, the broker the devices publish to.
- **`brewd`**, a headless binary built from the same `brewcore` crate. It
  subscribes, timestamps each message on receipt, and appends to one journal file
  per device. It serves those journals over HTTP with `Range` exactly the way
  Fermentrack served its CSVs, so the desktop's Phase 2 sync machinery works
  against it with the dialect swapped. It also runs temperature profiles and
  alert rules, because those must run while the app is closed.
- **The control loop stays on the ESP32** (ESPHome's `thermostat`). If the Pi dies,
  the fridge still holds its last setpoint. If the ESP32 dies, nothing else could
  have saved it anyway. Temp control is deferred until everything else is built
  and running, so v2 starts as a logger only.

The Pi deliberately does **not** own `brewery.db`. That would turn the desktop
app into a client of a server — the web-app shape this document rejected, with
auth and deploys to follow. The journals are raw, append-only and dumb, and the
desktop keeps everything that has meaning. Revisit if a second person needs to
write.

**The costs.** Tier 1 is no longer backed by a box someone else maintains. The
journals are now the only raw copy of every reading, so they need a backup too.
And BrewPi's compressor protection, profile runner and alerting are now this
project's code or config.

Updated picture:

```text
             NOW (v2)
Your App → brewd (Pi) → MQTT → ESP32 / iSpindle
```
