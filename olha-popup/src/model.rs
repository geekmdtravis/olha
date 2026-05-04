use std::time::Instant;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
}

/// Mirrors the daemon's `Notification` struct. Only the fields we render or act
/// on are typed; everything else is tolerated via `#[serde(default)]` or by
/// simply not appearing in the struct.
#[derive(Debug, Clone, Deserialize)]
pub struct Notification {
    #[serde(default)]
    pub row_id: Option<i64>,
    #[allow(dead_code)]
    pub dbus_id: u32,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub urgency: Urgency,
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone)]
pub struct PopupState {
    pub row_id: Option<i64>,
    pub urgency: Urgency,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    /// None = sticky (no auto-dismiss).
    pub expires_at: Option<Instant>,
    /// Pixel height chosen for this popup's layer-shell surface.
    /// Stacking math reads it to position popups below this one.
    pub height: u32,
}
