//! Grouping a device's messages into one reading with one timestamp.
//!
//! The iSpindle's stock firmware publishes a wake cycle as one topic per field, in a
//! burst. Stamping each field on arrival would spread one reading over a second or
//! two and draw a trend out of nothing — the same rule as 3e's `record_quick_entry`.
//! ESPHome's sensors publish independently, and the same rule groups two probes that
//! share an `update_interval`.
//!
//! A device's first message opens a burst and fixes its timestamp. The burst closes
//! when every column has arrived, when a column arrives a second time (the next cycle
//! has started), or when the window since it opened runs out.
//!
//! No clock and no I/O here: the caller passes `now` in, so every case is a plain
//! unit test.

use std::time::{Duration, Instant};

use brewcore::units::Timestamp;

/// A closed burst, ready to journal. `cells` is in the device's column order; `None`
/// is a field that did not arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub device: usize,
    pub time: Timestamp,
    pub cells: Vec<Option<String>>,
}

#[derive(Debug)]
struct Open {
    time: Timestamp,
    deadline: Instant,
    cells: Vec<Option<String>>,
    filled: usize,
}

#[derive(Debug)]
pub struct Bursts {
    window: Duration,
    widths: Vec<usize>,
    open: Vec<Option<Open>>,
}

impl Bursts {
    /// `widths[d]` is device `d`'s column count.
    pub fn new(widths: Vec<usize>, window: Duration) -> Self {
        let open = widths.iter().map(|_| None).collect();
        Self {
            window,
            widths,
            open,
        }
    }

    /// Record one message. Returns the rows it closed, oldest first: at most the
    /// device's previous burst and then this one.
    pub fn push(
        &mut self,
        device: usize,
        column: usize,
        value: String,
        now: Instant,
        time: Timestamp,
    ) -> Vec<Row> {
        let mut closed = Vec::new();
        let slot = &mut self.open[device];

        // Checked here as well as in `expire`, so the outcome does not depend on
        // whether the caller's timer or the message won the race.
        let stale = slot
            .as_ref()
            .is_some_and(|open| now >= open.deadline || open.cells[column].is_some());
        if stale {
            closed.extend(close(device, slot));
        }

        let open = slot.get_or_insert_with(|| Open {
            time,
            deadline: now + self.window,
            cells: vec![None; self.widths[device]],
            filled: 0,
        });
        open.cells[column] = Some(value);
        open.filled += 1;
        if open.filled == open.cells.len() {
            closed.extend(close(device, slot));
        }
        closed
    }

    /// Close every burst whose window has run out by `now`.
    pub fn expire(&mut self, now: Instant) -> Vec<Row> {
        let mut closed = Vec::new();
        for (device, slot) in self.open.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|open| now >= open.deadline) {
                closed.extend(close(device, slot));
            }
        }
        closed
    }

    /// When `expire` next has work to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.open.iter().flatten().map(|open| open.deadline).min()
    }

    /// Close everything, at shutdown. A partial burst is still a reading.
    pub fn drain(&mut self) -> Vec<Row> {
        (0..self.open.len())
            .filter_map(|device| close(device, &mut self.open[device]))
            .collect()
    }
}

fn close(device: usize, slot: &mut Option<Open>) -> Option<Row> {
    slot.take().map(|open| Row {
        device,
        time: open.time,
        cells: open.cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(2);
    const T0: Timestamp = Timestamp::from_secs(1_790_000_000);

    fn ms(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn cells(values: &[Option<&str>]) -> Vec<Option<String>> {
        values.iter().map(|v| v.map(str::to_owned)).collect()
    }

    #[test]
    fn a_complete_burst_closes_on_its_last_field_with_the_first_fields_time() {
        let base = Instant::now();
        let mut bursts = Bursts::new(vec![3], WINDOW);

        assert!(bursts.push(0, 1, "1.012".into(), base, T0).is_empty());
        assert!(
            bursts
                .push(
                    0,
                    0,
                    "45.2".into(),
                    ms(base, 300),
                    T0.saturating_add_secs(1)
                )
                .is_empty()
        );
        let rows = bursts.push(0, 2, "3.9".into(), ms(base, 600), T0.saturating_add_secs(1));

        assert_eq!(
            rows,
            vec![Row {
                device: 0,
                time: T0,
                cells: cells(&[Some("45.2"), Some("1.012"), Some("3.9")]),
            }]
        );
        assert_eq!(bursts.next_deadline(), None);
    }

    #[test]
    fn a_repeated_field_closes_the_open_burst_and_starts_the_next() {
        let base = Instant::now();
        let mut bursts = Bursts::new(vec![2], WINDOW);
        let t1 = T0.saturating_add_secs(1);

        bursts.push(0, 0, "18.0".into(), base, T0);
        let rows = bursts.push(0, 0, "18.1".into(), ms(base, 500), t1);

        assert_eq!(
            rows,
            vec![Row {
                device: 0,
                time: T0,
                cells: cells(&[Some("18.0"), None]),
            }]
        );
        assert_eq!(bursts.drain()[0].time, t1);
    }

    #[test]
    fn a_burst_that_never_completes_closes_when_its_window_runs_out() {
        let base = Instant::now();
        let mut bursts = Bursts::new(vec![2], WINDOW);

        bursts.push(0, 1, "1.012".into(), base, T0);
        assert_eq!(bursts.next_deadline(), Some(base + WINDOW));
        assert!(bursts.expire(ms(base, 1_999)).is_empty());

        let rows = bursts.expire(base + WINDOW);
        assert_eq!(rows[0].cells, cells(&[None, Some("1.012")]));
        assert_eq!(bursts.next_deadline(), None);
    }

    /// The timer and the message can race. A message after the deadline must not
    /// join the stale burst whichever one the caller saw first.
    #[test]
    fn a_late_message_starts_a_new_burst_even_if_expire_was_not_called() {
        let base = Instant::now();
        let mut bursts = Bursts::new(vec![2], WINDOW);
        let later = T0.saturating_add_secs(5);

        bursts.push(0, 0, "18.0".into(), base, T0);
        let rows = bursts.push(0, 1, "19.0".into(), ms(base, 5_000), later);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells, cells(&[Some("18.0"), None]));
        let rest = bursts.drain();
        assert_eq!(rest[0].time, later);
        assert_eq!(rest[0].cells, cells(&[None, Some("19.0")]));
    }

    #[test]
    fn devices_group_independently() {
        let base = Instant::now();
        let mut bursts = Bursts::new(vec![2, 2], WINDOW);
        let t1 = T0.saturating_add_secs(1);

        bursts.push(0, 0, "a0".into(), base, T0);
        bursts.push(1, 0, "b0".into(), ms(base, 100), t1);
        let rows = bursts.push(0, 1, "a1".into(), ms(base, 200), t1);
        assert_eq!(
            rows,
            vec![Row {
                device: 0,
                time: T0,
                cells: cells(&[Some("a0"), Some("a1")]),
            }]
        );

        let rows = bursts.push(1, 1, "b1".into(), ms(base, 300), t1);
        assert_eq!(rows[0].device, 1);
        assert_eq!(rows[0].time, t1);
    }

    #[test]
    fn a_single_column_device_journals_every_message() {
        let base = Instant::now();
        let mut bursts = Bursts::new(vec![1], WINDOW);
        assert_eq!(bursts.push(0, 0, "1".into(), base, T0).len(), 1);
        assert_eq!(bursts.push(0, 0, "2".into(), ms(base, 10), T0).len(), 1);
        assert!(bursts.drain().is_empty());
    }
}
