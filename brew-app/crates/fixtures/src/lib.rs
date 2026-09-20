//! Byte-accurate Fermentrack log files, generated.
//!
//! There is no Fermentrack instance to point at, and waiting for one would block the
//! only two phases that matter (docs/BUILDOUT.md, "Build the fixture generator
//! first"). It is not needed: the log format is fully determined by
//! `thorrak/fermentrack`'s `app/models.py`, the repo's last commit is 2023-09-04, and
//! everything the parser has to survive can be generated today.
//!
//! What was read from the source, and what it costs to get wrong:
//!
//! - **`_full.csv` defaults missing values to `0`.** `BeerLogPoint.save()` calls
//!   `self.data_point('full_csv')`, and `data_point`'s signature is
//!   `(self, data_format='base_csv', set_defaults=True)`, so `beerTemp = self.beer_temp or 0`
//!   applies to the full log too. Only `beer_ann` and `fridge_ann` are ever blank.
//!   We still read `_full.csv` — for the annotations, `state`, `temp_format` and the
//!   gravity columns — but not because it preserves nulls. It does not.
//! - **`model_version` does not change `_full.csv`.** The check in
//!   `Beer.column_headers` is `if which == 'base_csv' and self.model_version > 1`, so
//!   the version only decides whether `_graph.csv` carries `state`.
//! - **Lines end `\r\n`** — see `csv_writer`.
//! - In `_full.csv` the gravity columns are appended at the end, after
//!   `associated_beer_id`. In `_graph.csv` they go *before* `state`.

mod csv_writer;
mod curve;
mod time;

#[cfg(feature = "serve")]
mod serve;

#[cfg(feature = "serve")]
pub use serve::{Fault, FixtureServer, RangeMode};

pub use csv_writer::py_float;
pub use time::format_log_time;

use csv_writer::{Field, round_to, write_row};

/// Which of the two log files. Never `_annotations.almost_json`: annotations are
/// already in `_full.csv`, and parking that file is a Phase 2h decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `_full.csv` — the one we ingest.
    Full,
    /// `_graph.csv` — generated only so the parser can be tested on refusing it.
    Graph,
}

impl Kind {
    pub fn suffix(self) -> &'static str {
        match self {
            Kind::Full => "_full.csv",
            Kind::Graph => "_graph.csv",
        }
    }
}

/// A sensor column that can go missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    BeerTemp,
    BeerSet,
    FridgeTemp,
    FridgeSet,
    RoomTemp,
    Gravity,
    GravTemp,
}

/// Everything ugly a real log does, switchable.
#[derive(Debug, Clone, Default)]
pub struct Quirks {
    /// After row `N`, jump the clock forward by `secs` — Fermentrack was down.
    pub gap: Option<(usize, i64)>,
    /// Columns with no sensor behind them. Written as a bare `0` on every row,
    /// which is exactly the trap: an absent room probe is indistinguishable from a
    /// real 0 °C except by the missing decimal point.
    pub absent_sensors: Vec<Column>,
    /// Emit annotations, including one containing a comma and one containing a
    /// quote, so the parser has to honour CSV quoting rather than `split(',')`.
    pub annotations: bool,
    /// From row `N`, the Beer's cached temp format flips `C` to `F` and the values
    /// come with it. Fermentrack really does this — `temp_format` is per row.
    pub temp_format_switch: Option<usize>,
}

/// One log file to generate.
#[derive(Debug, Clone)]
pub struct LogSpec {
    pub device_id: u32,
    pub log_id: u32,
    pub name: String,
    pub gravity_enabled: bool,
    /// Affects `_graph.csv` only. See the module docs.
    pub model_version: u8,
    /// Pitch time, UTC epoch seconds.
    pub start: i64,
    /// How far into the ferment the first row sits. The curve is anchored at pitch,
    /// so without this every short fixture lands in the 12 h lag phase: flat gravity
    /// and an IDLE controller on every row, which is the one shape that proves
    /// nothing.
    pub start_hours_in: f64,
    /// `BrewPiDevice.data_point_log_interval`; 30 s is Fermentrack's default.
    pub interval_secs: i64,
    pub rows: usize,
    pub seed: u64,
    pub quirks: Quirks,
}

