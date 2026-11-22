# Fermentrack Interface — what's actually there

Source: [thorrak/fermentrack](https://github.com/thorrak/fermentrack) @ `1aea05b0`
(master, last commit 2023-09-04). Read from the code, not from docs. Fermentrack 2
does not exist yet, so this is the only target.

Headline: **there is no MQTT and no history API.** `grep -ril mqtt` over the whole
repo returns nothing. The JSON endpoints under `/api/` exist to feed Fermentrack's
own dashboard widgets and to let `brewpi-script` write points — they answer "what is
it right now", never "what happened between t1 and t2".

History is in **flat CSV files** served over HTTP. That is the integration point.

---

## 1. History — flat CSV at `/data/`

`BeerLogPoint` and `GravityLogPoint` are Django models with:

```python
class Meta:
    managed = False  # Since we're using flatfiles rather than a database
```

`save()` appends a CSV row to disk (`app/models.py:1540`, `gravity/models.py:531`).
Nothing is ever written to the database. `settings.DATA_ROOT = ROOT_DIR/data`,
`settings.DATA_URL = '/data/'`, and nginx maps `^/data/` straight at that directory.

### Filenames

Built by `Beer.base_filename()` / `GravityLog.base_filename()`:

```text
/data/Device {device_id} - B{beer_id} - {beer_name}_graph.csv
/data/Device {device_id} - B{beer_id} - {beer_name}_full.csv
/data/Device {device_id} - B{beer_id} - {beer_name}_annotations.almost_json

/data/Gravity Device {device_id} - L{log_id} - {log_name}_graph.csv
/data/Gravity Device {device_id} - L{log_id} - {log_name}_full.csv
/data/Gravity Device {device_id} - L{log_id} - {log_name}_annotations.almost_json
```

The name is in the path, so a rename moves the file. Key on `beer_id` / `log_id`
and re-resolve the filename every sync. Names are validated against
`^[a-zA-Z0-9 _-]*$`, so no escaping surprises — but they do contain spaces.

### Columns

`_full.csv` (`data_format='full_csv'`):

```text
log_time, beer_temp, beer_set, beer_ann, fridge_temp, fridge_set, fridge_ann,
room_temp, state, temp_format, associated_beer_id [, gravity, grav_temp]
```

`_graph.csv` (`base_csv`; `state` only when `model_version > 1`):

```text
log_time, beer_temp, beer_set, fridge_temp, fridge_set, room_temp
           [, gravity, grav_temp] , state
```

Note the gravity columns sit in different places: appended after
`associated_beer_id` in `_full.csv`, but *before* `state` in `_graph.csv`.

**Both files default missing readings to `0`.** `BeerLogPoint.save()` calls
`self.data_point('full_csv')`, and `data_point`'s signature is
`(self, data_format='base_csv', set_defaults=True)` — so `beerTemp = self.beer_temp or 0`
applies to the full log too. Only `beer_ann` and `fridge_ann` are ever written blank.

Take `_full.csv` anyway: it is the only file carrying the annotations, `state`,
`temp_format` and `associated_beer_id`. Just don't expect it to preserve nulls.

A missing reading is still distinguishable from a real one, because Python writes
`str(value)`: `x or 0` emits the *integer* `0`, while any genuine reading emits a
float with a decimal point (`0.0`, `18.0`, `1.043`). So on ingest, a bare `0` in a
temp or gravity column is `None`; `0.0` is a real zero. `state`, `temp_format` and
`associated_beer_id` are genuine integers and are not subject to this rule.

### Dialect

Python's `csv.writer` default (the `excel` dialect), so:

- Rows terminate `
`, including the last one. Files are CRLF end to end.
- Comma delimiter, `"` quotechar, `QUOTE_MINIMAL` — a field is quoted only when it
  contains `,` `"` `` or `
`, and an embedded `"` is doubled. Annotations are free
  text, so `split(',')` will eventually eat a row. Use a real CSV reader.
- The header is written once at file creation (`'w'`); rows are appended (`'a'`).
- Values are raw Python floats and ints, never Decimals — the API view assigns
  `data.get('beer_temp')` straight onto the unmanaged model, which never round-trips
  a database. So the bytes are `str()` of a JSON float: `18.06`, `18.0`, `0`, `4`,
  `1.043`. No fixed decimal places, no padding.

- `log_time` format: `%Y/%m/%d %H:%M:%SZ`, always UTC.
- `temp_format` is `'C'` or `'F'` per row — the Beer caches its own format, which
  can drift from the device's. Convert on ingest using the row's own value.
- `state` is the BrewPi controller state, `0..9`: IDLE, STATE_OFF, DOOR_OPEN,
  HEATING, COOLING, WAITING_TO_COOL, WAITING_TO_HEAT, WAITING_FOR_PEAK_DETECT,
  COOLING_MIN_TIME, HEATING_MIN_TIME.
- The gravity columns only exist if `Beer.gravity_enabled` — column count varies
  per file. Read the header row, don't assume positions.
- `Beer.model_version` (currently 2) only affects `_graph.csv`: the check is
  `if which == 'base_csv' and self.model_version > 1`, so v1 graph files have no
  `state` column. `_full.csv` has `state` at index 8 in every version.

### Why this is good news

The files are **append-only**. Incremental sync is an HTTP `Range: bytes=N-`
request with the last byte offset, then parse whatever comes back. No pagination,
no watermark timestamps, no dedup pass — and if the file shrank or the offset
lands mid-line, refetch whole and re-derive. Cheap and exact.

`_annotations.almost_json` is a JSON array that is written incrementally and
never closed with `]` — hence "almost". `app/almost_json.py` is Fermentrack's own
fixup. Parse it the same way or ignore it; annotations also live in `_full.csv`
as `beer_ann` / `fridge_ann`.

---

## 2. The JSON API — current state only

| Endpoint | Returns |
|---|---|
| `GET /api/devices/` | flat list of active BrewPiDevice ids, e.g. `[1, 2]` — nothing else |
| `GET /api/gravity/` | per-sensor current gravity/temp, plus dashboard chrome |
| `GET /api/gravity/<device_id>/` | same, one sensor |
| `GET /api/gravity/ispindel/<device_id>/` | iSpindel extras (battery, angle) from Redis |
| `GET /api/gravity/tilt/<device_id>/` | Tilt extras from Redis |
| `GET /api/lcd/`, `/api/lcd/<device_id>/` | the controller's LCD text as 4 strings |
| `GET /api/panel/<device_id>/` | dashpanel: beer/fridge/room temp + setpoints |
| `GET /api/log/<text\|json>/<device_type>/<stdout\|stderr>/d<id>/l<lines>/` | controller log tail |
| `POST /api/save_point/` | creates a BeerLogPoint; `@csrf_exempt`; used by brewpi-script |

`/api/gravity/` returns display strings, not clean data — `"current_gravity":
"1.043"` zero-padded via `"{:05.3f}"`, `"temp_string": "18.1&deg; C"`,
`"--.-&deg;"` when stale, plus `device_url`, `manage_text`, `modal_name`. It also
suppresses points older than an expiry window (Tilt 2 min, iSpindel 62 min, other
30 days) by returning nulls. Usable for a live tile; parse defensively.

`/api/panel/<device_id>/` is the better "right now" source for a BrewPi device —
it comes from `get_dashpanel_info()`, which talks to brewpi-script over its socket.

**Not exposed by any API:** the list of beers, gravity logs, fermentation
profiles, device names, or anything historical.

---

## 3. Auth — there is none

- No API view carries `@login_required`. `get_devices` and `get_gravity_sensors`
  have no decorator at all.
- `REQUIRE_LOGIN_FOR_DASHBOARD` is a Constance setting, default `False`, and only
  wraps *page* views via `@login_if_required_for_dashboard`.
- `/data/` is served by nginx with no auth.
- There is no API key or token concept for inbound requests. The `api_key` field
  on `GenericPushTarget` is a string Fermentrack sends *outbound* to whoever it
  pushes to.

So our client needs a base URL and nothing else. If dashboard login is ever
enabled, the `/api/` and `/data/` paths still won't require it.

---

## 4. Push — Fermentrack → us

`external_push.GenericPushTarget` will POST JSON (or write raw TCP) to a target we
configure in Fermentrack's UI:

```json
{
  "api_key": "<whatever we typed in>",
  "version": "1.0",
  "brewpi_devices": [
    {"name": "Fermenter 1", "internal_id": 1, "temp_format": "C",
     "beer_temp": 18.1, "fridge_temp": 17.4, "room_temp": 21.0,
     "beer_setting": 18.0, "fridge_setting": 17.5}
  ],
  "gravity_sensors": [ ... ]
}
```

Frequency choices: 1, 2, 5, 10, 15, 30, 60 minutes (30 s is present but commented
out). Data format `generic`; a `tiltbridge` format exists but is disabled.

Useful, but **not** a replacement for CSV sync:

- Minimum 1 min, while `data_point_log_interval` can be 10 s — push is lossier
  than the log.
- A desktop app that isn't running receives nothing, and Fermentrack does not
  retry or queue.
- It carries current values only, no history.

Treat push as an optional low-latency nicety that needs an HTTP listener in the
Tauri process. CSV sync stays the backbone either way.

---

## 5. Control — the brewpi-script socket

No HTTP API sets temperature. Fermentrack's own web views form-POST with CSRF, and
underneath they all call `BrewPiDevice.send_message()`, which opens a socket to
`brewpi-script` (`app/models.py:665`).

Wire protocol, one command per connection:

1. Connect — `AF_UNIX` to `socket_name`, or `AF_INET` to `socketHost:socketPort`
   (default `2222`) when `useInetSocket` is true.
2. Send `command` or `command=argument`, encoded **cp437**.
3. Optionally one `recv(65536)`, decoded cp437. JSON for the getters.
4. Close.

Commands in use:

```text
setBeer=<temp>        setFridge=<temp>      setOff
setActiveProfile=<id> startNewBrew=<name>
stopLogging           pauseLogging          resumeLogging
getMode  getBeer  getFridge  lcd  getVersion
getDeviceList  refreshDeviceList  applyDevice=<json>  resetController
```

This is the `Controller` trait's first implementation. Note it bypasses
Fermentrack's ORM — Fermentrack's own DB will not know we changed a setpoint, so
anything we set this way should also be reflected in our own records.

---

## 6. Fermentrack's own database

Docker Compose runs django + **postgres** + redis + nginx. Postgres holds the
config metadata that has no API: `Beer`, `BrewPiDevice`, `GravitySensor`,
`GravityLog`, `FermentationProfile`. Redis holds each sensor's latest point
(`save_to_redis` / `retrieve_latest_point`).

A read-only Postgres connection is the only way to enumerate beers and gravity
logs programmatically. It is also a coupling we said we'd avoid. Prefer scraping
the two list pages (`/beer/list/`, `/gravity/logs/`) or letting the user paste the
ids once per batch; fall back to read-only SQL if that gets annoying.

Redis is not worth touching — the CSV has everything and the API covers "now".

---

## 7. Cadence

`BrewPiDevice.data_point_log_interval`: default **30 s**, choices 10 s, 30 s, 1,
2, 5, 10, 30 min, 1 h. Gravity sensors report on their own schedule — Tilt roughly
every second (smoothed before logging), iSpindel sleeps and can be quiet for an
hour.

At 30 s that's ~1M rows/year/fermenter, which is the volume ARCHITECTURE.md
already sized SQLite against. Keep everything.

---

## 8. Project health

Master's last commit is 2023-09-04 and Fermentrack 2 is unreleased. Nothing here
is going to change under us, which makes the CSV format a safe thing to depend on
— and makes the Option 2 migration a question of when, not if.
