//! The shared handle: what `set` persists, what it refuses, and what clones see.

use brewcore::{
    config::{self, Config, ConfigStore, Fermentrack, SyncConfig},
    units::TempDisplay,
};

fn interval(secs: u64) -> Config {
    Config {
        sync: SyncConfig {
            poll_interval_secs: secs,
        },
        ..Config::default()
    }
}

#[test]
fn open_does_not_create_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");

    let store = ConfigStore::open(&path).unwrap();
    assert_eq!(store.get(), Config::default());
    assert!(!path.exists());
}

#[test]
fn set_persists_so_a_fresh_store_sees_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");

    let store = ConfigStore::open(&path).unwrap();
    store.set(interval(300)).unwrap();

    assert_eq!(ConfigStore::open(&path).unwrap().get(), interval(300));
}

#[test]
fn set_returns_the_normalised_value() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::open(tmp.path().join("config.toml")).unwrap();

    let stored = store
        .set(Config {
            fermentrack: Fermentrack {
                base_url: "  http://host/  ".to_owned(),
            },
            ..Config::default()
        })
        .unwrap();

    assert_eq!(stored.fermentrack.base_url, "http://host");
    assert_eq!(store.get(), stored);
}

#[test]
fn a_rejected_set_changes_neither_the_file_nor_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let store = ConfigStore::open(&path).unwrap();
    store.set(interval(300)).unwrap();

    let err = store.set(interval(0)).unwrap_err();
    assert_eq!(err.code(), "config.invalid");

    assert_eq!(store.get(), interval(300), "memory must be untouched");
    assert_eq!(
        config::load(&path).unwrap(),
        interval(300),
        "the file must be untouched"
    );
}

#[test]
fn clones_share_one_value() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::open(tmp.path().join("config.toml")).unwrap();
    let other = store.clone();

    store.set(interval(120)).unwrap();

    assert_eq!(other.get(), interval(120));
}

#[test]
fn a_store_opened_on_a_broken_file_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "[sync\n").unwrap();

    assert_eq!(ConfigStore::open(&path).unwrap_err().code(), "config.parse");

    let store = ConfigStore::open_or_default(&path);
    assert_eq!(store.get(), Config::default());

    // The bad file survives until the user deliberately saves over it.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[sync\n");

    let written = Config {
        ui: brewcore::config::Ui {
            temp_display: TempDisplay::F,
            ..Default::default()
        },
        ..Config::default()
    };
    store.set(written.clone()).unwrap();
    assert_eq!(config::load(&path).unwrap(), written);
}
