//! Alert state: what is raised now, and what has been said about it.
//!
//! Without this every burst would re-fire. One alert per `(batch, rule, device)` while
//! its condition holds; it is pushed to the phone once, again only when a snooze runs
//! out, and never after an ack. `brewd` persists the book, so a reboot resumes it
//! rather than raising everything afresh.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{Condition, LogEvent, LogRecord, Rule, Verdict, Watch};
use crate::units::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: u64,
    pub batch_id: i64,
    pub code: String,
    pub rule: Rule,
    pub device: Option<String>,
    pub raised_at: Timestamp,
    pub detail: String,
    pub acked_at: Option<Timestamp>,
    pub snoozed_until: Option<Timestamp>,
    pub notified_at: Option<Timestamp>,
}

impl Alert {
    /// What the phone shows: the batch, then what happened.
    pub fn title(&self) -> String {
        match (self.rule, &self.device) {
            (Rule::Silent, Some(device)) => format!("{}: {device} silent", self.code),
            (Rule::Due | Rule::Overdue, _) => {
                format!("{}: {} {}", self.code, self.subject(), self.rule.title())
            }
            _ => format!("{}: {}", self.code, self.rule.title()),
        }
    }

    /// A due alert's detail opens with what is due, up to the dash.
    fn subject(&self) -> &str {
        self.detail
            .split_once(" — ")
            .map_or(self.detail.as_str(), |(s, _)| s)
    }

    fn record(&self, time: Timestamp, event: LogEvent, detail: String) -> LogRecord {
        LogRecord {
            time,
            alert_id: self.id,
            batch_id: self.batch_id,
            rule: self.rule,
            device: self.device.clone(),
            event,
            until: None,
            detail,
        }
    }

