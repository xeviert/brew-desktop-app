//! User settings: a TOML file with working defaults, and the shared handle that
//! owns it.
//!
//! A missing file is not an error — it is the normal first-run state. Malformed
//! TOML *is* an error, because silently reverting to defaults hides a typo and then
//! lets the next save overwrite the file the user was trying to fix.

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    calc::{health::HealthParams, stability::StabilityParams},
    units::{GravityDisplay, TempDisplay},
};

/// Environment override for the config file location. Mirrors
/// [`crate::db::DB_PATH_ENV`]: it names the *file*, not the directory, so a test can
/// point at a tempdir without building an app-config layout around it.
pub const CONFIG_PATH_ENV: &str = "BREWAPP_CONFIG";

pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Phase 2g's default poll cadence. Inert until the scheduler exists.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;
pub const MIN_POLL_INTERVAL_SECS: u64 = 5;
pub const MAX_POLL_INTERVAL_SECS: u64 = 86_400;

/// Phase 3e. The two numbers a manual gravity entry needs and cannot guess: what
/// temperature the hydrometer was calibrated at, and what this refractometer reads on
/// wort. Both are properties of the user's kit, so they are settings rather than
/// constants — and both are passed into `db::measurement` as arguments, never read from
/// here by it, so a recorded measurement is reproducible from its own row.
pub const DEFAULT_HYDROMETER_CALIBRATION_C: f64 = 20.0;
pub const MIN_HYDROMETER_CALIBRATION_C: f64 = 0.0;
pub const MAX_HYDROMETER_CALIBRATION_C: f64 = 30.0;

pub const DEFAULT_REFRACTOMETER_WORT_CORRECTION: f64 = crate::calc::WORT_CORRECTION_DEFAULT;
pub const MIN_REFRACTOMETER_WORT_CORRECTION: f64 = 0.90;
pub const MAX_REFRACTOMETER_WORT_CORRECTION: f64 = 1.20;

/// Phase 4c. The finish detector's three numbers. Settings and not constants because
/// the first ten batches will say what they really are, and 4c's replay shows at once
/// what a change would have done to every past batch.
pub const DEFAULT_STABILITY_WINDOW_HOURS: f64 = 24.0;
pub const MIN_STABILITY_WINDOW_HOURS: f64 = 6.0;
pub const MAX_STABILITY_WINDOW_HOURS: f64 = 168.0;

pub const DEFAULT_STABILITY_SLOPE_SG_PER_DAY: f64 =
    crate::calc::stability::SLOPE_SG_PER_DAY_DEFAULT;
pub const MIN_STABILITY_SLOPE_SG_PER_DAY: f64 = 0.0001;
pub const MAX_STABILITY_SLOPE_SG_PER_DAY: f64 = 0.01;

pub const DEFAULT_STABILITY_FG_BAND: f64 = crate::calc::stability::FG_BAND_DEFAULT;
pub const MIN_STABILITY_FG_BAND: f64 = 0.0005;
pub const MAX_STABILITY_FG_BAND: f64 = 0.020;

/// Phase 4f. When a device's own battery and signal are worth a warning. Settings
/// because a Li-ion cutoff and a Wi-Fi floor are properties of the hardware.
pub const DEFAULT_HEALTH_BATTERY_WARN_V: f64 = crate::calc::health::BATTERY_WARN_V_DEFAULT;
pub const MIN_HEALTH_BATTERY_WARN_V: f64 = 3.0;
pub const MAX_HEALTH_BATTERY_WARN_V: f64 = 4.2;

pub const DEFAULT_HEALTH_RSSI_WARN_DBM: f64 = crate::calc::health::RSSI_WARN_DBM_DEFAULT;
pub const MIN_HEALTH_RSSI_WARN_DBM: f64 = -100.0;
pub const MAX_HEALTH_RSSI_WARN_DBM: f64 = -50.0;

/// Resolve the config path from `default_dir`, honouring [`CONFIG_PATH_ENV`].
///
/// As with [`crate::db::resolve_db_path`], the caller supplies the directory because
/// `brewcore` cannot ask Tauri for it, and there is no fallback to the current
/// directory.
pub fn resolve_config_path(default_dir: &Path) -> PathBuf {
    match std::env::var_os(CONFIG_PATH_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => default_dir.join(CONFIG_FILE_NAME),
    }
}

