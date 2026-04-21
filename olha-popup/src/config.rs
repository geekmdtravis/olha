use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub enum Position {
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "top-right" => Ok(Position::TopRight),
            "top-left" => Ok(Position::TopLeft),
            "bottom-right" => Ok(Position::BottomRight),
            "bottom-left" => Ok(Position::BottomLeft),
            other => Err(serde::de::Error::custom(format!(
                "invalid popup.position {other:?} (expected top-right|top-left|bottom-right|bottom-left)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationsConfig {
    #[serde(default = "default_timeout")]
    pub default_timeout: u32,
    #[serde(default = "timeout_low")]
    pub timeout_low: u32,
    #[serde(default)]
    pub timeout_critical: u32,
}

fn default_timeout() -> u32 {
    10
}
fn timeout_low() -> u32 {
    5
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            default_timeout: default_timeout(),
            timeout_low: timeout_low(),
            timeout_critical: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PopupConfig {
    #[serde(default = "default_position")]
    pub position: Position,
    #[serde(default = "default_max_visible")]
    pub max_visible: usize,
    #[serde(default = "default_margin")]
    pub margin: u32,
    #[serde(default = "default_gap")]
    pub gap: u32,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
}

fn default_position() -> Position {
    Position::TopRight
}
fn default_max_visible() -> usize {
    5
}
fn default_margin() -> u32 {
    12
}
fn default_gap() -> u32 {
    8
}
fn default_width() -> u32 {
    380
}
fn default_height() -> u32 {
    120
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            position: default_position(),
            max_visible: default_max_visible(),
            margin: default_margin(),
            gap: default_gap(),
            width: default_width(),
            height: default_height(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub popup: PopupConfig,
}

impl AppConfig {
    pub fn load() -> Self {
        let path = config_path();
        let Some(path) = path else {
            tracing::info!("no XDG config dir; using defaults");
            return Self::default();
        };
        if !path.exists() {
            tracing::debug!("{} does not exist; using defaults", path.display());
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::error!("failed to parse {}: {e}; using defaults", path.display());
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!("failed to read {}: {e}; using defaults", path.display());
                Self::default()
            }
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("olha").join("config.toml"))
}
