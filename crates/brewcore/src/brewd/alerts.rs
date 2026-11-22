//! Phase 6. The desktop's conversation with `brewd` about alerts.
//!
//! One round, on the sync timer: push the watches if `brewd` holds a different set,
//! then read whatever the alert log has gained since last time. The push needs the
//! token; the read doesn't, so an app without a token still sees what `brewd` raised
//! under the watches it last had.
//!
//! The log is read like a journal — `Range` from a stored offset, complete lines only,
//! offset moved in the same transaction as the rows — but it is not a source: it holds
//! no readings, and a failed round must not back the reading sync off.

use crate::{
    Error, Result,
    alert::{LOG_HEADER, WatchSet, log::read_records},
    config::Config,
    db::{Db, alert},
    journal::{Client, FetchOutcome, log_url, split_complete_lines, split_first_record},
    sync::blocking,
    units::Timestamp,
};

use super::discovery::base_url;

pub const WATCHES_PATH: &str = "/watches";
pub const ALERT_LOG_PATH: &str = "/alerts/log";

/// Whether the watches went out this round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchPush {
    Pushed,
    /// `brewd` already had this set.
    Unchanged,
    /// No token, so nothing was sent.
    NoToken,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AlertSyncReport {
    pub watches: WatchPush,
    pub watched: usize,
    /// Log lines that changed a row.
    pub applied: usize,
}

pub async fn sync_alerts(db: &Db, client: &Client, config: &Config) -> Result<AlertSyncReport> {
    let base = base_url(&config.brewd.base_url)?.to_owned();

    let set = {
        let (db, config) = (db.clone(), config.clone());
        blocking(move || db.read(|conn| alert::build_watches(conn, &config))).await?
    };
    let watched = set.len();
    let set = WatchSet::new(set);
    let watches = push_watches(client, &base, &config.brewd.token, &set).await?;
    let applied = read_log(db, client, &base).await?;
    Ok(AlertSyncReport {
        watches,
        watched,
        applied,
    })
}

/// Sends the set only when `brewd`'s differs — read back rather than remembered, so a
/// `brewd` that lost its state file gets the set again on the next round.
async fn push_watches(
    client: &Client,
    base: &str,
    token: &str,
    set: &WatchSet,
) -> Result<WatchPush> {
    if token.trim().is_empty() {
        return Ok(WatchPush::NoToken);
    }
    let url = log_url(base, WATCHES_PATH)?;
    let held = client.fetch_body(&url, Some(4 * 1024 * 1024)).await?;
    if WatchSet::from_json(&held).is_ok_and(|held| held == *set) {
        return Ok(WatchPush::Unchanged);
    }
    let json = serde_json::to_vec(set).map_err(|e| Error::InvalidWatch {
        reason: e.to_string(),
    })?;
    client
        .send(reqwest::Method::PUT, &url, token.trim(), Some(json))
        .await?;
    Ok(WatchPush::Pushed)
}

async fn read_log(db: &Db, client: &Client, base: &str) -> Result<usize> {
    let url = log_url(base, ALERT_LOG_PATH)?;
    let offset = {
        let (db, base) = (db.clone(), base.to_owned());
        blocking(move || db.read(|conn| alert::alert_offset(conn, &base))).await?
    };
    let (bytes, at) = match client.fetch(&url, offset).await? {
        FetchOutcome::Unchanged { .. } => return Ok(0),
        // `brewd` lost its log, or this is another `brewd` at the same address. Every
        // write is an upsert on brewd's ids, so reading it again from 0 is safe.
        FetchOutcome::Truncated { .. } => match client.fetch(&url, 0).await? {
            FetchOutcome::Fresh { bytes, offset, .. } => (bytes, offset),
            _ => return Ok(0),
        },
        FetchOutcome::Fresh { bytes, offset, .. } => (bytes, offset),
    };

    let mut body = bytes.as_slice();
    let mut header_len = 0;
    if at == 0 {
        let Some((header, rest)) = split_first_record(body) else {
            return Ok(0);
        };
        let line = String::from_utf8_lossy(header);
        if line.trim_end() != LOG_HEADER {
            return Err(Error::InvalidAlertLog {
                detail: format!("unexpected header '{}'", line.trim_end()),
            });
        }
        header_len = header.len();
        body = rest;
    }
    let (complete, _) = split_complete_lines(body);
    let records = read_records(complete)?;
    let next = at + (header_len + complete.len()) as u64;

    let (db, base) = (db.clone(), base.to_owned());
    blocking(move || {
        db.write(|conn| alert::apply_records(conn, &base, &records, next, Timestamp::now()))
    })
    .await
}

/// `POST /alerts/<id>/ack`. `brewd` answering 404 is an alert already cleared.
pub async fn ack(client: &Client, config: &Config, id: i64) -> Result<()> {
    write(client, config, &format!("/alerts/{id}/ack"), None, id).await
}

/// `POST /alerts/<id>/snooze` until `until`.
pub async fn snooze(client: &Client, config: &Config, id: i64, until: Timestamp) -> Result<()> {
    let body = serde_json::json!({ "until": until })
        .to_string()
        .into_bytes();
    write(
        client,
        config,
        &format!("/alerts/{id}/snooze"),
        Some(body),
        id,
    )
    .await
}

/// `POST /notify/test`: one notification through `brewd`'s ntfy, end to end.
pub async fn test_notification(client: &Client, config: &Config) -> Result<()> {
    write(client, config, "/notify/test", None, 0).await
}

async fn write(
    client: &Client,
    config: &Config,
    path: &str,
    body: Option<Vec<u8>>,
    id: i64,
) -> Result<()> {
    let base = base_url(&config.brewd.base_url)?;
    let token = config.brewd.token.trim();
    if token.is_empty() {
        return Err(Error::BrewdTokenUnset);
    }
    let url = log_url(base, path)?;
    match client.send(reqwest::Method::POST, &url, token, body).await {
        Err(Error::HttpStatus { status: 404, .. }) if id > 0 => Err(Error::NoSuchAlert {
            id: id.unsigned_abs(),
        }),
        other => other,
    }
}
