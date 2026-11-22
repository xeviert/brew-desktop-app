//! `log_time` formatting, hand-rolled.
//!
//! Fermentrack writes `self.log_time.astimezone(utc).strftime('%Y/%m/%d %H:%M:%SZ')`
//! (`app/models.py`, `BeerLogPoint.data_point`). This module reproduces that from
//! epoch seconds without `chrono`, deliberately: the Phase 2b parser will use
//! `chrono`, and a generator that shared its date maths with the parser could not
//! catch a bug in either. Two independent implementations that agree are evidence;
//! one implementation compared against itself is not.

/// `%Y/%m/%d %H:%M:%SZ` in UTC. Negative epoch seconds work, though nothing in a
/// fermentation log should ever predate 1970.
pub fn format_log_time(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{y:04}/{m:02}/{d:02} {h:02}:{min:02}:{s:02}Z")
}

/// Howard Hinnant's `civil_from_days`, shifting the era so that the year starts in
/// March and the leap day lands at the end of it.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed points, each checkable by hand or against `date -u -d @N`.
    #[test]
    fn known_instants() {
        assert_eq!(format_log_time(0), "1970/01/01 00:00:00Z");
        assert_eq!(format_log_time(1), "1970/01/01 00:00:01Z");
        assert_eq!(format_log_time(86_399), "1970/01/01 23:59:59Z");
        assert_eq!(format_log_time(86_400), "1970/01/02 00:00:00Z");
        assert_eq!(format_log_time(1_758_153_600), "2025/09/18 00:00:00Z");
        assert_eq!(format_log_time(1_000_000_000), "2001/09/09 01:46:40Z");
    }

    /// The two cases a hand-rolled calendar gets wrong.
    #[test]
    fn leap_days() {
        assert_eq!(format_log_time(951_782_400), "2000/02/29 00:00:00Z"); // 400-year rule
        assert_eq!(format_log_time(1_709_164_800), "2024/02/29 00:00:00Z");
        assert_eq!(format_log_time(1_709_251_200), "2024/03/01 00:00:00Z");
        // 1900 was not a leap year: 1900/03/01, not 1900/02/29.
        assert_eq!(format_log_time(-2_203_891_200), "1900/03/01 00:00:00Z");
    }

    #[test]
    fn year_boundaries() {
        assert_eq!(format_log_time(1_735_689_599), "2024/12/31 23:59:59Z");
        assert_eq!(format_log_time(1_735_689_600), "2025/01/01 00:00:00Z");
    }

    /// Every day for eight years, walked forward one day at a time and compared to
    /// an independently-incremented calendar. Catches an off-by-one that fixed
    /// points would miss.
    #[test]
    fn agrees_with_a_naive_calendar_over_eight_years() {
        fn leap(y: i64) -> bool {
            (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
        }
        let mdays = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

        let (mut y, mut m, mut d) = (2020i64, 1u32, 1u32);
        let start = 1_577_836_800; // 2020/01/01
        for day in 0..(365 * 8) {
            assert_eq!(
                format_log_time(start + day * 86_400),
                format!("{y:04}/{m:02}/{d:02} 00:00:00Z")
            );
            let len = if m == 2 && leap(y) {
                29
            } else {
                mdays[(m - 1) as usize]
            };
            d += 1;
            if d > len {
                d = 1;
                m += 1;
            }
            if m > 12 {
                m = 1;
                y += 1;
            }
        }
    }
}