/// Every field here is a table, and it must stay that way: TOML requires scalars to
/// be written before tables, so a bare top-level scalar would serialise to an error
/// nobody sees until the first save. New settings go inside an existing table or get
/// a new one. `tests/config.rs` asserts this.
///
/// `#[serde(default)]` is repeated on every struct, not just this one. Without it on
/// the inner types, a file containing a bare `[sync]` fails to deserialise.
///
/// No `Eq`: [`Measurement`] holds `f64`s, which are not `Eq`. Nothing needs it —
/// every use is `assert_eq!`, `clone_from`, `Serialize` or `tauri::State`, and all of
/// those want `PartialEq` at most.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub fermentrack: Fermentrack,
    pub brewd: Brewd,
    pub sync: SyncConfig,
    pub ui: Ui,
    pub measurement: Measurement,
    pub gravity: Gravity,
    pub stability: Stability,
    pub health: Health,
    pub export: Export,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fermentrack {
    /// Empty until the user configures one, which is a state and not an error —
    /// Phase 1 never touches the network.
    ///
    /// Phase 2f supersedes rather than replaces this: each `source.config` carries
    /// its own URL, and this becomes the first-run default. Nothing may assume there
    /// is exactly one Fermentrack.
    pub base_url: String,
}

/// Phase V. The `brewd` daemon on the Pi, whose journals this app syncs over HTTP.
///
/// Only where to reach it. The MQTT broker is `brewd`'s business and its config lives
/// on the Pi: the desktop app never links an MQTT client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Brewd {
    /// Empty until the Pi is set up, which is a state and not an error.
    pub base_url: String,
}

// Named `SyncConfig` rather than `Sync`, which would shadow `std::marker::Sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// Inert until Phase 2g wires the scheduler.
    pub poll_interval_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub temp_display: TempDisplay,
    /// Inert in Phase 1d — nothing renders gravity until Phase 4's chart, which gets
    /// its conversions from `calc`, not from the frontend.
    pub gravity_display: GravityDisplay,
}

/// Phase 3e. The defaults the manual-entry form starts from.
///
/// Defaults and not silent corrections: 20 °C is the usual hydrometer calibration but
/// 60 °F instruments are common, and the wort correction factor is 1.0 for a sugar wash
/// — which is most of what this app ferments. `db::measurement` applies neither unless
/// it is passed one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Measurement {
    pub hydrometer_calibration_c: f64,
    pub refractometer_wort_correction: f64,
}

impl Default for Measurement {
    fn default() -> Self {
        Self {
            hydrometer_calibration_c: DEFAULT_HYDROMETER_CALIBRATION_C,
            refractometer_wort_correction: DEFAULT_REFRACTOMETER_WORT_CORRECTION,
        }
    }
}

/// Phase 4c. Passed to `calc::stability` via [`Stability::params`]; nothing there
/// reads the config.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stability {
    pub window_hours: f64,
    pub slope_sg_per_day: f64,
    pub fg_band: f64,
}

impl Default for Stability {
    fn default() -> Self {
        Self {
            window_hours: DEFAULT_STABILITY_WINDOW_HOURS,
            slope_sg_per_day: DEFAULT_STABILITY_SLOPE_SG_PER_DAY,
            fg_band: DEFAULT_STABILITY_FG_BAND,
        }
    }
}

impl Stability {
    pub fn params(&self) -> StabilityParams {
        StabilityParams {
            window_secs: (self.window_hours * 3_600.0).round() as i64,
            slope_sg_per_day: self.slope_sg_per_day,
            fg_band: self.fg_band,
        }
    }
}

/// Phase 4f. Passed to `calc::health` via [`Health::params`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Health {
    pub battery_warn_v: f64,
    pub rssi_warn_dbm: f64,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            battery_warn_v: DEFAULT_HEALTH_BATTERY_WARN_V,
            rssi_warn_dbm: DEFAULT_HEALTH_RSSI_WARN_DBM,
        }
    }
}

impl Health {
    pub fn params(&self) -> HealthParams {
        HealthParams {
            battery_warn_v: self.battery_warn_v,
            rssi_warn_dbm: self.rssi_warn_dbm,
        }
    }
}

/// Phase V. What the iSpindle's calibration polynomial outputs.
///
/// No payload says: the iSpindle publishes a bare `gravity` number, and whether it is
/// SG or °P is whatever polynomial was typed into its config page. So there is no
/// default — a wrong guess stores 12.0 as a specific gravity, or 1.048 as °P, and
/// both would pass for a reading at a glance. Unset until the user records it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gravity {
    pub ispindel_unit: Option<IspindelUnit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IspindelUnit {
    Sg,
    Plato,
}

