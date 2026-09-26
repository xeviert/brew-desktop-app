mod commands;
mod error;
mod logging;
mod sync_events;

#[cfg(test)]
mod ipc_tests;

use brewcore::{
    config::{ConfigStore, resolve_config_path},
    db::{Db, resolve_db_path},
    fermentrack::{Client, discovery},
    sync::SyncManager,
};
use tauri::{Manager, RunEvent, Runtime, ipc::Invoke};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
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

            // One per application, shared: a fresh `reqwest::Client` per poll throws
            // away the connection pool and leaves sockets in `TIME_WAIT`.
            app.manage(Client::new()?);

            // Phase 2g. Managed here but *started* below, inside the async runtime:
            // `SyncManager::spawn` calls `tokio::spawn`, and `setup` runs before there
            // is a runtime context to spawn into.
            let manager = SyncManager::new(
                app.state::<ConfigStore>().inner().clone(),
                std::sync::Arc::new(sync_events::TauriSink::new(app.handle().clone())),
            );
            app.manage(manager);

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { start_polling(&handle) });

            Ok(())
        })
        .invoke_handler(handler())
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Blocking on purpose. A poll aborted mid-`commit` leaves the transaction
            // for SQLite to roll back on close rather than for us to finish; the few
            // hundred milliseconds this can cost are the price of a clean WAL.
            let manager = handle.state::<SyncManager>().inner().clone();
            tauri::async_runtime::block_on(manager.shutdown());
            tracing::info!("polling stopped; exiting");
        }
    });
}

/// Every command the frontend can call. A function rather than inline in [`run`] so
/// `ipc_tests` can register the same list on a mock runtime.
fn handler<R: Runtime>() -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::get_config,
        commands::set_config,
        commands::db_status,
        commands::probe_log,
        commands::add_source,
        commands::list_sources,
        commands::check_fermentrack,
        commands::list_journals,
        commands::probe_journal,
        commands::add_journal_source,
        commands::list_vessels,
        commands::create_vessel,
        commands::list_streams,
        commands::set_vessel_streams,
        commands::resync,
        commands::rebuild,
        commands::sync_status,
        commands::list_batches,
        commands::get_batch,
        commands::batch_series,
        commands::list_batch_events,
        commands::batch_stability,
        commands::batch_dashboard,
        commands::set_batch_og,
        commands::home_summary,
        commands::compare_batches,
        commands::run_batch_query,
        commands::run_saved_query,
        commands::list_saved_queries,
        commands::create_saved_query,
        commands::update_saved_query,
        commands::archive_saved_query,
        commands::query_options,
    ]
}

/// Start a polling task for every source already in the database.
///
/// One broken source must not stop the others: a row with no `sync_state` or an
/// unparseable `config` is logged and skipped, because the alternative is an app that
/// syncs nothing because of one bad paste six months ago.
fn start_polling(app: &tauri::AppHandle) {
    let db = app.state::<Db>();
    let client = app.state::<Client>();
    let manager = app.state::<SyncManager>();

    let sources = match discovery::source_summaries(&db) {
        Ok(sources) => sources,
        Err(err) => {
            tracing::error!(%err, "could not list the sources; nothing will be polled");
            return;
        }
    };

    let config = app.state::<ConfigStore>();
    for summary in &sources {
        match commands::build_source(&db, &client, &config, summary.id, summary.kind) {
            Ok(Some(source)) => manager.spawn(source),
            Ok(None) => {}
            Err(err) => tracing::error!(
                source_id = summary.id,
                name = %summary.name,
                %err,
                "this source cannot be polled; skipping it"
            ),
        }
    }
    tracing::info!(count = sources.len(), "sources scheduled");
}
