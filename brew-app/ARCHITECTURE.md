
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
