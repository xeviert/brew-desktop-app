//! `brewd` — the Pi daemon. Subscribes to the broker, journals every reading per
//! device, and serves the journals over HTTP for the desktop app to sync. It runs
//! what must happen while the laptop is shut; see docs/BUILDOUT.md, Phase V.
//!
//! No SQLite here: `brewery.db` stays on the desktop, and `brewd` owns only its
//! journals and its config.
//!
//! Usage: `brewd [config path]`, default `/etc/brewd/brewd.toml`.

mod burst;
mod config;
mod http;
mod journal;
mod mqtt;

use std::path::PathBuf;
use std::process::ExitCode;

use brewcore::units::Timestamp;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use config::{Config, DEFAULT_CONFIG_PATH};
use journal::Journal;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let path = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from);

    match run(path).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// journald stamps every line and records the level itself, so no timestamps and no
/// colour. `RUST_LOG` overrides the default of `info`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .without_time()
        .init();
}

async fn run(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load(&path)?;
    std::fs::create_dir_all(&config.journal_dir)?;

    let now = Timestamp::now();
    let journals = config
        .devices
        .iter()
        .map(|device| {
            let columns: Vec<_> = device.columns.iter().map(|c| c.column.as_str()).collect();
            Journal::open(&config.journal_dir, &device.name, &columns, now)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ingest = mqtt::Ingest::new(&config.devices, journals, config.burst_window());

    let listener = tokio::net::TcpListener::bind(config.http.listen).await?;
    tracing::info!(
        config = %path.display(),
        listen = %config.http.listen,
        journals = %config.journal_dir.display(),
        devices = config.devices.len(),
        "brewd started"
    );

    let (stop, stopped) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutting down");
        let _ = stop.send(true);
    });

    let mut http_stopped = stopped.clone();
    let server = axum::serve(listener, http::router(config.journal_dir.clone()))
        .with_graceful_shutdown(async move {
            let _ = http_stopped.changed().await;
        });
    let (served, ()) = tokio::join!(server, mqtt::run(&config, ingest, stopped));
    served?;
    Ok(())
}

/// Ctrl-C anywhere; SIGTERM too on Unix, which is how systemd stops a service.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
