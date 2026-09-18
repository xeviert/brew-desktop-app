mod commands;
mod error;
mod logging;

use brewcore::{
    config::{ConfigStore, resolve_config_path},
    db::{Db, resolve_db_path},
};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // First, so everything below it is logged. `Db::open` already emits
            // "database ready"; until now there was no subscriber to receive it.
            let log_dir = logging::resolve_log_dir(&app.path().app_log_dir()?);
            app.manage(logging::init(&log_dir)?);
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                log_dir = %log_dir.display(),
                "brew-app starting"
            );

            // `app_config_dir()`, not `app_data_dir()`: the same folder on Windows,
            // different on Linux, and the config file belongs in the config dir.
            //
            // `open_or_default` because an unreadable config must not stop the app
            // launching, whereas `Db::open` uses `?` — an app with no database has
            // nothing to show.
            let config_path = resolve_config_path(&app.path().app_config_dir()?);
            app.manage(ConfigStore::open_or_default(config_path));

            let db_path = resolve_db_path(&app.path().app_data_dir()?);
            app.manage(Db::open(db_path)?);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::db_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
