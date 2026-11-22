//! `brewd` — the Pi daemon. Subscribes to the broker, journals every reading per
//! device, and serves the journals over HTTP for the desktop app to sync. It runs
//! what must happen while the laptop is shut; see docs/BUILDOUT.md, Phases V and 6:
//! since Phase 6 that includes judging every reading against the alert rules and
//! pushing what fires to a phone.
//!
//! No SQLite here: `brewery.db` stays on the desktop, and `brewd` owns only its
//! journals, its config, and the state files of [`alerts`].
//!
//! Usage: `brewd [config path]`, default `/etc/brewd/brewd.toml`.

mod alerts;
mod burst;
mod config;
mod http;
mod journal;
mod mqtt;
mod notify;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use brewcore::units::Timestamp;
use tokio::sync::{Notify, watch};
use tracing_subscriber::EnvFilter;

use alerts::Alerts;
use config::{Config, DEFAULT_CONFIG_PATH};
use journal::Journal;
use notify::Ntfy;

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
    let alerts = Arc::new(Mutex::new(Alerts::open(
        &config.state_dir,
        &config.journal_dir,
        &config.devices,
        now,
    )?));
    let wake = Arc::new(Notify::new());
    let ntfy = config.notify.as_ref().map(Ntfy::new).transpose()?;
    let ingest = mqtt::Ingest::new(&config.devices, journals, config.burst_window())
        .with_alerts(alerts.clone(), wake.clone());

    let listener = tokio::net::TcpListener::bind(config.http.listen).await?;
    tracing::info!(
        config = %path.display(),
        listen = %config.http.listen,
        journals = %config.journal_dir.display(),
        devices = config.devices.len(),
        state = %config.state_dir.display(),
        notify = config.notify.is_some(),
        writes = config.http.token.is_some(),
        "brewd started"
    );

    let (stop, stopped) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutting down");
        let _ = stop.send(true);
    });

    let mut http_stopped = stopped.clone();
    let app = http::AppState {
        journal_dir: Arc::new(config.journal_dir.clone()),
        alert_log: Arc::new(config.state_dir.join(format!(
            "{}.{}",
            alerts::LOG_NAME,
            brewcore::journal::JOURNAL_EXTENSION
        ))),
        alerts: alerts.clone(),
        token: config.http.token.as_deref().map(Arc::from),
        ntfy: ntfy.clone(),
        wake: wake.clone(),
    };
    let server = axum::serve(listener, http::router(app)).with_graceful_shutdown(async move {
        let _ = http_stopped.changed().await;
    });
    let (served, (), ()) = tokio::join!(
        server,
        mqtt::run(&config, ingest, stopped.clone()),
        alerts::run(alerts, ntfy, wake, stopped),
    );
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