/// Phase 5e. How a CSV export is written. Values are canonical either way; only the
/// punctuation changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Export {
    pub dialect: ExportDialect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportDialect {
    /// `,` separator, `.` decimal.
    #[default]
    Rfc4180,
    /// `;` separator, `,` decimal, UTF-8 BOM: French Excel opens a comma CSV as one
    /// column, and without the BOM reads UTF-8 as Windows-1252.
    ExcelFr,
}

/// `f64::clamp` propagates NaN, and TOML has `nan` and `inf` literals — so a
/// hand-edited file can hand us a value that survives clamping and then poisons every
/// reading it touches. A non-finite value is not out of range, it is absent, so it goes
/// back to the default rather than to a bound.
fn clamp_finite(value: f64, default: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

impl Config {
    /// Coerce into range, returning a note per adjustment.
    ///
    /// Used on load, where leniency is the point: a file hand-edited by someone who
    /// does not know the bounds must not stop the app from starting. Callers log the
    /// returned notes.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut notes = Vec::new();

        for (field, url) in [
            ("fermentrack.base_url", &mut self.fermentrack.base_url),
            ("brewd.base_url", &mut self.brewd.base_url),
        ] {
            let trimmed = normalize_base_url(url);
            if trimmed != *url {
                notes.push(format!("{field} normalised to '{trimmed}'"));
                *url = trimmed;
            }
        }

        let clamped = self
            .sync
            .poll_interval_secs
            .clamp(MIN_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS);
        if clamped != self.sync.poll_interval_secs {
            notes.push(format!(
                "sync.poll_interval_secs {} is out of range, using {clamped}",
                self.sync.poll_interval_secs
            ));
            self.sync.poll_interval_secs = clamped;
        }

        for (field, value, default, min, max) in [
            (
                "measurement.hydrometer_calibration_c",
                &mut self.measurement.hydrometer_calibration_c,
                DEFAULT_HYDROMETER_CALIBRATION_C,
                MIN_HYDROMETER_CALIBRATION_C,
                MAX_HYDROMETER_CALIBRATION_C,
            ),
            (
                "measurement.refractometer_wort_correction",
                &mut self.measurement.refractometer_wort_correction,
                DEFAULT_REFRACTOMETER_WORT_CORRECTION,
                MIN_REFRACTOMETER_WORT_CORRECTION,
                MAX_REFRACTOMETER_WORT_CORRECTION,
            ),
            (
                "stability.window_hours",
                &mut self.stability.window_hours,
                DEFAULT_STABILITY_WINDOW_HOURS,
                MIN_STABILITY_WINDOW_HOURS,
                MAX_STABILITY_WINDOW_HOURS,
            ),
            (
                "stability.slope_sg_per_day",
                &mut self.stability.slope_sg_per_day,
                DEFAULT_STABILITY_SLOPE_SG_PER_DAY,
                MIN_STABILITY_SLOPE_SG_PER_DAY,
                MAX_STABILITY_SLOPE_SG_PER_DAY,
            ),
            (
                "stability.fg_band",
                &mut self.stability.fg_band,
                DEFAULT_STABILITY_FG_BAND,
                MIN_STABILITY_FG_BAND,
                MAX_STABILITY_FG_BAND,
            ),
            (
                "health.battery_warn_v",
                &mut self.health.battery_warn_v,
                DEFAULT_HEALTH_BATTERY_WARN_V,
                MIN_HEALTH_BATTERY_WARN_V,
                MAX_HEALTH_BATTERY_WARN_V,
            ),
            (
                "health.rssi_warn_dbm",
                &mut self.health.rssi_warn_dbm,
                DEFAULT_HEALTH_RSSI_WARN_DBM,
                MIN_HEALTH_RSSI_WARN_DBM,
                MAX_HEALTH_RSSI_WARN_DBM,
            ),
        ] {
            let clamped = clamp_finite(*value, default, min, max);
            if clamped != *value {
                notes.push(format!("{field} {value} is out of range, using {clamped}"));
                *value = clamped;
            }
        }

        notes
    }

    /// Reject outright.
    ///
    /// Used on save, where the settings pane is a controlled surface and a refusal is
    /// actionable feedback rather than a mystery. The enum fields need no check —
    /// serde rejects anything that is not a known variant before this runs.
    pub fn validate(&self) -> Result<()> {
        if !(MIN_POLL_INTERVAL_SECS..=MAX_POLL_INTERVAL_SECS)
            .contains(&self.sync.poll_interval_secs)
        {
            return Err(Error::ConfigInvalid {
                field: "sync.poll_interval_secs",
                reason: format!(
                    "must be between {MIN_POLL_INTERVAL_SECS} and {MAX_POLL_INTERVAL_SECS} seconds"
                ),
            });
        }

        // Non-finite is checked first and by name: a NaN fails every range test, and
        // "must be between 0 and 30" is the wrong thing to tell someone who typed `nan`.
        for (field, value, range) in [
            (
                "measurement.hydrometer_calibration_c",
                self.measurement.hydrometer_calibration_c,
                MIN_HYDROMETER_CALIBRATION_C..=MAX_HYDROMETER_CALIBRATION_C,
            ),
            (
                "measurement.refractometer_wort_correction",
                self.measurement.refractometer_wort_correction,
                MIN_REFRACTOMETER_WORT_CORRECTION..=MAX_REFRACTOMETER_WORT_CORRECTION,
            ),
            (
                "stability.window_hours",
                self.stability.window_hours,
                MIN_STABILITY_WINDOW_HOURS..=MAX_STABILITY_WINDOW_HOURS,
            ),
            (
                "stability.slope_sg_per_day",
                self.stability.slope_sg_per_day,
                MIN_STABILITY_SLOPE_SG_PER_DAY..=MAX_STABILITY_SLOPE_SG_PER_DAY,
            ),
            (
                "stability.fg_band",
                self.stability.fg_band,
                MIN_STABILITY_FG_BAND..=MAX_STABILITY_FG_BAND,
            ),
            (
                "health.battery_warn_v",
                self.health.battery_warn_v,
                MIN_HEALTH_BATTERY_WARN_V..=MAX_HEALTH_BATTERY_WARN_V,
            ),
            (
                "health.rssi_warn_dbm",
                self.health.rssi_warn_dbm,
                MIN_HEALTH_RSSI_WARN_DBM..=MAX_HEALTH_RSSI_WARN_DBM,
            ),
        ] {
            if !value.is_finite() {
                return Err(Error::ConfigInvalid {
                    field,
                    reason: "must be a finite number".to_owned(),
                });
            }
            if !range.contains(&value) {
                return Err(Error::ConfigInvalid {
                    field,
                    reason: format!("must be between {} and {}", range.start(), range.end()),
                });
            }
        }

        validate_base_url("fermentrack.base_url", &self.fermentrack.base_url)?;
        validate_base_url("brewd.base_url", &self.brewd.base_url)?;

        Ok(())
    }
}

