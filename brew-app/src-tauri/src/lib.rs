//! Thin Tauri shell: resolve the database path, own the [`Db`], expose commands.
//! Any logic that appears here belongs in `brewcore` instead.

mod commands;
mod error;

use brewcore::db::{Db, resolve_db_path};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // `brewcore` cannot ask Tauri where app data lives, so the path is
            // resolved here and handed down. `BREWAPP_DB` overrides it.
            let data_dir = app.path().app_data_dir()?;
            let db_path = resolve_db_path(&data_dir);
            app.manage(Db::open(db_path)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::db_status])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
