//! Phase 2f. Turning a pasted URL into a source.
//!
//! No Fermentrack API enumerates beers or gravity logs, and the log id *and the beer
//! name* are both in the filename, so there is nothing to guess and nothing to list.
//! The user pastes the URL once; this module proves it is a beer `_full.csv` by
//! fetching the first [`VALIDATION_PREFIX_BYTES`] and reading the header, then writes
//! the `source` and `sync_state` rows that 2c–2e already know how to drive.
//!
//! Three rules hold this together:
//!
//! 1. **Validate on bytes, never on a [`FetchOutcome`].** A capped body is
//!    `Fresh { offset: 0 }`, which is what a genuine complete first fetch looks like;
//!    a probe that produced one could be handed to `ingest` and would store a truncated
//!    log under an offset claiming the whole of it. Hence [`Client::fetch_prefix`].
//! 2. **Classify by column, not by filename.** [`csv_log::inspect_header`] makes a
//!    pasted `_graph.csv` and a pasted gravity log two *different* messages, because
//!    they are two different mistakes: one has the right beer and the wrong file, the
//!    other has the wrong device entirely.
//! 3. **`base_url` + `remote_path` must rebuild the URL forever.** [`probe_log`]
//!    asserts the round trip through [`log_url`] before it returns, so the pair stored
//!    across two tables can never drift into naming a URL we cannot fetch.
//!
//! The device and log ids are *diagnostic only*. They are read out of the filename when
//! it has Fermentrack's shape and are never used to construct a path — the beer name
//! sits between them and the suffix, and no API returns it.

use serde::Serialize;

use crate::{
    Error, Result,
    db::{
        Db,
        tier1::{self, SourceKind},
    },
    fermentrack::{
        client::{Client, log_url},
        csv_log::{self, HeaderInfo},
        store,
    },
    units::Timestamp,
};

/// How much of a log to read before deciding what it is.
///
/// A Fermentrack header is ~120 bytes; 2 KB is generous enough that a future column set
/// still fits and small enough to be free on a slow Pi.
pub const VALIDATION_PREFIX_BYTES: u64 = 2048;

/// What a URL turned out to be. Everything the "Add a log" form shows, and everything
/// [`add_source`] needs to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogProbe {
    /// Absolute and percent-encoded: exactly what was fetched.
    pub url: String,
    /// Origin of `url`. Goes in `source.config`.
    pub base_url: String,
    /// Decoded, as `sync_state.remote_path` stores it and as the UI shows it.
    pub remote_path: String,
    /// The filename, decoded. What the graph/gravity refusals quote back.
    pub file_name: String,
    /// The beer name out of the filename, else the filename itself. A default for the
    /// name field, never authoritative — the user may rename freely.
    pub suggested_name: String,
    pub device_id: Option<u32>,
    pub log_id: Option<u32>,
    pub header: HeaderInfo,
}

/// One source as the UI lists it: the `source` row, its sync state, and how fresh it is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceSummary {
    pub id: i64,
    pub kind: SourceKind,
    pub name: String,
    pub remote_path: Option<String>,
    pub byte_offset: u64,
    /// Timestamp of the newest row *in the log*, which runs ahead of `newest_reading`
    /// when rows parsed but carried no usable value.
    pub last_ts: Option<Timestamp>,
    /// Newest row actually stored. The one number that answers "is this working".
    pub newest_reading: Option<Timestamp>,
    pub last_error: Option<String>,
    pub synced_at: Option<Timestamp>,
}

/// What goes in `source.config`. Diagnostic plus the one field that is load-bearing.
///
/// `remote_path` is deliberately *not* here: it lives in `sync_state`, where the
/// `ON CONFLICT` that resets `byte_offset` on a repoint can see it change. Two copies
/// would eventually disagree, and the copy the fetch does not read would be the one the
/// UI shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SourceConfig {
    pub base_url: String,
    pub device_id: Option<u32>,
    pub log_id: Option<u32>,
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------

