use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Notification urgency levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

impl Urgency {
    pub fn from_u8(val: u8) -> Self {
        match val {
            0 => Urgency::Low,
            1 => Urgency::Normal,
            2 => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }

    pub fn as_u32(&self) -> u32 {
        *self as u32
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::Critical => "critical",
        }
    }
}

/// Notification status/lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationStatus {
    /// Newly received, not yet seen
    Unread,
    /// Marked as read by user
    Read,
    /// Dismissed/cleared but kept in history
    Cleared,
}

impl NotificationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotificationStatus::Unread => "unread",
            NotificationStatus::Read => "read",
            NotificationStatus::Cleared => "cleared",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "unread" => Some(NotificationStatus::Unread),
            "read" => Some(NotificationStatus::Read),
            "cleared" => Some(NotificationStatus::Cleared),
            _ => None,
        }
    }
}

/// Reason a notification was closed
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedReason {
    Expired = 1,
    Dismissed = 2,
    ClosedByCall = 3,
    Undefined = 4,
}

/// Action button on a notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
}

/// A single notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Internal database row ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_id: Option<i64>,

    /// D-Bus notification ID (uint32)
    pub dbus_id: u32,

    /// Application name
    pub app_name: String,

    /// Application icon (icon name or path)
    pub app_icon: String,

    /// Notification title/summary
    pub summary: String,

    /// Notification body text
    pub body: String,

    /// Urgency level
    pub urgency: Urgency,

    /// Category (e.g., "im.received", "network.connected")
    pub category: String,

    /// Desktop entry (e.g., "org.mozilla.firefox")
    pub desktop_entry: String,

    /// Action buttons
    pub actions: Vec<Action>,

    /// Raw D-Bus hints as JSON
    pub hints: serde_json::Value,

    /// Current status
    pub status: NotificationStatus,

    /// Requested expiration timeout in milliseconds
    pub expire_timeout: i32,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,

    /// Reason closed (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_reason: Option<ClosedReason>,
}

impl Notification {
    /// Create a new notification from D-Bus parameters
    pub fn from_dbus(
        dbus_id: u32,
        app_name: String,
        _replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<(String, String)>,
        hints: HashMap<String, zbus::zvariant::Value>,
        expire_timeout: i32,
    ) -> Self {
        let now = Utc::now();

        // Convert actions
        let actions = actions
            .into_iter()
            .map(|(id, label)| Action { id, label })
            .collect();

        // Extract hints
        let (urgency, category, desktop_entry, hints_json) = extract_hints(&hints);

        Self {
            row_id: None,
            dbus_id,
            app_name,
            app_icon,
            summary,
            body,
            urgency,
            category,
            desktop_entry,
            actions,
            hints: hints_json,
            status: NotificationStatus::Unread,
            expire_timeout,
            created_at: now,
            updated_at: now,
            closed_reason: None,
        }
    }
}

/// Extract relevant hint fields from D-Bus hints dictionary
fn extract_hints(
    hints: &HashMap<String, zbus::zvariant::Value>,
) -> (Urgency, String, String, serde_json::Value) {
    let mut hints_obj = serde_json::json!({});

    // Extract urgency
    let urgency = if let Some(val) = hints.get("urgency") {
        // Try to get byte value
        if let Ok(u8_val) = <&u8>::try_from(val) {
            Urgency::from_u8(*u8_val)
        } else {
            Urgency::Normal
        }
    } else {
        Urgency::Normal
    };

    // Extract category
    let category = if let Some(val) = hints.get("category") {
        if let Ok(s) = <&str>::try_from(val) {
            s.to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Extract desktop entry
    let desktop_entry = if let Some(val) = hints.get("desktop-entry") {
        if let Ok(s) = <&str>::try_from(val) {
            s.to_string()
        } else {
            String::new()
        }
    } else if let Some(val) = hints.get("desktop_entry") {
        if let Ok(s) = <&str>::try_from(val) {
            s.to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Store all hints as JSON for later reference
    for (k, v) in hints {
        hints_obj[k] = serde_json::json!(v.to_string());
    }

    (urgency, category, desktop_entry, hints_obj)
}
