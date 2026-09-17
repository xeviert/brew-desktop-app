//! `BREWAPP_DB` is process-global state, so these live in their own test binary
//! and run single-threaded. Sharing a binary with the schema tests would make them
//! order-dependent.
//!
//! Run as: `cargo test --test db_path -- --test-threads=1`

use brewcore::db::{DB_PATH_ENV, resolve_db_path};

#[test]
fn defaults_to_brewery_db_in_the_given_directory() {
    unsafe { std::env::remove_var(DB_PATH_ENV) };
    let dir = std::path::Path::new("/some/app/data");
    assert_eq!(resolve_db_path(dir), dir.join("brewery.db"));
}

#[test]
fn the_env_override_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().join("scratch.db");
    unsafe { std::env::set_var(DB_PATH_ENV, &override_path) };

    let resolved = resolve_db_path(std::path::Path::new("/some/app/data"));
    unsafe { std::env::remove_var(DB_PATH_ENV) };

    assert_eq!(resolved, override_path);
}
