use brewcore::{
    config::{Config, ConfigStore},
    db::{Db, DbStatus},
    fermentrack::{Client, LogProbe, SourceSummary, discovery},
};

use crate::error::{ApiError, ApiResult};

#[tauri::command]
pub async fn get_config(config: tauri::State<'_, ConfigStore>) -> ApiResult<Config> {
    Ok(config.get())
}

/// Whole-document replace, not a patch. The settings pane has four fields and already
/// holds the `Config` it got from [`get_config`]; a patch type of nested `Option`s
/// would be more code than the thing it configures. Last-write-wins is fine with one
/// window and no background writer — revisit if Phase 6's rule engine starts writing
/// thresholds.
///
/// Returns the *stored* value, which may differ from the argument (a trimmed URL, a
/// clamped interval). The frontend adopts the return value, so normalisation is
/// visible without a second round trip.
///
/// The write is one small `sync_all`'d file, so it runs inline rather than through
/// `spawn_blocking`.
#[tauri::command]
pub async fn set_config(
    config_store: tauri::State<'_, ConfigStore>,
    config: Config,
) -> ApiResult<Config> {
    Ok(config_store.set(config)?)
}

/// Async so Tauri runs it off the main thread: `rusqlite` is blocking, and even a
/// cheap status query should not stall the webview. Phase 2's sync path will want
/// `spawn_blocking` for the same reason.
#[tauri::command]
pub async fn db_status(db: tauri::State<'_, Db>) -> ApiResult<DbStatus> {
    Ok(db.status()?)
}

// ---------------------------------------------------------------------------
// Phase 2f: sources
// ---------------------------------------------------------------------------

/// Validate a pasted log URL without writing anything.
///
/// `input` is a whole URL or a path relative to the configured base. The error is the
/// point of this command as much as the success: `fermentrack.graph_log` and
/// `fermentrack.gravity_log` are two different mistakes and the pane says so.
#[tauri::command]
pub async fn probe_log(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    input: String,
) -> ApiResult<LogProbe> {
    let base_url = config.get().fermentrack.base_url;
    Ok(discovery::probe_log(&client, &base_url, &input).await?)
}

/// Validate and then create the source, in that order.
///
/// Deliberately re-probes rather than taking a `LogProbe` from the frontend: a write
/// must not trust state the client has been holding, and the round trip costs 2 KB.
#[tauri::command]
pub async fn add_source(
    db: tauri::State<'_, Db>,
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    input: String,
    name: Option<String>,
) -> ApiResult<SourceSummary> {
    let base_url = config.get().fermentrack.base_url;
    let probe = discovery::probe_log(&client, &base_url, &input).await?;

    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| probe.suggested_name.clone());

    // Two small statements in one transaction; `spawn_blocking` would cost more than
    // it saves, for the same reason `db_status` runs inline.
    let id = discovery::add_source(&db, &probe, &name)?;
    discovery::source_summary(&db, id)?.ok_or_else(|| ApiError {
        code: "sync.missing_state".to_owned(),
        message: format!("source {id} vanished immediately after being created"),
        detail: None,
    })
}

#[tauri::command]
pub async fn list_sources(db: tauri::State<'_, Db>) -> ApiResult<Vec<SourceSummary>> {
    Ok(discovery::source_summaries(&db)?)
}

/// One call to `GET /api/devices/`, purely to tell "wrong host or port" from "right
/// box, wrong path". It returns device ids and nothing else — no beers, no names —
/// which is why the log URL still has to be pasted.
#[tauri::command]
pub async fn check_fermentrack(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    base_url: Option<String>,
) -> ApiResult<Vec<i64>> {
    // The pane's unsaved field wins when it has one, so "Test" tests what is on screen
    // rather than what was last saved.
    let base_url = base_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| config.get().fermentrack.base_url);
    Ok(discovery::check_fermentrack(&client, &base_url).await?)
}
