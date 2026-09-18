//! `load` / `save` against real files. No environment mutation, so these run in
//! parallel; the `BREWAPP_CONFIG` tests live in `config_path.rs`.

use brewcore::{
    config::{
        self, Config, Fermentrack, MAX_POLL_INTERVAL_SECS, MIN_POLL_INTERVAL_SECS, SyncConfig, Ui,
    },
    units::{GravityDisplay, TempDisplay},
};

fn write(dir: &std::path::Path, text: &str) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

fn entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_missing_file_yields_defaults_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");

    assert_eq!(config::load(&path).unwrap(), Config::default());
    assert!(entries(tmp.path()).is_empty(), "load must not write");
}

#[test]
fn the_documented_defaults_are_the_actual_defaults() {
    let config = Config::default();
    assert_eq!(config.fermentrack.base_url, "");
    assert_eq!(config.sync.poll_interval_secs, 60);
    assert_eq!(config.ui.temp_display, TempDisplay::C);
    assert_eq!(config.ui.gravity_display, GravityDisplay::Sg);
}

#[test]
fn an_empty_file_is_all_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "");
    assert_eq!(config::load(&path).unwrap(), Config::default());
}

#[test]
fn a_partial_file_fills_in_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "[sync]\npoll_interval_secs = 120\n");

    let config = config::load(&path).unwrap();
    assert_eq!(config.sync.poll_interval_secs, 120);
    assert_eq!(config.fermentrack.base_url, "");
    assert_eq!(config.ui.temp_display, TempDisplay::C);
}

#[test]
fn an_empty_table_is_all_defaults() {
    // Catches `#[serde(default)]` missing from an inner struct, which is the failure
    // a root-only attribute produces.
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "[sync]\n[ui]\n[fermentrack]\n");
    assert_eq!(config::load(&path).unwrap(), Config::default());
}

#[test]
fn a_malformed_file_is_an_error_not_a_silent_default() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "[sync\npoll_interval_secs = 30\n");
    let before = std::fs::read(&path).unwrap();

    let err = config::load(&path).unwrap_err();
    assert_eq!(err.code(), "config.parse");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a failed load must not touch the file"
    );
}

#[test]
fn unknown_keys_are_reported_but_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let text = "[nope]\nx = 1\n\n[sync]\npoll = 1\npoll_interval_secs = 30\n";
    let path = write(tmp.path(), text);

    let config = config::load(&path).unwrap();
    assert_eq!(config.sync.poll_interval_secs, 30);
    // An unknown table is named once, not recursed into: `nope`, not `nope.x`.
    assert_eq!(config::unknown_keys(text, &config), ["nope", "sync.poll"]);
}

#[test]
fn save_then_load_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let written = Config {
        fermentrack: Fermentrack {
            base_url: "http://fermentrack.local".to_owned(),
        },
        sync: SyncConfig {
            poll_interval_secs: 300,
        },
        ui: Ui {
            temp_display: TempDisplay::F,
            gravity_display: GravityDisplay::Plato,
        },
    };

    config::save(&path, &written).unwrap();
    assert_eq!(config::load(&path).unwrap(), written);
}

#[test]
fn every_display_variant_round_trips() {
    // Guards the serde renames, which are UPPERCASE on one enum and lowercase on the
    // other and will eventually be typed the wrong way round.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");

    for temp_display in [TempDisplay::C, TempDisplay::F] {
        for gravity_display in [
            GravityDisplay::Sg,
            GravityDisplay::Plato,
            GravityDisplay::Brix,
        ] {
            let written = Config {
                ui: Ui {
                    temp_display,
                    gravity_display,
                },
                ..Config::default()
            };
            config::save(&path, &written).unwrap();
            assert_eq!(config::load(&path).unwrap(), written);
        }
    }
}

#[test]
fn save_leaves_no_temp_file_behind() {
    let tmp = tempfile::tempdir().unwrap();
    config::save(&tmp.path().join("config.toml"), &Config::default()).unwrap();
    assert_eq!(entries(tmp.path()), ["config.toml"]);
}

#[test]
fn save_creates_the_parent_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nested").join("deeper").join("config.toml");

    config::save(&path, &Config::default()).unwrap();
    assert!(path.is_file());
}

#[test]
fn save_replaces_an_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "[sync]\npoll_interval_secs = 30\n");

    let replacement = Config {
        sync: SyncConfig {
            poll_interval_secs: 90,
        },
        ..Config::default()
    };
    config::save(&path, &replacement).unwrap();

    assert_eq!(config::load(&path).unwrap(), replacement);
    assert_eq!(entries(tmp.path()), ["config.toml"]);
}

#[test]
fn config_is_all_tables_so_toml_cannot_reorder_it() {
    // TOML writes scalars before tables. A bare top-level scalar on `Config` would
    // serialise to `ValueAfterTable`, which nobody discovers until the first save.
    let text = toml::to_string_pretty(&Config::default()).unwrap();
    assert_eq!(toml::from_str::<Config>(&text).unwrap(), Config::default());
}

#[test]
fn an_out_of_range_interval_is_clamped_on_load() {
    let tmp = tempfile::tempdir().unwrap();

    let low = write(tmp.path(), "[sync]\npoll_interval_secs = 0\n");
    assert_eq!(
        config::load(&low).unwrap().sync.poll_interval_secs,
        MIN_POLL_INTERVAL_SECS
    );

    let high = write(tmp.path(), "[sync]\npoll_interval_secs = 999999999\n");
    assert_eq!(
        config::load(&high).unwrap().sync.poll_interval_secs,
        MAX_POLL_INTERVAL_SECS
    );
}
