use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// "clear", "ignore", or "none". "none" keeps the notification as-is —
    /// useful when the rule exists only to attach `on_action` handlers.
    pub action: String,
    /// Map of action key (e.g. "default", "reply") → shell command. When the
    /// user invokes the action in the popup, the command is spawned under
    /// `sh -c` with notification context exposed via `OLHA_*` env vars.
    #[serde(default)]
    pub on_action: Option<HashMap<String, String>>,
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
    pub encryption: EncryptionConfig,

    #[serde(default)]
    pub dnd: DndConfig,

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

/// At-rest encryption settings. When `enabled` is true, the daemon
/// loads the long-lived X25519 public key from the DB's `meta` table
/// and seals `summary`, `body`, and `hints` of every notification
/// before writing. The matching secret key lives in memory only
/// between `olha unlock` and `olha lock` / auto-lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "EncryptionConfig::default_pass_entry")]
    pub pass_entry: String,

    /// Idle timeout in seconds before the daemon auto-locks and
    /// zeroes the X25519 secret key. 0 disables auto-lock. Default
    /// 300 (5 minutes).
    #[serde(default = "EncryptionConfig::default_auto_lock_secs")]
    pub auto_lock_secs: u64,
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pass_entry: Self::default_pass_entry(),
            auto_lock_secs: Self::default_auto_lock_secs(),
        }
    }
}

impl EncryptionConfig {
    fn default_pass_entry() -> String {
        "olha/db-key".to_string()
    }

    fn default_auto_lock_secs() -> u64 {
        300
    }
}

/// Do Not Disturb. The `enabled` flag itself is runtime state (toggled
/// via `olha dnd on/off` and persisted to the `meta` table), so it
/// deliberately does *not* live here. This section is the static
/// policy applied while DND is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DndConfig {
    /// When true, notifications with `urgency = critical` still pop
    /// through while DND is on. Defaults to false so DND silences
    /// everything unless pass-through is explicitly enabled.
    #[serde(default = "DndConfig::default_allow_critical")]
    pub allow_critical: bool,
}

impl Default for DndConfig {
    fn default() -> Self {
        Self {
            allow_critical: Self::default_allow_critical(),
        }
    }
}

impl DndConfig {
    fn default_allow_critical() -> bool {
        false
    }
}

/// Template written to `~/.config/olha/config.toml` on first run. Contains
/// every option as a commented-out default so users discover knobs by
/// reading the file rather than the README.
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("config.template.toml");

impl Config {
    /// Load config from file, or use defaults. On first run (XDG default
    /// path missing) writes the template so the user has a file to edit.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let explicit = path.is_some();
        let path = if let Some(p) = path {
            p.to_path_buf()
        } else {
            default_config_path()
        };

        if !explicit && !path.exists() {
            match write_default_config(&path) {
                Ok(()) => tracing::info!("wrote default config to {}", path.display()),
                Err(e) => {
                    tracing::warn!("could not create default config at {}: {e}", path.display())
                }
            }
        }

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
            encryption: EncryptionConfig::default(),
            dnd: DndConfig::default(),
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

fn write_default_config(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, DEFAULT_CONFIG_TEMPLATE)
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

    #[test]
    fn dnd_does_not_allow_critical_by_default() {
        assert!(!DndConfig::default().allow_critical);
    }

    /// The bundled template is what first-run users get written to disk, so
    /// it must parse and produce the same values as `Config::default()`.
    /// Every setting is commented out, so the deserializer falls back to
    /// each field's serde default. A diff here means somebody added a knob
    /// to the template without a matching `Config` field (or vice versa).
    #[test]
    fn default_config_template_parses_to_defaults() {
        let parsed: Config = toml::from_str(DEFAULT_CONFIG_TEMPLATE)
            .expect("bundled config template must be valid TOML");
        let defaults = Config::default();
        assert_eq!(parsed.retention.max_age, defaults.retention.max_age);
        assert_eq!(parsed.retention.max_count, defaults.retention.max_count);
        assert_eq!(
            parsed.retention.cleanup_interval,
            defaults.retention.cleanup_interval
        );
        assert_eq!(
            parsed.notifications.default_timeout,
            defaults.notifications.default_timeout
        );
        assert_eq!(
            parsed.notifications.timeout_low,
            defaults.notifications.timeout_low
        );
        assert_eq!(
            parsed.notifications.timeout_critical,
            defaults.notifications.timeout_critical
        );
        assert_eq!(parsed.encryption.enabled, defaults.encryption.enabled);
        assert_eq!(parsed.encryption.pass_entry, defaults.encryption.pass_entry);
        assert_eq!(
            parsed.encryption.auto_lock_secs,
            defaults.encryption.auto_lock_secs
        );
        assert_eq!(parsed.dnd.allow_critical, defaults.dnd.allow_critical);
        assert!(parsed.rules.is_empty());
    }
}
