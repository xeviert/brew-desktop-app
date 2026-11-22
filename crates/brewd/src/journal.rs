//! One append-only journal per device — the raw archive under v2. The desktop app
//! syncs these over HTTP `Range`, so the only promise that matters is the one
//! `brewcore::journal::split_complete_lines` relies on: every byte before the last
//! `\n` is a complete record, and nothing before it ever changes.
//!
//! Each record is one `write_all` on an unbuffered `File`, so the HTTP side sees it at
//! once. No `fsync` per line: on a microSD that is wear for no gain, and a power cut
//! loses at most the kernel's last few seconds, which the restart trim below cleans up.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use brewcore::journal::{JOURNAL_EXTENSION, TIME_COLUMN};
use brewcore::units::Timestamp;

/// How far into an existing journal to look for its header before calling it foreign.
const MAX_HEADER_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
}

impl Journal {
    /// Open `<dir>/<device>.csv` for appending, creating it with a header if needed.
    ///
    /// A half-written last line — `brewd` killed mid-write, or a power cut — is cut
    /// back to the last `\n`. The desktop never commits an offset past an unterminated
    /// record, so nothing it has stored refers to those bytes.
    ///
    /// A journal whose header is not this config's columns is renamed to
    /// `<device>.<now>.csv` and a new one started. Appending rows under the wrong
    /// header would misfile every value, and the desktop's resume check already
    /// treats a changed header as a new file.
    pub fn open(dir: &Path, device: &str, columns: &[&str], now: Timestamp) -> io::Result<Self> {
        let path = dir.join(format!("{device}.{JOURNAL_EXTENSION}"));
        let header = encode(std::iter::once(TIME_COLUMN).chain(columns.iter().copied()))?;

        if path.exists() {
            trim_partial_tail(&path)?;
            let existing = read_header(&path)?;
            if !existing.is_empty() && existing != header {
                let rotated = dir.join(format!("{device}.{}.{JOURNAL_EXTENSION}", now.as_secs()));
                if rotated.exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "cannot rotate {}: {} exists",
                            path.display(),
                            rotated.display()
                        ),
                    ));
                }
                fs::rename(&path, &rotated)?;
                tracing::warn!(
                    from = %path.display(),
                    to = %rotated.display(),
                    "journal header changed, rotated the old journal"
                );
            }
        }

        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        if file.metadata()?.len() == 0 {
            file.write_all(&header)?;
        }
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. `None` is an empty cell.
    ///
    /// A `\r` or `\n` inside a value becomes a space. The CSV writer would quote it
    /// correctly, but then a record would span lines, and the restart trim — which
    /// scans back for a bare `\n` — could cut one in half.
    pub fn append(&mut self, time: Timestamp, cells: &[Option<String>]) -> io::Result<()> {
        let time = time.as_secs().to_string();
        let cells: Vec<String> = cells
            .iter()
            .map(|cell| {
                cell.as_deref()
                    .unwrap_or("")
                    .trim()
                    .replace(['\r', '\n'], " ")
            })
            .collect();
        let line = encode(std::iter::once(time.as_str()).chain(cells.iter().map(String::as_str)))?;
        self.file.write_all(&line)
    }
}

/// One `\n`-terminated CSV record.
fn encode<'a>(fields: impl IntoIterator<Item = &'a str>) -> io::Result<Vec<u8>> {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    writer.write_record(fields)?;
    writer.into_inner().map_err(|e| e.into_error())
}

fn read_header(path: &Path) -> io::Result<Vec<u8>> {
    let mut header = Vec::new();
    BufReader::new(File::open(path)?.take(MAX_HEADER_BYTES)).read_until(b'\n', &mut header)?;
    Ok(header)
}

