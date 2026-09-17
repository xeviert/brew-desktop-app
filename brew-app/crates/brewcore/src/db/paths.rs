use std::path::{Path, PathBuf};

/// Environment override for the database location. Used by tests, `xtask`, and
/// anyone wanting to point a dev build at a scratch file.
pub const DB_PATH_ENV: &str = "BREWAPP_DB";

/// Resolve the database path from `default_dir`, honouring [`DB_PATH_ENV`].
///
/// `brewcore` cannot ask Tauri where the app-data directory is, so the caller
/// supplies it. There is deliberately no fallback to the current directory: a
/// database that lands wherever the process happened to start is worse than an
/// error.
pub fn resolve_db_path(default_dir: &Path) -> PathBuf {
    match std::env::var_os(DB_PATH_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => default_dir.join("brewery.db"),
    }
}
