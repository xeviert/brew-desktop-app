//! `BREWAPP_DB` is process-global state, so these live in their own test binary and
//! serialise on `ENV_LOCK`. Sharing a binary with the schema tests would make them
//! order-dependent; relying on `--test-threads=1` would only work when someone
//! remembers to pass it, and CI runs a bare `cargo test --workspace`.

use std::sync::Mutex;

use brewcore::db::{DB_PATH_ENV, resolve_db_path};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn defaults_to_brewery_db_in_the_given_directory() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe { std::env::remove_var(DB_PATH_ENV) };
    let dir = std::path::Path::new("/some/app/data");
    assert_eq!(resolve_db_path(dir), dir.join("brewery.db"));
}

#[test]
fn the_env_override_wins() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().join("scratch.db");
    unsafe { std::env::set_var(DB_PATH_ENV, &override_path) };

    let resolved = resolve_db_path(std::path::Path::new("/some/app/data"));
    unsafe { std::env::remove_var(DB_PATH_ENV) };

    assert_eq!(resolved, override_path);
}
