//! Phase Vc. What `brewd` and the desktop app agree on about a journal, so neither
//! side restates it.
//!
//! A journal is a CSV file with a header row, `\n`-terminated, one record per line.
//! The first column is always [`TIME_COLUMN`], UTC epoch seconds stamped by `brewd` at
//! receipt. Every other column is a raw payload as the device published it: no unit
//! conversion and no [`Metric`](crate::db::Metric) mapping, because only the desktop
//! config knows whether an iSpindle's `gravity` is SG or °P. An empty cell means the
//! field did not arrive in that burst.

use serde::{Deserialize, Serialize};

use crate::units::Timestamp;

/// The first header cell of every journal.
pub const TIME_COLUMN: &str = "time";

pub const JOURNAL_EXTENSION: &str = "csv";

/// One row of `GET /journals/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub name: String,
    pub bytes: u64,
    pub modified: Timestamp,
}

/// Whether `name` is something `brewd` would serve: a bare `.csv` filename of
/// `[A-Za-z0-9_.-]`, not starting with a dot. No separators, so no traversal.
pub fn is_journal_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_suffix(JOURNAL_EXTENSION)
        .and_then(|s| s.strip_suffix('.'))
    else {
        return false;
    };
    !stem.is_empty()
        && !stem.starts_with('.')
        && stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `(device, epoch)` for a journal `brewd` rotated away when its header changed:
/// `esp32-1.1790000000.csv` → `("esp32-1", 1790000000)`. `None` for a live journal,
/// including one whose device name merely contains a dot.
pub fn rotated_stem(name: &str) -> Option<(&str, i64)> {
    let stem = name.strip_suffix(JOURNAL_EXTENSION)?.strip_suffix('.')?;
    let (device, epoch) = stem.rsplit_once('.')?;
    if device.is_empty() || epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((device, epoch.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_names_are_bare_csv_filenames() {
        for ok in ["ispindel-1.csv", "esp32_1.csv", "esp32-1.1790000000.csv"] {
            assert!(is_journal_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".csv",
            "..csv",
            ".hidden.csv",
            "../etc/passwd.csv",
            "a/b.csv",
            "a\\b.csv",
            "a b.csv",
            "journal.txt",
            "journal.csv.bak",
            "journal",
        ] {
            assert!(!is_journal_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_rotated_name_carries_its_device_and_epoch() {
        assert_eq!(
            rotated_stem("esp32-1.1790000000.csv"),
            Some(("esp32-1", 1_790_000_000))
        );
        assert_eq!(rotated_stem("a.b.1.csv"), Some(("a.b", 1)));
        for live in [
            "esp32-1.csv",
            "a.b.csv",
            ".1790000000.csv",
            "a..csv",
            "a.1x.csv",
        ] {
            assert_eq!(rotated_stem(live), None, "{live}");
        }
    }

    #[test]
    fn an_entry_serialises_with_epoch_seconds() {
        let entry = JournalEntry {
            name: "ispindel-1.csv".to_owned(),
            bytes: 42,
            modified: Timestamp::from_secs(1_790_000_000),
        };
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"{"name":"ispindel-1.csv","bytes":42,"modified":1790000000}"#
        );
    }
}
