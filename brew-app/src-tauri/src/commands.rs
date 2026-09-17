use brewcore::db::{Db, DbStatus};

use crate::error::ApiResult;

/// Async so Tauri runs it off the main thread: `rusqlite` is blocking, and even a
/// cheap status query should not stall the webview. Phase 2's sync path will want
/// `spawn_blocking` for the same reason.
#[tauri::command]
pub async fn db_status(db: tauri::State<'_, Db>) -> ApiResult<DbStatus> {
    Ok(db.status()?)
}
