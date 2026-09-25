//! Phase Vd. Turning a journal `brewd` lists into a source.
//!
//! Unlike Fermentrack there is something to list: `GET /journals/` names every journal,
//! rotated ones included. The user picks one, [`probe_journal`] reads its header, and
//! [`add_source`] writes the `source` and `sync_state` rows together.
//!
//! The base URL is **not** stored per source. There is one `brewd`, and
//! `config.brewd.base_url` is read on every poll, so moving the Pi to a new address is
//! one setting and not one edit per journal.

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    brewd::dialect::{self, JournalHeader},
    db::{
        Db,
        tier1::{self, SourceKind},
    },
    journal::{Client, JournalEntry, is_journal_name, log_url, preview_bytes, store},
};

/// Where `brewd` lists and serves journals.
pub const JOURNALS_PATH: &str = "/journals/";

/// How much of a journal to read before deciding what it is. A header is tens of bytes.
pub const PROBE_PREFIX_BYTES: u64 = 2048;

/// What goes in `source.config`: the file the source was added from, for display.
/// `sync_state.remote_path` is the load-bearing copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalSourceConfig {
    pub journal: String,
}

/// A journal, read far enough to say what syncing it would store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalProbe {
    /// The filename, as `GET /journals/` lists it.
    pub journal: String,
    /// `/journals/<journal>`, as `sync_state.remote_path` stores it.
    pub remote_path: String,
    /// The device name: the filename less `.csv`, and less the epoch for a rotated one.
    pub suggested_name: String,
    /// `brewd` renamed this journal away when its header changed. It will not grow.
    pub rotated: bool,
    pub header: JournalHeader,
}

/// The address to reach `brewd` at, or [`Error::BrewdUnconfigured`].
pub fn base_url(configured: &str) -> Result<&str> {
    match configured.trim() {
        "" => Err(Error::BrewdUnconfigured),
        url => Ok(url),
    }
}

/// Every journal `brewd` has, sorted by name. Doubles as the "Test connection" check:
/// only `brewd` answers this path with this shape.
pub async fn list_journals(client: &Client, base_url: &str) -> Result<Vec<JournalEntry>> {
    let url = log_url(base_url, JOURNALS_PATH)?;
    // A listing is ~80 bytes a journal; 1 MB is ten thousand of them, and the cap is
    // what stops a misdirected URL streaming a dashboard into memory.
    let body = client
        .fetch_body(&url, Some(1024 * 1024))
        .await
        .map_err(|err| match err {
            Error::LogMissing { url } => Error::NotBrewd {
                url,
                detail: format!("it has no {JOURNALS_PATH} endpoint"),
            },
            other => other,
        })?;

    serde_json::from_slice(&body).map_err(|_| Error::NotBrewd {
        url,
        detail: format!(
            "{JOURNALS_PATH} answered something other than a journal list: {}",
            preview_bytes(&body)
        ),
    })
}

/// Read a journal's header and say what it would store.
pub async fn probe_journal(client: &Client, base_url: &str, journal: &str) -> Result<JournalProbe> {
    let remote_path = remote_path(journal)?;
    let url = log_url(base_url, &remote_path)?;
    let bytes = client.fetch_prefix(&url, PROBE_PREFIX_BYTES).await?;
    let header = dialect::inspect_header(&bytes)?;

    let rotated = crate::journal::rotated_stem(journal);
    let suggested_name = match rotated {
        Some((device, _)) => device,
        None => journal.strip_suffix(".csv").unwrap_or(journal),
    };
    Ok(JournalProbe {
        journal: journal.to_owned(),
        remote_path,
        suggested_name: suggested_name.to_owned(),
        rotated: rotated.is_some(),
        header,
    })
}

/// `/journals/<journal>`, for a name `brewd` would serve.
pub fn remote_path(journal: &str) -> Result<String> {
    if !is_journal_name(journal) {
        return Err(Error::InvalidUrl {
            url: journal.to_owned(),
            reason: "not a journal filename".to_owned(),
        });
    }
    Ok(format!("{JOURNALS_PATH}{journal}"))
}

/// Create the source a probe describes, with its `sync_state` row, in one transaction.
/// Both rows or neither, for the reason Fermentrack's `add_source` gives.
pub fn add_source(db: &Db, probe: &JournalProbe, name: &str) -> Result<i64> {
    let config = serde_json::to_string(&JournalSourceConfig {
        journal: probe.journal.clone(),
    })
    .map_err(|err| Error::InvalidUrl {
        url: probe.remote_path.clone(),
        reason: err.to_string(),
    })?;

    db.write(|conn| {
        let tx = conn.transaction()?;
        let id = tier1::create_source(&tx, SourceKind::BrewdJournal, name, Some(&config))?;
        store::upsert_sync_state_in(&tx, id, &probe.remote_path)?;
        tx.commit()?;
        Ok(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_path_is_only_ever_a_journal_filename() {
        assert_eq!(remote_path("esp32-1.csv").unwrap(), "/journals/esp32-1.csv");
        for bad in ["../x.csv", "a/b.csv", "esp32-1", ""] {
            assert_eq!(
                remote_path(bad).unwrap_err().code(),
                "fermentrack.url",
                "{bad}"
            );
        }
    }

    #[test]
    fn an_empty_base_url_is_unconfigured_not_invalid() {
        assert_eq!(base_url("  ").unwrap_err().code(), "brewd.unconfigured");
        assert_eq!(base_url(" http://pi:8080 ").unwrap(), "http://pi:8080");
    }
}