/// Empty means "not configured yet" and is always legal.
fn validate_base_url(field: &'static str, url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(());
    }
    let Some(host) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return Err(Error::ConfigInvalid {
            field,
            reason: "must start with http:// or https://".to_owned(),
        });
    };
    // A scheme with nothing after it is not a URL, and accepting one is how
    // `normalize` and `validate` end up disagreeing about the same string.
    // Trailing slashes are stripped here too, because `normalize` strips them
    // and "http:///" must not validate before it and fail after.
    if host.trim_end_matches('/').is_empty() {
        return Err(Error::ConfigInvalid {
            field,
            reason: "must include a host, e.g. http://fermentrack.local".to_owned(),
        });
    }
    if url.split_whitespace().count() != 1 {
        return Err(Error::ConfigInvalid {
            field,
            reason: "must not contain whitespace".to_owned(),
        });
    }
    // Phase 2c. The last gate, not a replacement for the three above: they
    // catch things `Url::parse` happily accepts (`ftp://host`), and this
    // catches things they miss (a port that does not fit in a u16). It is here
    // at all because the HTTP client parses the same string later, and a
    // base_url that saves but cannot be fetched from would have the settings
    // pane blaming no field.
    if let Err(err) = reqwest::Url::parse(url) {
        return Err(Error::ConfigInvalid {
            field,
            reason: err.to_string(),
        });
    }
    Ok(())
}