/// Fetch the head of a log and say what it is.
///
/// `input` is whatever the user pasted: a whole URL from their browser, or a path
/// relative to `base_url`. [`log_url`] already handles both, and already refuses a `..`
/// segment and a control character.
pub async fn probe_log(client: &Client, base_url: &str, input: &str) -> Result<LogProbe> {
    let url = log_url(base_url, input)?;
    let (origin, remote_path) = split_url(&url)?;

    let file_name = remote_path
        .rsplit('/')
        .next()
        .unwrap_or(&remote_path)
        .to_owned();

    let bytes = client.fetch_prefix(&url, VALIDATION_PREFIX_BYTES).await?;
    let header = csv_log::inspect_header(&file_name, &bytes)?;

    // The invariant that lets `source.config.base_url` and `sync_state.remote_path`
    // live in different tables. A path we cannot re-encode to the URL we just fetched
    // would sync today and 404 after a restart, with nothing in the UI to explain it.
    if log_url(&origin, &remote_path).ok().as_deref() != Some(url.as_str()) {
        return Err(Error::InvalidUrl {
            url: url.clone(),
            reason: "its path does not survive being stored and rebuilt".to_owned(),
        });
    }

    let (device_id, log_id, name) = parse_file_name(&file_name);
    Ok(LogProbe {
        url,
        base_url: origin,
        remote_path,
        suggested_name: name.unwrap_or_else(|| file_name.clone()),
        file_name,
        device_id,
        log_id,
        header,
    })
}