/// Cut the file back to just past its last `\n`, or to empty if it has none.
fn trim_partial_tail(path: &Path) -> io::Result<()> {
    const CHUNK: u64 = 4096;

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    let mut end = len;
    let mut buf = vec![0; CHUNK as usize];
    let keep = loop {
        if end == 0 {
            break 0;
        }
        let start = end.saturating_sub(CHUNK);
        let chunk = &mut buf[..(end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;
        if let Some(i) = chunk.iter().rposition(|&b| b == b'\n') {
            break start + i as u64 + 1;
        }
        end = start;
    };

    if keep < len {
        file.set_len(keep)?;
        tracing::warn!(
            path = %path.display(),
            dropped_bytes = len - keep,
            "journal ended mid-record, trimmed to the last complete line"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brewcore::journal::split_complete_lines;

    const T0: Timestamp = Timestamp::from_secs(1_790_000_000);

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn some(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|v| Some((*v).to_owned())).collect()
    }

    #[test]
    fn a_new_journal_gets_a_header_and_lf_terminated_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "ispindel-1", &["tilt", "gravity"], T0).unwrap();
        journal.append(T0, &some(&["45.2", "1.012"])).unwrap();
        journal
            .append(T0.saturating_add_secs(900), &[None, Some("1.011".into())])
            .unwrap();

        assert_eq!(
            read(journal.path()),
            "time,tilt,gravity\n1790000000,45.2,1.012\n1790000900,,1.011\n"
        );
    }

    #[test]
    fn reopening_with_the_same_columns_appends() {
        let dir = tempfile::tempdir().unwrap();
        Journal::open(dir.path(), "d", &["a"], T0)
            .unwrap()
            .append(T0, &some(&["1"]))
            .unwrap();
        let mut journal = Journal::open(dir.path(), "d", &["a"], T0).unwrap();
        journal.append(T0, &some(&["2"])).unwrap();

        assert_eq!(read(journal.path()), "time,a\n1790000000,1\n1790000000,2\n");
    }

    #[test]
    fn a_half_written_last_line_is_trimmed_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.csv");
        fs::write(&path, "time,a\n1790000000,1\n1790000030,1.").unwrap();

        let mut journal = Journal::open(dir.path(), "d", &["a"], T0).unwrap();
        journal
            .append(T0.saturating_add_secs(60), &some(&["2"]))
            .unwrap();

        assert_eq!(read(&path), "time,a\n1790000000,1\n1790000060,2\n");
    }

    #[test]
    fn a_half_written_header_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.csv");
        fs::write(&path, "time,").unwrap();

        Journal::open(dir.path(), "d", &["a"], T0).unwrap();
        assert_eq!(read(&path), "time,a\n");
    }

    /// The trim scans back in chunks; a last line longer than one chunk must still be
    /// found whole.
    #[test]
    fn the_trim_finds_a_newline_more_than_one_chunk_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.csv");
        let partial = "9".repeat(10_000);
        fs::write(&path, format!("time,a\n{partial}")).unwrap();

        Journal::open(dir.path(), "d", &["a"], T0).unwrap();
        assert_eq!(read(&path), "time,a\n");
    }

    #[test]
    fn a_changed_header_rotates_the_old_journal() {
        let dir = tempfile::tempdir().unwrap();
        Journal::open(dir.path(), "d", &["a"], T0)
            .unwrap()
            .append(T0, &some(&["1"]))
            .unwrap();

        let later = T0.saturating_add_secs(3600);
        let journal = Journal::open(dir.path(), "d", &["a", "b"], later).unwrap();

        assert_eq!(read(journal.path()), "time,a,b\n");
        assert_eq!(
            read(&dir.path().join("d.1790003600.csv")),
            "time,a\n1790000000,1\n"
        );
    }

    #[test]
    fn a_value_with_a_comma_or_newline_stays_one_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(dir.path(), "d", &["a", "b"], T0).unwrap();
        journal
            .append(T0, &some(&["x,y", "two\r\nlines \"q\""]))
            .unwrap();

        let bytes = fs::read(journal.path()).unwrap();
        let (complete, partial) = split_complete_lines(&bytes);
        assert!(partial.is_empty());
        assert_eq!(complete.iter().filter(|&&b| b == b'\n').count(), 2);
        assert_eq!(
            read(journal.path()),
            "time,a,b\n1790000000,\"x,y\",\"two  lines \"\"q\"\"\"\n"
        );
    }
}