/// Trim surrounding whitespace and trailing slashes, but never the scheme's own.
///
/// A bare `trim_end_matches('/')` turns `http://` into `http:`, which
/// [`Config::validate`] rejects — so a value [`ConfigStore::set`] accepted (it
/// validates the input, then normalises) would be stored in a form that can never be
/// saved again, with the settings pane blaming the field the user cannot fix. Slicing
/// past `://` keeps the two in agreement for every input.
fn normalize_base_url(url: &str) -> String {
    let trimmed = url.trim();
    let host_start = trimmed.find("://").map_or(0, |i| i + "://".len());
    let (scheme, rest) = trimmed.split_at(host_start);
    format!("{scheme}{}", rest.trim_end_matches('/'))
}

/// Keys present in `text` that [`Config`] does not understand.
///
/// Computed by diffing the file against a re-serialisation of the `Config` parsed
/// from it, so there is no key list to keep in sync and it cannot go stale when a
/// field is added. An unknown *table* is reported by its own name and not recursed
/// into, so a typo'd `[fermetrack]` is one warning rather than one per key under it.
///
/// `deny_unknown_fields` would be the obvious alternative and is the wrong trade: it
/// turns a stale or newer file into a boot failure. But a silently ignored typo
/// (`poll_interval_sec`) is how someone spends an afternoon wondering why a setting
/// does nothing, so callers log these at WARN.
pub fn unknown_keys(text: &str, config: &Config) -> Vec<String> {
    let Ok(found) = toml::from_str::<toml::Table>(text) else {
        return Vec::new();
    };
    let Ok(toml::Value::Table(known)) = toml::Value::try_from(config) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    diff_tables(&found, &known, "", &mut out);
    out.sort();
    out
}

fn diff_tables(found: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in found {
        match (known.get(key), value) {
            (None, _) => out.push(format!("{prefix}{key}")),
            (Some(toml::Value::Table(known_sub)), toml::Value::Table(found_sub)) => {
                diff_tables(found_sub, known_sub, &format!("{prefix}{key}."), out);
            }
            _ => {}
        }
    }
}