impl Default for LogSpec {
    fn default() -> Self {
        Self {
            device_id: 1,
            log_id: 17,
            name: "Seltzer 17".to_owned(),
            gravity_enabled: false,
            model_version: 2,
            start: 1_758_153_600, // 2025/09/18 00:00:00Z
            start_hours_in: 0.0,
            interval_secs: 30,
            rows: 120,
            seed: 1,
            quirks: Quirks::default(),
        }
    }
}

/// The named variants. `--scenario all` emits every one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// A clean temperature-only log. The baseline everything else is a diff against.
    Nominal,
    /// `gravity_enabled` — two extra columns, appended after `associated_beer_id`.
    Gravity,
    /// Every quirk at once: a gap, an absent sensor, quoted annotations, a mid-file
    /// `temp_format` switch.
    Ugly,
    /// `model_version = 1`. Its `_full.csv` is identical in shape to version 2's —
    /// which is the point of generating it.
    LegacyV1,
    /// Mid-ferment, where the numbers actually move: gravity falling, the exotherm
    /// pushing the beer off setpoint, and a controller doing something other than
    /// IDLE. Every other scenario starts at pitch and sits in the lag phase.
    Active,
}

impl Scenario {
    pub const ALL: [Scenario; 5] = [
        Scenario::Nominal,
        Scenario::Gravity,
        Scenario::Ugly,
        Scenario::LegacyV1,
        Scenario::Active,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Nominal => "nominal",
            Scenario::Gravity => "gravity",
            Scenario::Ugly => "ugly",
            Scenario::LegacyV1 => "v1",
            Scenario::Active => "active",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == s)
    }

    pub fn spec(self) -> LogSpec {
        match self {
            Scenario::Nominal => LogSpec {
                name: "Seltzer 17".to_owned(),
                ..LogSpec::default()
            },
            Scenario::Gravity => LogSpec {
                log_id: 18,
                name: "Seltzer 18 Citron".to_owned(),
                gravity_enabled: true,
                ..LogSpec::default()
            },
            Scenario::Ugly => LogSpec {
                log_id: 19,
                name: "Seltzer 19 Ugly".to_owned(),
                gravity_enabled: true,
                quirks: Quirks {
                    gap: Some((2, 4 * 3600)),
                    // `gravity_enabled` with nothing assigned to it: the column is
                    // there and every cell is a bare `0`. The column where reading
                    // that as a real value is least visible.
                    absent_sensors: vec![Column::RoomTemp, Column::Gravity],
                    annotations: true,
                    temp_format_switch: Some(3),
                },
                ..LogSpec::default()
            },
            Scenario::LegacyV1 => LogSpec {
                device_id: 2,
                log_id: 4,
                name: "Old Log 4".to_owned(),
                model_version: 1,
                ..LogSpec::default()
            },
            Scenario::Active => LogSpec {
                log_id: 20,
                name: "Seltzer 20 Active".to_owned(),
                gravity_enabled: true,
                // Past the lag and onto the steep part of the curve.
                start_hours_in: 14.0,
                // An hour between rows, so five of them span enough of the curve for
                // the gravity to actually fall. 30 s would round to the same SG five
                // times over.
                interval_secs: 3600,
                ..LogSpec::default()
            },
        }
    }
}

/// One log point, before it becomes cells. `None` is a value Fermentrack never had,
/// and is rendered as the bare `0` its `x or 0` produces — not as a blank.
#[derive(Debug, Clone)]
pub struct Row {
    pub ts: i64,
    pub beer_temp: Option<f64>,
    pub beer_set: Option<f64>,
    pub beer_ann: Option<String>,
    pub fridge_temp: Option<f64>,
    pub fridge_set: Option<f64>,
    pub fridge_ann: Option<String>,
    pub room_temp: Option<f64>,
    pub state: u8,
    pub temp_format: char,
    pub gravity: Option<f64>,
    pub grav_temp: Option<f64>,
}

