use brewcore::{
    config::{Config, ConfigStore},
    db::{Db, DbStatus},
};

use crate::error::ApiResult;

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
