//! Phase Vd. `brewd` journals, generated.
//!
//! The format is `brewcore::journal::format`'s, restated here rather than imported:
//! this crate is the oracle, and an oracle that shares the writer cannot catch it.
//! What `brewd` does that the parser has to survive:
//!
//! - **`\n` terminators**, never `\r\n` — `brewd` writes with the `csv` crate and
//!   `Terminator::Any(b'\n')`, unlike Fermentrack's Python `excel` dialect.
//! - **`time` is integer epoch seconds, one per burst.** Every field of an iSpindle
//!   wake cycle shares it; that is what `brewd`'s `burst.rs` exists for.
//! - **Cells are raw payloads.** A field that did not arrive in the burst is an empty
//!   cell, and ESPHome publishes `nan` for a Dallas probe that failed to read — which
//!   Rust's `f64::from_str` happily accepts.
//! - **A restart mid-write leaves half a line**, which `brewd` trims on its next start.
//!
//! These are placeholders for the Phase 0 captures, exactly as the Fermentrack
//! fixtures were: the tests stay, the bytes get replaced.

use crate::csv_writer::quote;
use crate::curve::{Rng, activity_at, gravity_at};

/// The first header cell of every journal.
pub const JOURNAL_TIME_COLUMN: &str = "time";

/// Which kind of device `brewd` journalled. Column names follow
/// `crates/brewd/deploy/brewd.example.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Ispindel,
    Esp32,
}

impl Device {
    pub fn columns(self) -> &'static [&'static str] {
        match self {
            Device::Ispindel => &[
                "tilt",
                "gravity",
                "temperature",
                "battery",
                "rssi",
                "interval",
            ],
            Device::Esp32 => &["vessel_temp", "chamber_temp"],
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JournalQuirks {
    /// A field goes missing for the middle third of the file: the chamber probe
    /// unplugged on an ESP32, `rssi` absent from the burst on an iSpindle.
    pub dropout: bool,
    /// ESPHome's `nan` on the first row, and a payload with a comma and a quote on
    /// the last — `brewd` quotes it, and the parser must drop it as a value without
    /// losing the row.
    pub odd_cells: bool,
    /// One more record, cut off halfway and with no `\n`: `brewd` killed mid-write.
    pub half_written_tail: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalSpec {
    pub device: Device,
    /// The device name in `brewd.toml`; the journal is `<name>.csv`.
    pub name: String,
    /// Epoch seconds of the first record.
    pub start: i64,
    /// Where in the ferment the first record sits, so a short journal can be mid-curve.
    pub start_hours_in: f64,
    pub interval_secs: i64,
    pub rows: usize,
    pub seed: u64,
    pub quirks: JournalQuirks,
}

impl Default for JournalSpec {
    fn default() -> Self {
        Self {
            device: Device::Esp32,
            name: "esp32-1".to_owned(),
            start: 1_790_000_000,
            start_hours_in: 24.0,
            interval_secs: 60,
            rows: 120,
            seed: 1,
            quirks: JournalQuirks::default(),
        }
    }
}

impl JournalSpec {
    pub fn file_name(&self) -> String {
        format!("{}.csv", self.name)
    }

    /// Where `brewd` serves it.
    pub fn remote_path(&self) -> String {
        format!("/journals/{}", self.file_name())
    }

    /// The name `brewd` renames the journal to when its header changes.
    pub fn rotated_name(&self, at: i64) -> String {
        format!("{}.{at}.csv", self.name)
    }

    pub fn header(&self) -> Vec<&'static str> {
        std::iter::once(JOURNAL_TIME_COLUMN)
            .chain(self.device.columns().iter().copied())
            .collect()
    }

    /// The time of record `i`.
    pub fn time_at(&self, i: usize) -> i64 {
        self.start + i as i64 * self.interval_secs
    }

    /// Every record's cells, `time` first. Absent fields are empty strings. Includes
    /// the half-written record when that quirk is on; [`JournalSpec::csv`] cuts it.
    pub fn records(&self) -> Vec<Vec<String>> {
        let mut rng = Rng::new(self.seed);
        let total = self.rows + usize::from(self.quirks.half_written_tail);
        let dropout = self.rows / 3..(2 * self.rows).div_ceil(3);

        (0..total)
            .map(|i| {
                let hours = self.start_hours_in + (i as i64 * self.interval_secs) as f64 / 3600.0;
                let mut cells = vec![self.time_at(i).to_string()];
                match self.device {
                    Device::Ispindel => {
                        let gravity = gravity_at(hours) + rng.noise(0.0015);
                        cells.extend([
                            format!("{:.2}", 25.0 + (gravity - 1.0) * 800.0 + rng.noise(0.3)),
                            format!("{gravity:.4}"),
                            format!("{:.1}", 20.0 + activity_at(hours) * 1.5 + rng.noise(0.1)),
                            format!("{:.2}", 4.10 - hours * 0.0005),
                            format!("{:.0}", -67.0 + rng.noise(4.0)),
                            self.interval_secs.to_string(),
                        ]);
                        if self.quirks.dropout && dropout.contains(&i) {
                            cells[5].clear();
                        }
                    }
                    Device::Esp32 => {
                        cells.extend([
                            format!("{:.1}", 18.0 + activity_at(hours) * 2.0 + rng.noise(0.06)),
                            format!("{:.1}", 17.0 + rng.noise(0.2)),
                        ]);
                        if self.quirks.dropout && dropout.contains(&i) {
                            cells[2].clear();
                        }
                    }
                }
                if self.quirks.odd_cells {
                    let last = cells.len() - 1;
                    if i == 0 {
                        cells[1] = "nan".to_owned();
                    } else if i + 1 == self.rows {
                        cells[last] = "1,5\"".to_owned();
                    }
                }
                cells
            })
            .collect()
    }

    /// The journal, byte for byte.
    pub fn csv(&self) -> String {
        let mut out = String::new();
        push_record(&mut out, &self.header());
        for (i, cells) in self.records().iter().enumerate() {
            let mut line = String::new();
            push_record(&mut line, cells);
            if i == self.rows {
                // Never cut on the `\n` itself, or the half line would be a whole one.
                line.truncate(line.len() / 2);
            }
            out.push_str(&line);
        }
        out
    }
}

fn push_record(out: &mut String, cells: &[impl AsRef<str>]) {
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&quote(cell.as_ref()));
    }
    out.push('\n');
}

