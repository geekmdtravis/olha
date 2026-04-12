use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
    #[error("Invalid config: {0}")]
    Invalid(String),
}

/// Notification matching rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRule {
    pub name: String,
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub urgency: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// "clear", "ignore", or "exec:command"
    pub action: String,
}

/// Main configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,

    #[serde(default)]
    pub retention: RetentionConfig,

    #[serde(default)]
    pub notifications: NotificationConfig,

    #[serde(default)]
    pub rules: Vec<NotificationRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default)]
    pub db_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Maximum age as duration string, e.g. "30d", "7d", "90d"
    #[serde(default = "RetentionConfig::default_max_age")]
    pub max_age: String,

    /// Maximum number of notifications to keep
    #[serde(default = "RetentionConfig::default_max_count")]
    pub max_count: i64,

    /// How often to run cleanup, e.g. "1h", "30m"
    #[serde(default = "RetentionConfig::default_cleanup_interval")]
    pub cleanup_interval: String,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            max_age: Self::default_max_age(),
            max_count: Self::default_max_count(),
            cleanup_interval: Self::default_cleanup_interval(),
        }
    }
}

impl RetentionConfig {
    fn default_max_age() -> String {
        "30d".to_string()
    }

    fn default_max_count() -> i64 {
        10000
    }

    fn default_cleanup_interval() -> String {
        "1h".to_string()
    }

    /// Parse duration string like "30d", "7d", "90d", "1h" to seconds
    pub fn max_age_secs(&self) -> u64 {
        parse_duration(&self.max_age).unwrap_or(30 * 24 * 3600) // default 30d
    }

    /// Parse cleanup interval to seconds
    pub fn cleanup_interval_secs(&self) -> u64 {
        parse_duration(&self.cleanup_interval).unwrap_or(3600) // default 1h
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    /// Default timeout in seconds
    #[serde(default = "NotificationConfig::default_timeout")]
    pub default_timeout: i32,

    /// Low urgency timeout in seconds
    #[serde(default = "NotificationConfig::default_timeout_low")]
    pub timeout_low: i32,

    /// Critical urgency timeout (0 = never) in seconds
    #[serde(default = "NotificationConfig::default_timeout_critical")]
    pub timeout_critical: i32,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            default_timeout: Self::default_timeout(),
            timeout_low: Self::default_timeout_low(),
            timeout_critical: Self::default_timeout_critical(),
        }
    }
}

impl NotificationConfig {
    fn default_timeout() -> i32 {
        10
    }

    fn default_timeout_low() -> i32 {
        5
    }

    fn default_timeout_critical() -> i32 {
        0
    }
}

impl Config {
    /// Load config from file, or use defaults
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = if let Some(p) = path {
            p.to_path_buf()
        } else {
            default_config_path()
        };

        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            toml::from_str(&content).map_err(ConfigError::TomlParse)
        } else {
            Ok(Self::default())
        }
    }

    /// Get the database path
    pub fn db_path(&self) -> PathBuf {
        if let Some(ref path) = self.general.db_path {
            PathBuf::from(shellexpand::tilde(path).as_ref())
        } else {
            let data_home = dirs::data_local_dir().expect("Could not determine XDG data dir");
            data_home.join("olha").join("notifications.db")
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            retention: RetentionConfig::default(),
            notifications: NotificationConfig::default(),
            rules: Vec::new(),
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self { db_path: None }
    }
}

/// Get the default config file path: $XDG_CONFIG_HOME/olha/config.toml
fn default_config_path() -> PathBuf {
    let config_home = dirs::config_dir().expect("Could not determine XDG config dir");
    config_home.join("olha").join("config.toml")
}

/// Parse a duration string like "30d", "7d", "1h", "30m"
fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let (num_str, unit) = if let Some(pos) = s.len().checked_sub(1) {
        (&s[..pos], &s[pos..])
    } else {
        return None;
    };

    let num: u64 = num_str.parse().ok()?;

    let secs = match unit {
        "d" => num * 24 * 3600,
        "h" => num * 3600,
        "m" => num * 60,
        "s" => num,
        _ => return None,
    };

    Some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("30d"), Some(30 * 24 * 3600));
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("60s"), Some(60));
    }
}
