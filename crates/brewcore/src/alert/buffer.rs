//! The recent rows of each journal, in memory, for the rules to read.
//!
//! `brewd` journals raw payload strings; a rule wants numbers by column. A buffer
//! holds what the longest rule looks back over and nothing older — a week of a 30 s
//! probe is a few megabytes, which a Pi Zero 2 W can spare and a journal re-read on
//! every burst could not.

use std::collections::{HashMap, VecDeque};
use std::io::Read;

use crate::{journal::TIME_COLUMN, units::Timestamp};

/// Device name → its buffer.
pub type Buffers = HashMap<String, DeviceBuffer>;

/// How far back a buffer keeps rows: 4c's longest window (168 h), half of 4b's
/// smoothing window either side, and the day [`super::Rule::Silent`] takes a median
/// interval over.
pub const RETENTION_SECS: i64 = 168 * 3_600 + 2 * 3_600 + 24 * 3_600;

#[derive(Debug, Clone, PartialEq)]
struct Row {
    time: Timestamp,
    cells: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceBuffer {
    columns: Vec<String>,
    rows: VecDeque<Row>,
}

impl DeviceBuffer {
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            rows: VecDeque::new(),
        }
    }

    /// One journal row, cells in this buffer's column order. A payload that isn't a
    /// number is an absent value: the journal keeps it, a rule can't use it.
    pub fn push(&mut self, time: Timestamp, cells: &[Option<String>]) {
        let cells = cells.iter().map(|c| c.as_deref().and_then(parse)).collect();
        self.push_parsed(time, cells);
    }

    fn push_parsed(&mut self, time: Timestamp, cells: Vec<Option<f64>>) {
        // Rows arrive in receipt order; one stamped earlier than the last is a clock
        // step, and a series must stay strictly ascending.
        if self.rows.back().is_some_and(|r| r.time > time) {
            return;
        }
        self.rows.push_back(Row { time, cells });
    }

    /// Rows from a journal file, mapped by header name: a journal rotated under an
    /// older header still fills the columns the two share. Unreadable lines are
    /// skipped, as the desktop's parser skips them.
    pub fn load_csv(&mut self, reader: impl Read) -> csv::Result<()> {
        let mut csv = csv::ReaderBuilder::new().flexible(true).from_reader(reader);
        let header = csv.headers()?.clone();
        if header.get(0) != Some(TIME_COLUMN) {
            return Ok(());
        }
        let map: Vec<Option<usize>> = header
            .iter()
            .skip(1)
            .map(|name| self.columns.iter().position(|c| c == name))
            .collect();
        for record in csv.records() {
            let Ok(record) = record else { continue };
            let Some(time) = record.get(0).and_then(|t| t.parse().ok()) else {
                continue;
            };
            let mut cells = vec![None; self.columns.len()];
            for (cell, slot) in record.iter().skip(1).zip(&map) {
                if let Some(i) = slot {
                    cells[*i] = parse(cell);
                }
            }
            self.push_parsed(Timestamp::from_secs(time), cells);
        }
        Ok(())
    }

    /// Drops rows older than [`RETENTION_SECS`] before `now`.
    pub fn trim(&mut self, now: Timestamp) {
        let cutoff = now.saturating_add_secs(-RETENTION_SECS);
        while self.rows.front().is_some_and(|r| r.time < cutoff) {
            self.rows.pop_front();
        }
    }

    /// `(time, value)` of every row at or after `since` that carries `column`,
    /// strictly ascending.
    pub fn series(&self, column: &str, since: Timestamp) -> Vec<(Timestamp, f64)> {
        let Some(i) = self.columns.iter().position(|c| c == column) else {
            return Vec::new();
        };
        let mut out: Vec<(Timestamp, f64)> = Vec::new();
        for row in self.rows.iter().filter(|r| r.time >= since) {
            let Some(v) = row.cells[i] else { continue };
            match out.last_mut() {
                // Two bursts in one second: the later is the newer reading.
                Some(last) if last.0 == row.time => last.1 = v,
                _ => out.push((row.time, v)),
            }
        }
        out
    }

    /// The time of every row, whatever it carries — a device is alive if it sends
    /// anything.
    pub fn times(&self) -> Vec<Timestamp> {
        let mut out: Vec<Timestamp> = self.rows.iter().map(|r| r.time).collect();
        out.dedup();
        out
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}

fn parse(cell: &str) -> Option<f64> {
    cell.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    fn cells(values: &[&str]) -> Vec<Option<String>> {
        values
            .iter()
            .map(|v| (!v.is_empty()).then(|| v.to_string()))
            .collect()
    }

    #[test]
    fn a_series_skips_absent_and_unreadable_cells() {
        let mut b = DeviceBuffer::new(vec!["gravity".into(), "battery".into()]);
        b.push(at(0), &cells(&["1.040", "4.1"]));
        b.push(at(60), &cells(&["", "4.1"]));
        b.push(at(120), &cells(&["nan", "4.0"]));
        b.push(at(180), &cells(&["1.039", "x"]));
        assert_eq!(
            b.series("gravity", at(0)),
            [(at(0), 1.040), (at(180), 1.039)]
        );
        assert_eq!(b.series("battery", at(60)).len(), 2);
        assert!(b.series("tilt", at(0)).is_empty());
        assert_eq!(b.times(), [at(0), at(60), at(120), at(180)]);
    }

    #[test]
    fn a_row_from_the_past_is_dropped_and_one_second_keeps_the_newer() {
        let mut b = DeviceBuffer::new(vec!["t".into()]);
        b.push(at(100), &cells(&["1"]));
        b.push(at(50), &cells(&["2"]));
        b.push(at(100), &cells(&["3"]));
        assert_eq!(b.series("t", at(0)), [(at(100), 3.0)]);
    }

    #[test]
    fn trim_keeps_the_retention_window() {
        let mut b = DeviceBuffer::new(vec!["t".into()]);
        b.push(at(0), &cells(&["1"]));
        b.push(at(RETENTION_SECS), &cells(&["2"]));
        b.trim(at(RETENTION_SECS));
        assert_eq!(b.times().len(), 2);
        b.trim(at(RETENTION_SECS + 1));
        assert_eq!(b.times(), [at(RETENTION_SECS)]);
    }

    #[test]
    fn a_journal_loads_by_header_name() {
        let mut b = DeviceBuffer::new(vec!["gravity".into(), "battery".into()]);
        let journal = "time,battery,tilt,gravity\n0,4.1,50,1.040\nbad\n60,,51,1.039\n";
        b.load_csv(journal.as_bytes()).unwrap();
        assert_eq!(
            b.series("gravity", at(0)),
            [(at(0), 1.040), (at(60), 1.039)]
        );
        assert_eq!(b.series("battery", at(0)), [(at(0), 4.1)]);
    }
}