/// Fermentrack's own check before a name goes into a filesystem path
/// (`Beer.name_is_valid`, guarding against path injection).
pub fn name_is_valid(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-')
}

impl LogSpec {
    /// `Device {device_id} - B{log_id} - {name}`, or Fermentrack's `NAME ERROR`
    /// fallback when the name would not survive being put in a path.
    pub fn base_filename(&self) -> String {
        if name_is_valid(&self.name) {
            format!(
                "Device {} - B{} - {}",
                self.device_id, self.log_id, self.name
            )
        } else {
            format!(
                "Device {} - B{} - NAME ERROR - ",
                self.device_id, self.log_id
            )
        }
    }

    pub fn file_name(&self, kind: Kind) -> String {
        format!("{}{}", self.base_filename(), kind.suffix())
    }

    /// The URL path nginx serves it from.
    pub fn remote_path(&self, kind: Kind) -> String {
        format!("/data/{}", self.file_name(kind))
    }

    /// Column names, in file order.
    pub fn header(&self, kind: Kind) -> Vec<&'static str> {
        let mut headers: Vec<&'static str> = match kind {
            Kind::Full => vec![
                "log_time",
                "beer_temp",
                "beer_set",
                "beer_ann",
                "fridge_temp",
                "fridge_set",
                "fridge_ann",
                "room_temp",
                "state",
                "temp_format",
                "associated_beer_id",
            ],
            Kind::Graph => vec![
                "log_time",
                "beer_temp",
                "beer_set",
                "fridge_temp",
                "fridge_set",
                "room_temp",
            ],
        };
        if self.gravity_enabled {
            headers.push("gravity");
            headers.push("grav_temp");
        }
        // `state` is already in the full header at index 8; in the graph file it is
        // appended last, and only from model version 2.
        if kind == Kind::Graph && self.model_version > 1 {
            headers.push("state");
        }
        headers
    }

    /// The log points, before rendering. Exposed so a test can assert on values
    /// without re-parsing its own fixture.
    pub fn generate(&self) -> Vec<Row> {
        let mut rng = curve::Rng::new(self.seed);
        let mut rows = Vec::with_capacity(self.rows);
        let mut ts = self.start;

        for i in 0..self.rows {
            if let Some((after, secs)) = self.quirks.gap
                && i == after + 1
            {
                ts += secs;
            }

            let hours = self.start_hours_in + (ts - self.start) as f64 / 3600.0;
            let activity = curve::activity_at(hours);

            let beer_set = 18.0;
            let beer_temp = beer_set + activity * 0.8 + rng.noise(0.12);
            let fridge_set = beer_set - 1.0 - activity * 2.0;
            let fridge_temp = fridge_set + rng.noise(0.35);
            let room_temp = 21.0 + rng.noise(0.4);
            let grav_temp = beer_temp + 0.3;

            let drift = beer_temp - beer_set;
            let state = match drift {
                d if d > 0.5 => 4,  // COOLING
                d if d > 0.2 => 5,  // WAITING_TO_COOL
                d if d < -0.5 => 3, // HEATING
                d if d < -0.2 => 6, // WAITING_TO_HEAT
                _ => 0,             // IDLE
            };

            let to_f = self
                .quirks
                .temp_format_switch
                .is_some_and(|switch| i >= switch);
            let temp = |c: f64, places: usize| {
                let v = if to_f { c * 9.0 / 5.0 + 32.0 } else { c };
                round_to(v, places)
            };

            let absent = |col: Column| self.quirks.absent_sensors.contains(&col);
            let keep = |col: Column, value: f64| (!absent(col)).then_some(value);

            let (beer_ann, fridge_ann) = self.annotations_for(i);

            rows.push(Row {
                ts,
                beer_temp: keep(Column::BeerTemp, temp(beer_temp, 2)),
                beer_set: keep(Column::BeerSet, temp(beer_set, 2)),
                beer_ann,
                fridge_temp: keep(Column::FridgeTemp, temp(fridge_temp, 2)),
                fridge_set: keep(Column::FridgeSet, temp(fridge_set, 2)),
                fridge_ann,
                room_temp: keep(Column::RoomTemp, temp(room_temp, 2)),
                state,
                temp_format: if to_f { 'F' } else { 'C' },
                gravity: (self.gravity_enabled && !absent(Column::Gravity))
                    .then(|| round_to(curve::gravity_at(hours), 3)),
                grav_temp: (self.gravity_enabled && !absent(Column::GravTemp))
                    .then(|| temp(grav_temp, 1)),
            });

            ts += self.interval_secs;
        }

        rows
    }

    /// Annotations look like Fermentrack's own, plus the two shapes that break a
    /// naive parser: one containing the delimiter, one containing the quotechar.
    fn annotations_for(&self, i: usize) -> (Option<String>, Option<String>) {
        if !self.quirks.annotations {
            return (None, None);
        }
        match i {
            1 => (Some("Temp profile started, target 18.0C".to_owned()), None),
            2 => (None, Some("Fridge renamed to \"Chest 2\"".to_owned())),
            4 => (
                Some("Beer temp set to 18.0 in web interface".to_owned()),
                None,
            ),
            _ => (None, None),
        }
    }

    /// The file, header included, exactly as it would arrive over HTTP.
    pub fn csv(&self, kind: Kind) -> String {
        let mut out = String::new();
        let header: Vec<Field> = self
            .header(kind)
            .into_iter()
            .map(|h| Field::Text(h.to_owned()))
            .collect();
        write_row(&mut out, &header);
        for row in self.generate() {
            write_row(&mut out, &self.cells(&row, kind));
        }
        out
    }

    pub fn full_csv(&self) -> String {
        self.csv(Kind::Full)
    }

    pub fn graph_csv(&self) -> String {
        self.csv(Kind::Graph)
    }

    fn cells(&self, row: &Row, kind: Kind) -> Vec<Field> {
        // Fermentrack's `or 0`: a value it never had is written as the integer zero,
        // in both files.
        let num = |v: Option<f64>| v.map_or(Field::Int(0), Field::Float);
        let ann = |v: &Option<String>| v.clone().map_or(Field::Null, Field::Text);

        let mut cells = match kind {
            Kind::Full => vec![
                Field::Text(format_log_time(row.ts)),
                num(row.beer_temp),
                num(row.beer_set),
                ann(&row.beer_ann),
                num(row.fridge_temp),
                num(row.fridge_set),
                ann(&row.fridge_ann),
                num(row.room_temp),
                Field::Int(row.state as i64),
                Field::Text(row.temp_format.to_string()),
                Field::Int(self.log_id as i64),
            ],
            Kind::Graph => vec![
                Field::Text(format_log_time(row.ts)),
                num(row.beer_temp),
                num(row.beer_set),
                num(row.fridge_temp),
                num(row.fridge_set),
                num(row.room_temp),
            ],
        };

        if self.gravity_enabled {
            cells.push(num(row.gravity));
            cells.push(num(row.grav_temp));
        }
        if kind == Kind::Graph && self.model_version > 1 {
            cells.push(Field::Int(row.state as i64));
        }
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(csv: &str) -> Vec<&str> {
        csv.split_terminator("\r\n").collect()
    }

    #[test]
    fn the_header_matches_fermentracks_column_headers() {
        let plain = LogSpec::default();
        assert_eq!(
            plain.header(Kind::Full).join(","),
            "log_time,beer_temp,beer_set,beer_ann,fridge_temp,fridge_set,fridge_ann,\
             room_temp,state,temp_format,associated_beer_id"
        );
        assert_eq!(
            plain.header(Kind::Graph).join(","),
            "log_time,beer_temp,beer_set,fridge_temp,fridge_set,room_temp,state"
        );
    }

    /// `_full.csv` appends gravity after `associated_beer_id`; `_graph.csv` puts it
    /// before `state`. Positional indexing reads gravity out of the wrong column the
    /// day it meets the other file.
    #[test]
    fn gravity_columns_land_in_different_places_in_the_two_files() {
        let spec = LogSpec {
            gravity_enabled: true,
            ..LogSpec::default()
        };
        assert_eq!(
            spec.header(Kind::Full),
            [
                "log_time",
                "beer_temp",
                "beer_set",
                "beer_ann",
                "fridge_temp",
                "fridge_set",
                "fridge_ann",
                "room_temp",
                "state",
                "temp_format",
                "associated_beer_id",
                "gravity",
                "grav_temp",
            ]
        );
        assert_eq!(
            spec.header(Kind::Graph),
            [
                "log_time",
                "beer_temp",
                "beer_set",
                "fridge_temp",
                "fridge_set",
                "room_temp",
                "gravity",
                "grav_temp",
                "state",
            ]
        );
    }

    /// The version only gates `state` in `_graph.csv`. Anything that branches on
    /// `model_version` while reading `_full.csv` is solving a problem that does not
    /// exist.
    #[test]
    fn model_version_changes_the_graph_file_only() {
        let v2 = LogSpec::default();
        let v1 = LogSpec {
            model_version: 1,
            ..LogSpec::default()
        };
        assert_eq!(v1.header(Kind::Full), v2.header(Kind::Full));
        assert!(!v1.header(Kind::Graph).contains(&"state"));
        assert!(v2.header(Kind::Graph).contains(&"state"));
    }

    #[test]
    fn every_line_ends_crlf_including_the_last() {
        let csv = LogSpec {
            rows: 3,
            ..LogSpec::default()
        }
        .full_csv();
        assert!(csv.ends_with("\r\n"));
        assert_eq!(csv.matches("\r\n").count(), 4); // header + 3 rows
        assert!(!csv.contains("\n\n"));
    }

    #[test]
    fn row_count_and_cadence() {
        let spec = LogSpec {
            rows: 5,
            ..LogSpec::default()
        };
        let rows = spec.generate();
        assert_eq!(rows.len(), 5);
        for pair in rows.windows(2) {
            assert_eq!(pair[1].ts - pair[0].ts, spec.interval_secs);
        }
        assert_eq!(lines(&spec.full_csv()).len(), 6);
    }

    #[test]
    fn a_gap_skips_time_without_skipping_rows() {
        let spec = LogSpec {
            rows: 6,
            quirks: Quirks {
                gap: Some((2, 4 * 3600)),
                ..Quirks::default()
            },
            ..LogSpec::default()
        };
        let rows = spec.generate();
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[3].ts - rows[2].ts, 4 * 3600 + spec.interval_secs);
        assert_eq!(rows[4].ts - rows[3].ts, spec.interval_secs);
    }

    #[test]
    fn an_absent_sensor_is_a_bare_zero_on_every_row() {
        let spec = LogSpec {
            rows: 4,
            quirks: Quirks {
                absent_sensors: vec![Column::RoomTemp],
                ..Quirks::default()
            },
            ..LogSpec::default()
        };
        for line in lines(&spec.full_csv()).into_iter().skip(1) {
            assert_eq!(line.split(',').nth(7), Some("0"), "in {line}");
        }
    }

    #[test]
    fn the_temp_format_switch_converts_the_values_too() {
        let spec = LogSpec {
            rows: 6,
            quirks: Quirks {
                temp_format_switch: Some(3),
                ..Quirks::default()
            },
            ..LogSpec::default()
        };
        let rows = spec.generate();
        assert_eq!(rows[2].temp_format, 'C');
        assert_eq!(rows[3].temp_format, 'F');
        // 18 °C is 64.4 °F: the setpoint must move with the label, not stay at 18.
        assert_eq!(rows[2].beer_set, Some(18.0));
        assert_eq!(rows[3].beer_set, Some(64.4));
    }

    #[test]
    fn annotations_are_quoted_not_escaped_away() {
        let csv = LogSpec {
            rows: 6,
            quirks: Quirks {
                annotations: true,
                ..Quirks::default()
            },
            ..LogSpec::default()
        }
        .full_csv();
        assert!(csv.contains("\"Temp profile started, target 18.0C\""));
        assert!(csv.contains("\"Fridge renamed to \"\"Chest 2\"\"\""));
        // An unannotated row leaves both cells empty, never quoted.
        assert!(lines(&csv)[1].contains(",,"));
    }

    #[test]
    fn generation_is_deterministic() {
        let spec = Scenario::Ugly.spec();
        assert_eq!(spec.full_csv(), spec.full_csv());
        let other = LogSpec {
            seed: 2,
            ..spec.clone()
        };
        assert_ne!(spec.full_csv(), other.full_csv());
    }

    #[test]
    fn filenames_match_fermentracks() {
        let spec = LogSpec::default();
        assert_eq!(
            spec.file_name(Kind::Full),
            "Device 1 - B17 - Seltzer 17_full.csv"
        );
        assert_eq!(
            spec.file_name(Kind::Graph),
            "Device 1 - B17 - Seltzer 17_graph.csv"
        );
        assert_eq!(
            spec.remote_path(Kind::Full),
            "/data/Device 1 - B17 - Seltzer 17_full.csv"
        );
    }

    #[test]
    fn an_invalid_name_falls_back_the_way_fermentrack_does() {
        let spec = LogSpec {
            name: "Seltzer/../17".to_owned(),
            ..LogSpec::default()
        };
        assert!(!name_is_valid(&spec.name));
        assert_eq!(
            spec.file_name(Kind::Full),
            "Device 1 - B17 - NAME ERROR - _full.csv"
        );
    }

    #[test]
    fn gravity_is_only_present_when_enabled() {
        let off = LogSpec {
            rows: 2,
            ..LogSpec::default()
        };
        assert!(off.generate().iter().all(|r| r.gravity.is_none()));
        assert_eq!(lines(&off.full_csv())[1].split(',').count(), 11);

        let on = LogSpec {
            rows: 2,
            gravity_enabled: true,
            ..LogSpec::default()
        };
        assert!(on.generate().iter().all(|r| r.gravity.is_some()));
        assert_eq!(lines(&on.full_csv())[1].split(',').count(), 13);
    }

    #[test]
    fn state_stays_inside_the_documented_enum() {
        let rows = LogSpec {
            rows: 500,
            ..LogSpec::default()
        }
        .generate();
        assert!(rows.iter().all(|r| r.state <= 9));
    }

    /// The curve is anchored at pitch, so a five-row fixture starting at t=0 is all
    /// lag: flat OG and an IDLE controller. `start_hours_in` is what lets a short
    /// fixture sit somewhere the numbers move.
    #[test]
    fn start_hours_in_moves_the_fixture_along_the_curve() {
        let at_pitch = LogSpec {
            rows: 5,
            gravity_enabled: true,
            ..LogSpec::default()
        };
        let mid = LogSpec {
            start_hours_in: 14.0,
            interval_secs: 3600,
            ..at_pitch.clone()
        };

        let lag = at_pitch.generate();
        assert!(lag.iter().all(|r| r.gravity == Some(1.045)));
        assert!(lag.iter().all(|r| r.state == 0));

        let active = mid.generate();
        assert!(active.iter().all(|r| r.gravity < Some(1.045)));
        assert!(active.last().unwrap().gravity < active.first().unwrap().gravity);
        assert!(active.iter().any(|r| r.state != 0));
        // The timestamps are untouched: this moves the curve, not the clock.
        assert_eq!(active[0].ts, at_pitch.start);
    }

    #[test]
    fn every_scenario_parses_back_from_its_own_name() {
        for scenario in Scenario::ALL {
            assert_eq!(Scenario::parse(scenario.name()), Some(scenario));
        }
        assert_eq!(Scenario::parse("nope"), None);
    }
}
