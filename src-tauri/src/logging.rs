//! Subscriber setup. `brewcore` emits `tracing` events and never configures a
//! subscriber — that is the binary's job, and keeping it that way is what lets
//! `cargo test -p brewcore` run silently.

use std::path::{Path, PathBuf};

use tracing_appender::{non_blocking::WorkerGuard, rolling};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// `RUST_LOG`-style filter, namespaced so it cannot collide with another tool's.
pub const LOG_ENV: &str = "BREWAPP_LOG";

/// Override for the log directory, mirroring `BREWAPP_DB` and `BREWAPP_CONFIG`.
pub const LOG_DIR_ENV: &str = "BREWAPP_LOG_DIR";

/// Keeps the non-blocking writer's worker thread alive.
///
/// `tracing_appender::non_blocking` returns a guard whose `Drop` flushes the buffer
/// and joins the worker. Binding it to a local in `setup()` drops it at the end of
/// that function and silently stops the file layer writing, with no error anywhere.
/// Handing it to `app.manage()` gives it exactly the app's lifetime and still lets it
/// flush at shutdown, which `std::mem::forget` would not.
pub struct Guard(#[allow(dead_code)] WorkerGuard);

pub fn resolve_log_dir(default_dir: &Path) -> PathBuf {
    match std::env::var_os(LOG_DIR_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => default_dir.to_path_buf(),
    }
}

pub fn init(log_dir: &Path) -> std::io::Result<Guard> {
    std::fs::create_dir_all(log_dir)?;

    let file = rolling::Builder::new()
        .rotation(rolling::Rotation::DAILY)
        .filename_prefix("brew-app")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
        .map_err(std::io::Error::other)?;
    let (writer, guard) = tracing_appender::non_blocking(file);

    // A release build sets `windows_subsystem = "windows"` and so has no console at
    // all, which is exactly why the file layer is not the optional one.
    //
    // `Option<L>` implements `Layer`, so this type-checks as `None` in release
    // without duplicating the whole chain under `#[cfg]`.
    let stderr = cfg!(debug_assertions).then(|| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_filter(filter())
    });

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(writer)
                .with_ansi(false) // a file is not a terminal
                .with_target(true)
                .with_filter(filter()),
        )
        .with(stderr)
        // A second init is a test artefact, not a reason to refuse to start.
        .try_init()
        .ok();

    install_panic_hook();
    Ok(Guard(guard))
}

/// `EnvFilter` is not `Clone`, so the two layers need two of them — building one and
/// passing it twice does not compile.
fn filter() -> EnvFilter {
    EnvFilter::try_from_env(LOG_ENV)
        .unwrap_or_else(|_| EnvFilter::new("brew_app_lib=debug,brewcore=debug,warn"))
}

/// A panic in a release build writes to a stderr that does not exist. Route it to the
/// log file first, then let the default hook run.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(
            panic = %info,
            backtrace = ?std::backtrace::Backtrace::force_capture(),
        );
        previous(info);
    }));
}