/// Read `path`, falling back to defaults when it does not exist.
///
/// Out-of-range values are clamped rather than rejected (see [`Config::normalize`]),
/// but syntax errors are fatal to the read.
pub fn load(path: &Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "no config file; using defaults");
            return Ok(Config::default());
        }
        Err(source) => {
            return Err(Error::ConfigRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let mut config: Config = toml::from_str(&text).map_err(|source| Error::ConfigParse {
        path: path.to_path_buf(),
        source,
    })?;

    for key in unknown_keys(&text, &config) {
        tracing::warn!(path = %path.display(), key, "unknown config key ignored");
    }
    for note in config.normalize() {
        tracing::warn!(path = %path.display(), "{note}");
    }

    Ok(config)
}

/// Write `config` to `path` via a sibling temp file and a rename.
///
/// `sync_all` before the rename is what makes this survive a power cut rather than
/// only a panic. `with_file_name` (not `with_extension`) keeps the temp in the same
/// directory, which is what makes the rename atomic — across devices it degrades to
/// a copy.
///
/// `std::fs::rename` replaces an existing destination on Windows as well as unix, so
/// there is no unlink-first dance. It can still fail with "access is denied" if a
/// scanner or an editor holds the destination open; that surfaces as
/// [`Error::ConfigWrite`], which is correct.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    let text = toml::to_string_pretty(config)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::ConfigWrite {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(CONFIG_FILE_NAME);
    let tmp = path.with_file_name(format!("{file_name}.tmp"));

    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    };
    if let Err(source) = write() {
        let _ = std::fs::remove_file(&tmp);
        // The destination, not `tmp`: the temp file is an implementation detail and
        // this message is read by whoever clicked Save.
        return Err(Error::ConfigWrite {
            path: path.to_path_buf(),
            source,
        });
    }

    std::fs::rename(&tmp, path).map_err(|source| {
        let _ = std::fs::remove_file(&tmp);
        Error::ConfigWrite {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// The live config, shared.
///
/// Mirrors [`crate::db::Db`]: `Clone`, `Arc` inside, owns its own lock. The lock
/// lives here rather than in the Tauri shell for three reasons, in order of weight:
///
/// 1. Phase 2g's sync scheduler is a tokio task *in this crate* and needs
///    `poll_interval_secs` live. A `tauri::State` is unreachable from `brewcore`, so
///    the scheduler would have to hold a snapshot that goes stale the moment the
///    user changes it.
/// 2. Validate, persist, publish must be one critical section. With the lock in the
///    shell, someone eventually writes the file outside it and the file and the UI
///    start disagreeing. Here that is not expressible.
/// 3. `cargo test` can drive this against a tempdir. A `tauri::State<Mutex<Config>>`
///    cannot be tested without a window.
///
/// `RwLock` rather than `Mutex`: reads (every render, every sync tick) vastly
/// outnumber writes (a user clicking Save).
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    config: RwLock<Config>,
}

impl ConfigStore {
    /// Read `path` into a new store. Does not create the file: a config that does not
    /// exist yet is the normal first-run state, and writing defaults on launch would
    /// scribble in the user's config directory before they have expressed a single
    /// preference.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let config = load(&path)?;
        Ok(Self::with(path, config))
    }

    /// As [`ConfigStore::open`], but an unreadable file is logged and replaced in
    /// memory by defaults rather than returned as an error — launching with the wrong
    /// settings beats not launching.
    ///
    /// The bad file is left on disk untouched. [`ConfigStore::set`] is what overwrites
    /// it, and by then the user has made a deliberate choice.
    pub fn open_or_default(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        match load(&path) {
            Ok(config) => Self::with(path, config),
            Err(err) => {
                tracing::error!(path = %path.display(), %err, "config unreadable; using defaults");
                Self::with(path, Config::default())
            }
        }
    }

    fn with(path: PathBuf, config: Config) -> Self {
        Self {
            inner: Arc::new(Inner {
                path,
                config: RwLock::new(config),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// The current config, cloned. No guard escapes, so no caller can hold the lock
    /// across an `.await`; `Config` is a `String`, a `u64` and two unit enums.
    ///
    /// Poisoning is recovered from for the same reason as [`crate::db::Db::write`]:
    /// refusing every subsequent read because one thread panicked turns one bug into
    /// a dead app.
    pub fn get(&self) -> Config {
        self.inner
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Validate, persist, *then* publish — in that order. If the write fails, memory
    /// is unchanged, so the UI never shows a setting that vanishes on restart.
    ///
    /// Returns the stored value, which may differ from the argument (a trimmed URL, a
    /// clamped interval). Callers should adopt the return value.
    pub fn set(&self, mut config: Config) -> Result<Config> {
        config.validate()?;
        for note in config.normalize() {
            tracing::debug!("{note}");
        }

        save(&self.inner.path, &config)?;

        let mut guard = self
            .inner
            .config
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        guard.clone_from(&config);
        tracing::info!(path = %self.inner.path.display(), "config saved");

        Ok(config)
    }
}

impl std::fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigStore")
            .field("path", &self.inner.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_url(url: &str) -> Config {
        Config {
            fermentrack: Fermentrack {
                base_url: url.to_owned(),
            },
            ..Config::default()
        }
    }

    fn with_interval(secs: u64) -> Config {
        Config {
            sync: SyncConfig {
                poll_interval_secs: secs,
            },
            ..Config::default()
        }
    }

    #[test]
    fn normalize_leaves_a_clean_config_alone() {
        let mut config = with_url("http://fermentrack.local");
        assert!(config.normalize().is_empty());
        assert_eq!(config, with_url("http://fermentrack.local"));
    }

    #[test]
    fn normalize_trims_whitespace_and_a_trailing_slash() {
        let mut config = with_url("  http://fermentrack.local/  ");
        assert_eq!(config.normalize().len(), 1);
        assert_eq!(config.fermentrack.base_url, "http://fermentrack.local");
    }

    #[test]
    fn normalize_clamps_both_ends_of_the_interval() {
        let mut low = with_interval(0);
        assert_eq!(low.normalize().len(), 1);
        assert_eq!(low.sync.poll_interval_secs, MIN_POLL_INTERVAL_SECS);

        let mut high = with_interval(u64::MAX);
        assert_eq!(high.normalize().len(), 1);
        assert_eq!(high.sync.poll_interval_secs, MAX_POLL_INTERVAL_SECS);
    }

    #[test]
    fn normalize_keeps_the_schemes_own_slashes() {
        // `trim_end_matches('/')` on the whole string turns this into "http:", which
        // `validate` rejects — so `set` would store a value that can never be saved
        // again. Covered end to end by `config_store.rs`.
        let mut config = with_url("http://");
        assert!(config.normalize().is_empty());
        assert_eq!(config.fermentrack.base_url, "http://");
    }

    #[test]
    fn normalize_is_idempotent_and_agrees_with_validate() {
        for input in [
            "",
            "   ",
            "http://",
            "https://",
            "http:///",
            "http://host",
            "  http://host/  ",
            "http://host:8080//",
            "http://host/sub/path/",
            "not a url",
            // Phase 2c added `Url::parse` as a final gate. An IPv6 literal must still
            // pass it; a port that does not fit in a u16 must now fail, which the
            // hand-rolled checks let through.
            "http://[::1]:8080",
            "http://host:99999",
        ] {
            let mut once = with_url(input);
            once.normalize();
            let mut twice = once.clone();
            assert!(
                twice.normalize().is_empty(),
                "normalize is not idempotent for {input:?}"
            );
            assert_eq!(once, twice);

            // The contract `set` depends on: whatever normalize produces from a value
            // validate accepted must itself validate.
            if with_url(input).validate().is_ok() {
                assert!(
                    once.validate().is_ok(),
                    "normalize made {input:?} invalid: {:?}",
                    once.fermentrack.base_url
                );
            }
        }
    }

    #[test]
    fn the_measurement_defaults_are_a_twenty_degree_hydrometer_and_a_wort_factor_of_one_oh_four() {
        let config = Config::default();
        assert_eq!(config.measurement.hydrometer_calibration_c, 20.0);
        assert_eq!(config.measurement.refractometer_wort_correction, 1.04);
    }

    #[test]
    fn measurement_settings_outside_their_bands_are_clamped() {
        let mut config = Config::default();
        config.measurement.hydrometer_calibration_c = 500.0;
        config.measurement.refractometer_wort_correction = 0.1;

        assert_eq!(config.normalize().len(), 2);
        assert_eq!(
            config.measurement.hydrometer_calibration_c,
            MAX_HYDROMETER_CALIBRATION_C
        );
        assert_eq!(
            config.measurement.refractometer_wort_correction,
            MIN_REFRACTOMETER_WORT_CORRECTION
        );
    }

    /// TOML has `nan` and `inf` literals and `f64::clamp` propagates NaN, so a
    /// hand-edited file could otherwise hand a NaN straight through to a reading.
    #[test]
    fn a_non_finite_measurement_setting_goes_back_to_the_default_not_to_a_bound() {
        let mut config = Config::default();
        config.measurement.hydrometer_calibration_c = f64::NAN;
        config.measurement.refractometer_wort_correction = f64::INFINITY;

        assert!(config.validate().is_err());
        assert_eq!(config.normalize().len(), 2);
        assert_eq!(
            config.measurement.hydrometer_calibration_c,
            DEFAULT_HYDROMETER_CALIBRATION_C
        );
        assert_eq!(
            config.measurement.refractometer_wort_correction,
            DEFAULT_REFRACTOMETER_WORT_CORRECTION
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_names_the_measurement_field_it_rejected() {
        let mut config = Config::default();
        config.measurement.refractometer_wort_correction = 2.0;

        let err = config.validate().unwrap_err();
        assert_eq!(err.code(), "config.invalid");
        assert!(
            err.to_string()
                .contains("measurement.refractometer_wort_correction"),
            "{err}"
        );
    }

    #[test]
    fn the_stability_defaults_are_a_day_half_a_point_a_day_and_three_points() {
        let stability = Config::default().stability;
        assert_eq!(stability.window_hours, 24.0);
        assert_eq!(stability.slope_sg_per_day, 0.0005);
        assert_eq!(stability.fg_band, 0.003);
        assert_eq!(stability.params(), StabilityParams::default());
    }

    #[test]
    fn stability_settings_are_clamped_and_a_non_finite_one_goes_back_to_the_default() {
        let mut config = Config::default();
        config.stability.window_hours = 1.0;
        config.stability.slope_sg_per_day = 1.0;
        config.stability.fg_band = f64::NAN;

        assert_eq!(config.normalize().len(), 3);
        assert_eq!(config.stability.window_hours, MIN_STABILITY_WINDOW_HOURS);
        assert_eq!(
            config.stability.slope_sg_per_day,
            MAX_STABILITY_SLOPE_SG_PER_DAY
        );
        assert_eq!(config.stability.fg_band, DEFAULT_STABILITY_FG_BAND);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_names_the_stability_field_it_rejected() {
        for (stability, field) in [
            (
                Stability {
                    window_hours: 200.0,
                    ..Stability::default()
                },
                "stability.window_hours",
            ),
            (
                Stability {
                    slope_sg_per_day: f64::INFINITY,
                    ..Stability::default()
                },
                "stability.slope_sg_per_day",
            ),
            (
                Stability {
                    fg_band: 0.0,
                    ..Stability::default()
                },
                "stability.fg_band",
            ),
        ] {
            let err = Config {
                stability,
                ..Config::default()
            }
            .validate()
            .unwrap_err();
            assert_eq!(err.code(), "config.invalid");
            assert!(err.to_string().contains(field), "{err}");
        }
    }

    #[test]
    fn the_health_defaults_are_three_point_six_volts_and_minus_eighty_dbm() {
        let health = Config::default().health;
        assert_eq!(health.battery_warn_v, 3.6);
        assert_eq!(health.rssi_warn_dbm, -80.0);
        assert_eq!(health.params(), HealthParams::default());
    }

    #[test]
    fn health_settings_are_clamped_and_validated_by_name() {
        let mut config = Config::default();
        config.health.battery_warn_v = 5.0;
        config.health.rssi_warn_dbm = f64::NAN;
        assert_eq!(config.normalize().len(), 2);
        assert_eq!(config.health.battery_warn_v, MAX_HEALTH_BATTERY_WARN_V);
        assert_eq!(config.health.rssi_warn_dbm, DEFAULT_HEALTH_RSSI_WARN_DBM);
        assert!(config.validate().is_ok());

        config.health.rssi_warn_dbm = -20.0;
        let err = config.validate().unwrap_err();
        assert_eq!(err.code(), "config.invalid");
        assert!(err.to_string().contains("health.rssi_warn_dbm"), "{err}");
    }

    #[test]
    fn the_brewd_url_is_normalised_and_validated_like_fermentracks() {
        let mut config = Config {
            brewd: Brewd {
                base_url: "  http://brewd.local:8080/  ".to_owned(),
            },
            ..Config::default()
        };
        assert_eq!(config.normalize().len(), 1);
        assert_eq!(config.brewd.base_url, "http://brewd.local:8080");
        assert!(config.validate().is_ok());

        config.brewd.base_url = "brewd.local".to_owned();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("brewd.base_url"), "{err}");
    }

    #[test]
    fn the_ispindel_unit_is_unset_until_the_user_says() {
        assert_eq!(Config::default().gravity.ispindel_unit, None);
    }

    #[test]
    fn validate_accepts_an_empty_url() {
        assert!(with_url("").validate().is_ok());
    }

    #[test]
    fn validate_rejects_a_scheme_with_no_host() {
        for url in [
            "http://",
            "https://",
            "  http://  ",
            "http:///",
            "https:////",
        ] {
            assert_eq!(
                with_url(url).validate().unwrap_err().code(),
                "config.invalid",
                "{url:?} should not validate"
            );
        }
    }

    #[test]
    fn validate_rejects_a_url_without_a_scheme() {
        let err = with_url("fermentrack.local").validate().unwrap_err();
        assert_eq!(err.code(), "config.invalid");
    }

    #[test]
    fn validate_rejects_a_url_containing_whitespace() {
        let err = with_url("http://ferm entrack.local")
            .validate()
            .unwrap_err();
        assert_eq!(err.code(), "config.invalid");
    }

    /// Phase 2c's final gate. The hand-rolled checks see a scheme, a host and no
    /// whitespace and pass a port no socket can ever use; `fermentrack::client` then
    /// fails to parse the same string, with the settings pane blaming no field.
    #[test]
    fn validate_rejects_a_url_only_a_url_parser_can_fault() {
        let err = with_url("http://host:99999").validate().unwrap_err();
        assert_eq!(err.code(), "config.invalid");
        // And it must not have become stricter about legitimate hosts.
        assert!(with_url("http://[::1]:8080").validate().is_ok());
        assert!(with_url("http://fermentrack.local").validate().is_ok());
    }

    #[test]
    fn validate_accepts_both_interval_bounds_inclusively() {
        assert!(with_interval(MIN_POLL_INTERVAL_SECS).validate().is_ok());
        assert!(with_interval(MAX_POLL_INTERVAL_SECS).validate().is_ok());
    }

    #[test]
    fn validate_rejects_an_interval_outside_the_bounds() {
        assert_eq!(
            with_interval(MIN_POLL_INTERVAL_SECS - 1)
                .validate()
                .unwrap_err()
                .code(),
            "config.invalid"
        );
        assert_eq!(
            with_interval(MAX_POLL_INTERVAL_SECS + 1)
                .validate()
                .unwrap_err()
                .code(),
            "config.invalid"
        );
    }
}