/// Confirm a host is a Fermentrack at all, returning its device ids.
///
/// `GET /api/devices/` is the only endpoint that enumerates anything. It lists ids and
/// nothing else — no names, no beers — so it is worth exactly one call, to tell "wrong
/// port" from "right box, wrong path".
pub async fn check_fermentrack(client: &Client, base_url: &str) -> Result<Vec<i64>> {
    let url = log_url(base_url, "/api/devices/")?;
    // 64 KB: a device list is tens of bytes, and the cap is what stops a misdirected
    // URL streaming a dashboard into memory.
    let body = client
        .fetch_body(&url, Some(64 * 1024))
        .await
        // A 404 here is not a renamed log, it is a host with no such endpoint — which
        // is the entire question this function was asked.
        .map_err(|err| match err {
            Error::LogMissing { url } => Error::NotFermentrack {
                url,
                detail: "it has no /api/devices/ endpoint".to_owned(),
            },
            other => other,
        })?;

    serde_json::from_slice::<Vec<i64>>(&body).map_err(|_| Error::NotFermentrack {
        url,
        detail: format!(
            "/api/devices/ answered something other than a list of device ids: {}",
            super::preview_bytes(&body)
        ),
    })
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Create the source a probe describes, with its `sync_state` row, in one transaction.
///
/// Both rows or neither: a `source` with no `sync_state` is listed in the UI and then
/// fails every poll with [`Error::MissingSyncState`], which reads like a bug in the
/// sync loop rather than a half-finished insert.
///
/// The offset starts at 0 — the prefix we validated was thrown away deliberately, so
/// the first real sync reads the file from the beginning and `bytes_consumed` means
/// what it always means.
pub fn add_source(db: &Db, probe: &LogProbe, name: &str) -> Result<i64> {
    let config = serde_json::to_string(&SourceConfig {
        base_url: probe.base_url.clone(),
        device_id: probe.device_id,
        log_id: probe.log_id,
    })
    .map_err(|err| Error::InvalidUrl {
        url: probe.url.clone(),
        reason: err.to_string(),
    })?;

    db.write(|conn| {
        let tx = conn.transaction()?;
        let id = tier1::create_source(&tx, SourceKind::FermentrackBeerLog, name, Some(&config))?;
        store::upsert_sync_state_in(&tx, id, &probe.remote_path)?;
        tx.commit()?;
        Ok(id)
    })
}

/// Every source with its sync state. What the sources pane lists, and what 2g's status
/// footer will read.
///
/// A `LEFT JOIN`, not an inner one: a source whose `sync_state` row is missing must
/// still be listed, precisely so that the broken one is the one you can see.
pub fn source_summaries(db: &Db) -> Result<Vec<SourceSummary>> {
    db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.kind, s.name, y.remote_path, y.byte_offset, y.last_ts, \
                    y.last_error, y.synced_at \
             FROM source s LEFT JOIN sync_state y ON y.source_id = s.id \
             ORDER BY s.id",
        )?;
        let rows: Vec<SourceSummary> = stmt
            .query_map([], |row| {
                Ok(SourceSummary {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    name: row.get(2)?,
                    remote_path: row.get(3)?,
                    // Same cast as `load_sync_state`: the CHECK refuses a negative, and
                    // a byte offset never approaches `i64::MAX`.
                    byte_offset: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                    last_ts: row.get(5)?,
                    newest_reading: None,
                    last_error: row.get(6)?,
                    synced_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        rows.into_iter()
            .map(|mut summary| {
                summary.newest_reading = tier1::newest_reading(conn, summary.id)?;
                Ok(summary)
            })
            .collect()
    })
}

/// One source by id, or `None`.
pub fn source_summary(db: &Db, id: i64) -> Result<Option<SourceSummary>> {
    Ok(source_summaries(db)?
        .into_iter()
        .find(|summary| summary.id == id))
}

// ---------------------------------------------------------------------------
// URLs and filenames
// ---------------------------------------------------------------------------

/// Split an absolute URL into `(origin, decoded path)`.
fn split_url(url: &str) -> Result<(String, String)> {
    let parsed = reqwest::Url::parse(url).map_err(|err| Error::InvalidUrl {
        url: url.to_owned(),
        reason: err.to_string(),
    })?;
    let origin = parsed.origin().ascii_serialization();
    if origin == "null" {
        return Err(Error::InvalidUrl {
            url: url.to_owned(),
            reason: "it has no host".to_owned(),
        });
    }
    Ok((origin, decode_path(parsed.path())))
}

/// Percent-decode a URL path. Invalid UTF-8 is left as written rather than replaced,
/// so that the round-trip check in [`probe_log`] catches it instead of a `�` reaching
/// the database.
fn decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: Option<&u8>| b.and_then(|b| (*b as char).to_digit(16));
        let escape = (bytes[i] == b'%')
            .then(|| hex(bytes.get(i + 1)).zip(hex(bytes.get(i + 2))))
            .flatten();
        match escape {
            Some((hi, lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_owned())
}

/// Read `Device {device_id} - B{log_id} - {name}_full.csv` back apart.
///
/// Everything is optional because nothing depends on it: a log served from a renamed
/// path is still ingested, it just shows no ids. Classification is the header's job.
fn parse_file_name(file_name: &str) -> (Option<u32>, Option<u32>, Option<String>) {
    let stem = file_name
        .strip_suffix("_full.csv")
        .or_else(|| file_name.strip_suffix("_graph.csv"))
        .unwrap_or(file_name);

    // `Gravity Device 2 - L5 - …` is a gravity log; the header refuses it, but reading
    // its ids means the error message can still name the device.
    let (rest, id_prefix) = match stem.strip_prefix("Gravity Device ") {
        Some(rest) => (rest, 'L'),
        None => match stem.strip_prefix("Device ") {
            Some(rest) => (rest, 'B'),
            None => return (None, None, None),
        },
    };

    let Some((device, rest)) = rest.split_once(" - ") else {
        return (None, None, None);
    };
    let device_id = device.parse::<u32>().ok();

    let Some((log, name)) = rest.split_once(" - ") else {
        return (device_id, None, None);
    };
    let log_id = log
        .strip_prefix(id_prefix)
        .and_then(|digits| digits.parse::<u32>().ok());

    // Fermentrack's own fallback for a name that would not survive a path. Offering it
    // as a source name would be worse than offering the filename.
    let name = (!name.is_empty() && !name.starts_with("NAME ERROR")).then(|| name.to_owned());
    (device_id, log_id, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filename is the only place the device and beer ids appear, and reading them
    /// back wrong would label a source with another fermenter's number.
    #[test]
    fn a_fermentrack_filename_yields_its_ids_and_beer_name() {
        assert_eq!(
            parse_file_name("Device 1 - B17 - Saison 17_full.csv"),
            (Some(1), Some(17), Some("Saison 17".to_owned()))
        );
        assert_eq!(
            parse_file_name("Device 1 - B17 - Saison 17_graph.csv"),
            (Some(1), Some(17), Some("Saison 17".to_owned()))
        );
        assert_eq!(
            parse_file_name("Gravity Device 2 - L5 - Tilt Red_full.csv"),
            (Some(2), Some(5), Some("Tilt Red".to_owned()))
        );
    }

    /// A beer name with a hyphen in it splits on ` - ` too. Only the first two splits
    /// are ids, and the rest is the name however many separators it contains.
    #[test]
    fn a_beer_name_containing_a_separator_stays_whole() {
        assert_eq!(
            parse_file_name("Device 3 - B9 - West Coast - Batch 2_full.csv"),
            (Some(3), Some(9), Some("West Coast - Batch 2".to_owned()))
        );
    }

    /// Fermentrack writes `NAME ERROR` when a beer name would not survive being put in
    /// a path. Offering that as the source name would be worse than the filename.
    #[test]
    fn fermentracks_name_error_fallback_is_not_offered_as_a_name() {
        let (device, log, name) = parse_file_name("Device 1 - B4 - NAME ERROR - _full.csv");
        assert_eq!((device, log), (Some(1), Some(4)));
        assert_eq!(name, None);
    }

    /// A log served from a renamed path must still be addable; it simply shows no ids.
    #[test]
    fn an_unrecognisable_filename_is_not_an_error() {
        assert_eq!(parse_file_name("brewlog.csv"), (None, None, None));
        assert_eq!(parse_file_name("Device x - B1 - S_full.csv").0, None);
    }

    /// `remote_path` is stored decoded so the UI never shows `%20`, and `log_url`
    /// re-encodes it. If these two ever disagree the source syncs today and 404s after
    /// a restart, which is why `probe_log` asserts the round trip.
    #[test]
    fn a_decoded_path_re_encodes_to_the_url_it_came_from() {
        let url = "http://fermentrack.local/data/Device%201%20-%20B17%20-%20Saison%2017_full.csv";
        let (origin, path) = split_url(url).unwrap();
        assert_eq!(origin, "http://fermentrack.local");
        assert_eq!(path, "/data/Device 1 - B17 - Saison 17_full.csv");
        assert_eq!(log_url(&origin, &path).unwrap(), url);
    }

    /// A Fermentrack behind a path prefix keeps that prefix in `remote_path`, not in
    /// `base_url`: `log_url` appends, so a prefix in both would be doubled.
    #[test]
    fn a_base_path_stays_in_the_remote_path() {
        let url = "http://box:8080/fermentrack/data/Device%202%20-%20B1%20-%20S_full.csv";
        let (origin, path) = split_url(url).unwrap();
        assert_eq!(origin, "http://box:8080");
        assert_eq!(path, "/fermentrack/data/Device 2 - B1 - S_full.csv");
        assert_eq!(log_url(&origin, &path).unwrap(), url);
    }

    /// A stray `%` is not an escape. Decoding it as one would silently eat the two
    /// characters after it.
    #[test]
    fn a_lone_percent_sign_decodes_to_itself() {
        assert_eq!(
            decode_path("/data/100%25 Brett.csv"),
            "/data/100% Brett.csv"
        );
        assert_eq!(decode_path("/data/50%zz.csv"), "/data/50%zz.csv");
    }
}