    fn key(&self) -> (i64, Rule, Option<&str>) {
        (self.batch_id, self.rule, self.device.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertBook {
    next_id: u64,
    alerts: Vec<Alert>,
}

impl AlertBook {
    /// Ids start at `now` in milliseconds, so a book started over after its state file
    /// was lost never reuses an id the desktop already holds.
    pub fn new(now: Timestamp) -> Self {
        Self {
            next_id: u64::try_from(now.as_secs()).unwrap_or(0) * 1_000,
            alerts: Vec::new(),
        }
    }

    /// Every alert currently raised.
    pub fn alerts(&self) -> &[Alert] {
        &self.alerts
    }

    /// Applies one watch's verdicts. An alert of this batch whose rule the watch no
    /// longer produces — a band removed, a device unbound — is cleared, not orphaned.
    pub fn step(
        &mut self,
        watch: &Watch,
        conditions: Vec<Condition>,
        now: Timestamp,
    ) -> Vec<LogRecord> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for Condition {
            rule,
            device,
            verdict,
        } in conditions
        {
            seen.insert((rule, device.clone()));
            let key = (watch.batch_id, rule, device.as_deref());
            let active = self.alerts.iter().position(|a| a.key() == key);
            match (verdict, active) {
                (Verdict::Raise { detail }, None) => {
                    let alert = Alert {
                        id: self.next_id,
                        batch_id: watch.batch_id,
                        code: watch.code.clone(),
                        rule,
                        device,
                        raised_at: now,
                        detail,
                        acked_at: None,
                        snoozed_until: None,
                        notified_at: None,
                    };
                    self.next_id += 1;
                    out.push(alert.record(now, LogEvent::Raised, alert.detail.clone()));
                    self.alerts.push(alert);
                }
                (Verdict::Clear, Some(i)) => {
                    let alert = self.alerts.remove(i);
                    out.push(alert.record(now, LogEvent::Cleared, String::new()));
                }
                _ => {}
            }
        }
        let mut i = 0;
        while i < self.alerts.len() {
            let a = &self.alerts[i];
            if a.batch_id == watch.batch_id && !seen.contains(&(a.rule, a.device.clone())) {
                let alert = self.alerts.remove(i);
                out.push(alert.record(now, LogEvent::Cleared, "rule no longer applies".into()));
            } else {
                i += 1;
            }
        }
        out
    }

    /// Clears every alert of a batch no longer watched — packaged, archived, muted.
    pub fn retain(&mut self, watched: &HashSet<i64>, now: Timestamp) -> Vec<LogRecord> {
        let (keep, drop): (Vec<_>, Vec<_>) = std::mem::take(&mut self.alerts)
            .into_iter()
            .partition(|a| watched.contains(&a.batch_id));
        self.alerts = keep;
        drop.into_iter()
            .map(|a| a.record(now, LogEvent::Cleared, "batch no longer watched".into()))
            .collect()
    }

    /// No more notifications for this alert; it stays raised until its rule clears.
    pub fn ack(&mut self, id: u64, now: Timestamp) -> Option<LogRecord> {
        let alert = self.alerts.iter_mut().find(|a| a.id == id)?;
        alert.acked_at = Some(now);
        alert.snoozed_until = None;
        Some(alert.record(now, LogEvent::Acked, String::new()))
    }

    /// Notify again at `until` if it still holds and nobody has acked it.
    pub fn snooze(&mut self, id: u64, until: Timestamp, now: Timestamp) -> Option<LogRecord> {
        let alert = self.alerts.iter_mut().find(|a| a.id == id)?;
        alert.snoozed_until = Some(until);
        let mut record = alert.record(now, LogEvent::Snoozed, String::new());
        record.until = Some(until);
        Some(record)
    }

    /// Alerts owed a notification: never sent, or a snooze that has run out since.
    pub fn due(&self, now: Timestamp) -> Vec<Alert> {
        self.alerts
            .iter()
            .filter(|a| a.acked_at.is_none())
            .filter(|a| match (a.notified_at, a.snoozed_until) {
                (None, _) => true,
                (Some(sent), Some(until)) => until <= now && sent < until,
                (Some(_), None) => false,
            })
            .cloned()
            .collect()
    }

    pub fn notified(&mut self, id: u64, now: Timestamp) -> Option<LogRecord> {
        let alert = self.alerts.iter_mut().find(|a| a.id == id)?;
        alert.notified_at = Some(now);
        Some(alert.record(now, LogEvent::Notified, String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::watch::tests::watch;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    fn cond(rule: Rule, verdict: Verdict) -> Condition {
        Condition {
            rule,
            device: None,
            verdict,
        }
    }

    fn raise(detail: &str) -> Verdict {
        Verdict::Raise {
            detail: detail.into(),
        }
    }

    fn events(records: &[LogRecord]) -> Vec<(Rule, LogEvent)> {
        records.iter().map(|r| (r.rule, r.event)).collect()
    }

    #[test]
    fn a_held_condition_is_one_alert_and_unknown_holds_it() {
        let w = watch(1);
        let mut book = AlertBook::new(at(1_000));
        let first = book.step(&w, vec![cond(Rule::TempHigh, raise("a"))], at(1));
        assert_eq!(events(&first), [(Rule::TempHigh, LogEvent::Raised)]);
        assert_eq!(first[0].alert_id, 1_000_000);

        assert!(
            book.step(&w, vec![cond(Rule::TempHigh, raise("b"))], at(2))
                .is_empty()
        );
        assert!(
            book.step(&w, vec![cond(Rule::TempHigh, Verdict::Unknown)], at(3))
                .is_empty()
        );
        assert_eq!(book.alerts().len(), 1);
        assert_eq!(book.alerts()[0].detail, "a");

        let cleared = book.step(&w, vec![cond(Rule::TempHigh, Verdict::Clear)], at(4));
        assert_eq!(events(&cleared), [(Rule::TempHigh, LogEvent::Cleared)]);
        assert!(book.alerts().is_empty());

        let again = book.step(&w, vec![cond(Rule::TempHigh, raise("c"))], at(5));
        assert_eq!(again[0].alert_id, 1_000_001, "a new alert, a new id");
    }

    #[test]
    fn a_rule_the_watch_stops_producing_is_cleared() {
        let w = watch(1);
        let mut book = AlertBook::new(at(0));
        book.step(
            &w,
            vec![
                cond(Rule::TempHigh, raise("a")),
                cond(Rule::Finished, raise("b")),
            ],
            at(1),
        );
        let out = book.step(&w, vec![cond(Rule::Finished, Verdict::Unknown)], at(2));
        assert_eq!(events(&out), [(Rule::TempHigh, LogEvent::Cleared)]);
        assert_eq!(out[0].detail, "rule no longer applies");
        assert_eq!(book.alerts()[0].rule, Rule::Finished);
    }

    #[test]
    fn devices_are_separate_alerts_and_other_batches_are_untouched() {
        let mut book = AlertBook::new(at(0));
        let silent = |device: &str| Condition {
            rule: Rule::Silent,
            device: Some(device.into()),
            verdict: raise("quiet"),
        };
        book.step(&watch(1), vec![silent("a"), silent("b")], at(1));
        book.step(&watch(2), vec![silent("a")], at(1));
        assert_eq!(book.alerts().len(), 3);
        assert_eq!(book.alerts()[0].title(), "SELTZER-001: a silent");

        let out = book.retain(&HashSet::from([2]), at(2));
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .all(|r| r.batch_id == 1 && r.event == LogEvent::Cleared)
        );
        assert_eq!(book.alerts().len(), 1);
    }

    #[test]
    fn a_due_alert_is_titled_by_its_subject_and_ends_when_the_subject_leaves() {
        let mut book = AlertBook::new(at(0));
        let due = |key: &str| Condition {
            rule: Rule::Due,
            device: Some(key.into()),
            verdict: raise("Primary — 3 d in"),
        };
        book.step(&watch(1), vec![due("step:5")], at(1));
        assert_eq!(book.alerts()[0].title(), "SELTZER-001: Primary due");

        let out = book.step(&watch(1), vec![due("step:6")], at(2));
        let events: Vec<_> = out.iter().map(|r| (r.device.as_deref(), r.event)).collect();
        assert_eq!(
            events,
            [
                (Some("step:6"), LogEvent::Raised),
                (Some("step:5"), LogEvent::Cleared)
            ]
        );
    }

    #[test]
    fn notified_once_again_after_a_snooze_and_never_after_an_ack() {
        let w = watch(1);
        let mut book = AlertBook::new(at(0));
        book.step(&w, vec![cond(Rule::Stalled, raise("flat"))], at(10));
        let id = book.alerts()[0].id;
        assert_eq!(book.due(at(10)).len(), 1);
        book.notified(id, at(11));
        assert!(book.due(at(12)).is_empty());

        let snoozed = book.snooze(id, at(100), at(20)).unwrap();
        assert_eq!(snoozed.until, Some(at(100)));
        assert!(book.due(at(99)).is_empty());
        assert_eq!(book.due(at(100)).len(), 1);
        book.notified(id, at(100));
        assert!(book.due(at(500)).is_empty(), "once per snooze");

        book.snooze(id, at(600), at(500));
        book.ack(id, at(550));
        assert!(book.due(at(700)).is_empty());
        assert!(book.ack(999, at(700)).is_none());
    }

    #[test]
    fn a_book_survives_serde() {
        let mut book = AlertBook::new(at(5));
        book.step(&watch(1), vec![cond(Rule::Finished, raise("done"))], at(6));
        let json = serde_json::to_string(&book).unwrap();
        assert_eq!(serde_json::from_str::<AlertBook>(&json).unwrap(), book);
    }
}
