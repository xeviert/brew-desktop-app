//! `brewd.toml`. Loaded once at startup and validated before anything opens: a
//! daemon that starts with a bad config under systemd logs nothing useful until the
//! first message arrives, so every mistake here is fatal and immediate.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use brewcore::journal::TIME_COLUMN;
use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/brewd/brewd.toml";

const DEFAULT_BURST_WINDOW_MS: u64 = 2_000;
const BURST_WINDOW_MS: std::ops::RangeInclusive<u64> = 100..=60_000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub journal_dir: PathBuf,
    /// How long after a device's first message its burst stays open. See
    /// [`crate::burst`].
    #[serde(default = "default_burst_window_ms")]
    pub burst_window_ms: u64,
    #[serde(default)]
    pub http: Http,
    pub mqtt: Mqtt,
    #[serde(rename = "device", default)]
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    pub listen: SocketAddr,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8080)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mqtt {
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// One journal. Its columns come from any number of topics, in header order.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub name: String,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub topic: String,
    pub column: String,
}

fn default_burst_window_ms() -> u64 {
    DEFAULT_BURST_WINDOW_MS
}

fn default_mqtt_port() -> u16 {
    1883
}

fn default_client_id() -> String {
    "brewd".to_owned()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn burst_window(&self) -> Duration {
        Duration::from_millis(self.burst_window_ms)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |reason: String| Err(ConfigError::Invalid(reason));

        if !BURST_WINDOW_MS.contains(&self.burst_window_ms) {
            return invalid(format!(
                "burst_window_ms {} is outside {}..={}",
                self.burst_window_ms,
                BURST_WINDOW_MS.start(),
                BURST_WINDOW_MS.end()
            ));
        }
        if self.mqtt.host.trim().is_empty() {
            return invalid("mqtt.host is empty".to_owned());
        }
        if self.mqtt.password.is_some() && self.mqtt.username.is_none() {
            return invalid("mqtt.password is set without mqtt.username".to_owned());
        }
        if self.devices.is_empty() {
            return invalid("no [[device]] is configured".to_owned());
        }

        let mut names = HashSet::new();
        let mut topics = HashSet::new();
        for device in &self.devices {
            // No dots: a rotated journal is `<name>.<epoch>.csv`, and a device named
            // `esp32.1790000000` would collide with one.
            let name_ok = !device.name.is_empty()
                && device
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
            if !name_ok {
                return invalid(format!(
                    "device name '{}' must be [A-Za-z0-9_-] and non-empty",
                    device.name
                ));
            }
            if !names.insert(device.name.as_str()) {
                return invalid(format!("device '{}' is configured twice", device.name));
            }
            if device.columns.is_empty() {
                return invalid(format!("device '{}' has no columns", device.name));
            }

            let mut columns = HashSet::new();
            for Column { topic, column } in &device.columns {
                let column_ok = !column.is_empty()
                    && column
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_');
                if !column_ok {
                    return invalid(format!(
                        "device '{}': column '{column}' must be [A-Za-z0-9_] and non-empty",
                        device.name
                    ));
                }
                if column == TIME_COLUMN {
                    return invalid(format!(
                        "device '{}': '{TIME_COLUMN}' is brewd's own column",
                        device.name
                    ));
                }
                if !columns.insert(column.as_str()) {
                    return invalid(format!(
                        "device '{}': column '{column}' appears twice",
                        device.name
                    ));
                }
                // Topics are looked up exactly. A wildcard would subscribe fine and
                // then match nothing.
                if topic.is_empty() || topic.contains(['+', '#']) {
                    return invalid(format!(
                        "device '{}': topic '{topic}' must be a literal topic, not a filter",
                        device.name
                    ));
                }
                if !topics.insert(topic.as_str()) {
                    return invalid(format!("topic '{topic}' is mapped twice"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../deploy/brewd.example.toml");

    fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: PathBuf::new(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    fn minimal(devices: &str) -> String {
        format!("journal_dir = \"j\"\n[mqtt]\nhost = \"localhost\"\n{devices}")
    }

    #[test]
    fn the_shipped_example_is_valid() {
        let config = parse(EXAMPLE).unwrap();
        assert_eq!(config.devices.len(), 2);
        assert_eq!(config.devices[0].columns[0].column, "tilt");
    }

    #[test]
    fn defaults_fill_in_everything_optional() {
        let config = parse(&minimal(
            "[[device]]\nname = \"d\"\ncolumns = [{ topic = \"t\", column = \"c\" }]",
        ))
        .unwrap();
        assert_eq!(config.burst_window(), Duration::from_secs(2));
        assert_eq!(config.http.listen.port(), 8080);
        assert_eq!(config.mqtt.port, 1883);
        assert_eq!(config.mqtt.client_id, "brewd");
    }

    #[test]
    fn mistakes_are_rejected_with_the_offending_name() {
        let cases = [
            ("", "no [[device]]"),
            (
                "[[device]]\nname = \"a.b\"\ncolumns = [{ topic = \"t\", column = \"c\" }]",
                "a.b",
            ),
            ("[[device]]\nname = \"d\"\ncolumns = []", "no columns"),
            (
                "[[device]]\nname = \"d\"\ncolumns = [{ topic = \"t\", column = \"time\" }]",
                "brewd's own column",
            ),
            (
                "[[device]]\nname = \"d\"\ncolumns = [{ topic = \"a\", column = \"c\" }, { topic = \"b\", column = \"c\" }]",
                "appears twice",
            ),
            (
                "[[device]]\nname = \"d\"\ncolumns = [{ topic = \"ispindel/+/tilt\", column = \"c\" }]",
                "not a filter",
            ),
            (
                "[[device]]\nname = \"d\"\ncolumns = [{ topic = \"t\", column = \"c\" }]\n\
                 [[device]]\nname = \"e\"\ncolumns = [{ topic = \"t\", column = \"c\" }]",
                "mapped twice",
            ),
            (
                "[[device]]\nname = \"d\"\ncolumns = [{ topic = \"t\", column = \"c\" }]\n\
                 [[device]]\nname = \"d\"\ncolumns = [{ topic = \"u\", column = \"c\" }]",
                "configured twice",
            ),
        ];
        for (devices, expected) in cases {
            let err = parse(&minimal(devices)).unwrap_err().to_string();
            assert!(err.contains(expected), "{expected:?} not in {err:?}");
        }
    }

    #[test]
    fn an_unknown_key_is_an_error_not_a_silent_default() {
        let err = parse(&format!(
            "burst_window = 5\n{}",
            minimal("[[device]]\nname = \"d\"\ncolumns = [{ topic = \"t\", column = \"c\" }]")
        ))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }
}