/// Named journals for `cargo xtask gen-fixture --format journal` and the goldens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalScenario {
    /// An iSpindle mid-ferment at its default 15-minute wake cycle.
    Ispindel,
    /// Two Dallas probes at ESPHome's default 60 s.
    Esp32,
    /// Every quirk, on an ESP32.
    Ugly,
}

impl JournalScenario {
    pub const ALL: [JournalScenario; 3] = [Self::Ispindel, Self::Esp32, Self::Ugly];

    pub fn name(self) -> &'static str {
        match self {
            Self::Ispindel => "ispindel",
            Self::Esp32 => "esp32",
            Self::Ugly => "ugly",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scenario| scenario.name() == s)
    }

    pub fn spec(self) -> JournalSpec {
        match self {
            Self::Ispindel => JournalSpec {
                device: Device::Ispindel,
                name: "ispindel-1".to_owned(),
                interval_secs: 900,
                rows: 96,
                ..JournalSpec::default()
            },
            Self::Esp32 => JournalSpec::default(),
            Self::Ugly => JournalSpec {
                name: "esp32-9".to_owned(),
                rows: 12,
                quirks: JournalQuirks {
                    dropout: true,
                    odd_cells: true,
                    half_written_tail: true,
                },
                ..JournalSpec::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_is_lf_terminated_with_a_time_column() {
        let csv = JournalScenario::Esp32.spec().csv();
        assert!(csv.starts_with("time,vessel_temp,chamber_temp\n1790000000,"));
        assert!(csv.ends_with('\n'));
        assert!(!csv.contains('\r'));
        assert_eq!(csv.lines().count(), 121);
    }

    #[test]
    fn an_ispindel_burst_shares_one_time() {
        let spec = JournalScenario::Ispindel.spec();
        let records = spec.records();
        assert_eq!(records[1][0], "1790000900");
        assert!(
            records
                .iter()
                .all(|r| r.len() == 7 && r[1..].iter().all(|c| !c.is_empty()))
        );
    }

    #[test]
    fn the_ugly_journal_carries_every_quirk() {
        let csv = JournalScenario::Ugly.spec().csv();
        let lines: Vec<&str> = csv.split('\n').collect();
        assert!(lines[1].contains(",nan,"), "{}", lines[1]);
        assert!(lines.iter().any(|l| l.ends_with(",")), "a dropout");
        assert!(lines[12].ends_with(",\"1,5\"\"\""), "{}", lines[12]);
        let tail = lines.last().unwrap();
        assert!(
            !tail.is_empty() && !csv.ends_with('\n'),
            "a half-written tail"
        );
    }
}
