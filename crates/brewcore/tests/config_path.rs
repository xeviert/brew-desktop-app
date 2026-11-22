//! `BREWAPP_CONFIG` is process-global state. Same reasoning as `db_path.rs`: own
//! binary, and a lock so the two tests cannot interleave their env mutations.

use std::sync::Mutex;

use brewcore::config::{CONFIG_PATH_ENV, resolve_config_path};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn defaults_to_config_toml_in_the_given_directory() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe { std::env::remove_var(CONFIG_PATH_ENV) };
    let dir = std::path::Path::new("/some/app/config");
    assert_eq!(resolve_config_path(dir), dir.join("config.toml"));
}

#[test]
fn the_env_override_wins() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().join("scratch.toml");
    unsafe { std::env::set_var(CONFIG_PATH_ENV, &override_path) };

    let resolved = resolve_config_path(std::path::Path::new("/some/app/config"));
    unsafe { std::env::remove_var(CONFIG_PATH_ENV) };

    assert_eq!(resolved, override_path);
}
