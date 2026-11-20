//! The alert log: every transition of every alert, one CSV line each.
//!
//! `brewd` appends it beside its state; the desktop reads it with the same `Range`
//! resume as a journal (`alert::sync`), so an alert raised at 3 am is in
//! `brewery.db` the next time the app opens. Lines are facts, never rewritten: a
//! reader that applies them in order, twice, ends where it would have once.
//!
//! `brewd` writes it through the same `Journal` as a device's readings, so it gets the
//! same promises: a header row, one record per `\n`-terminated line, a half-written
//! last line trimmed on restart.

use serde::{Deserialize, Serialize};

use super::Rule;
use crate::{Error, Result, units::Timestamp};

/// Every column after `time`, in order.
pub const LOG_COLUMNS: [&str; 7] = [
    "alert_id", "batch_id", "rule", "device", "event", "until", "detail",
];

pub const LOG_HEADER: &str = "time,alert_id,batch_id,rule,device,event,until,detail";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEvent {
    Raised,
    Cleared,
    Acked,
    /// `until` says to when.
    Snoozed,
    /// Pushed to the phone.
    Notified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    pub time: Timestamp,
    pub alert_id: u64,
    pub batch_id: i64,
    pub rule: Rule,
    pub device: Option<String>,
    pub event: LogEvent,
    pub until: Option<Timestamp>,
    /// What the rule saw, on `raised`; why, on a `cleared` the rule didn't decide.
    pub detail: String,
}

impl LogRecord {
    /// The record after `time`, as [`LOG_COLUMNS`] orders them.
    pub fn cells(&self) -> Vec<Option<String>> {
        vec![
            Some(self.alert_id.to_string()),
            Some(self.batch_id.to_string()),
            Some(self.rule.as_str().to_owned()),
            self.device.clone(),
            Some(self.event.as_str().to_owned()),
            self.until.map(|t| t.as_secs().to_string()),
            Some(self.detail.clone()).filter(|d| !d.is_empty()),
        ]
    }
}

impl LogEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            LogEvent::Raised => "raised",
            LogEvent::Cleared => "cleared",
            LogEvent::Acked => "acked",
            LogEvent::Snoozed => "snoozed",
            LogEvent::Notified => "notified",
        }
    }
}

/// Records from complete lines without the header. A line that doesn't read is an
/// error naming it: the log is ours, so a bad line is a bug, not weather.
pub fn read_records(body: &[u8]) -> Result<Vec<LogRecord>> {
    let mut csv = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(body);
    csv.deserialize()
        .map(|r| {
            r.map_err(|e| Error::InvalidAlertLog {
                detail: e.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_read_back_with_commas_and_empty_options() {
        let records = vec![
            LogRecord {
                time: Timestamp::from_secs(1_790_000_000),
                alert_id: 7,
                batch_id: 3,
                rule: Rule::TempHigh,
                device: None,
                event: LogEvent::Raised,
                until: None,
                detail: "20.5 °C, above 20.0 °C for 10 min".into(),
            },
            LogRecord {
                time: Timestamp::from_secs(1_790_000_060),
                alert_id: 8,
                batch_id: 3,
                rule: Rule::Silent,
                device: Some("ispindel-1".into()),
                event: LogEvent::Snoozed,
                until: Some(Timestamp::from_secs(1_790_014_400)),
                detail: String::new(),
            },
        ];
        let mut csv = csv::WriterBuilder::new()
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(Vec::new());
        for r in &records {
            let time = r.time.as_secs().to_string();
            let cells = r.cells();
            let cells = cells.iter().map(|c| c.as_deref().unwrap_or(""));
            csv.write_record(std::iter::once(time.as_str()).chain(cells))
                .unwrap();
        }
        let out = csv.into_inner().unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        assert_eq!(
            text,
            "1790000000,7,3,temp_high,,raised,,\"20.5 °C, above 20.0 °C for 10 min\"\n\
             1790000060,8,3,silent,ispindel-1,snoozed,1790014400,\n"
        );
        assert_eq!(read_records(&out).unwrap(), records);
    }

    #[test]
    fn a_line_that_does_not_read_is_an_error() {
        let err = read_records(b"1,2,3,boiling,,raised,,\n").unwrap_err();
        assert_eq!(err.code(), "alert.invalid_log");
    }
}
