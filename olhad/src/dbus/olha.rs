use zbus::interface;
use zbus::object_server::SignalEmitter;
use std::sync::Arc;
use serde_json;

use crate::DaemonState;
use crate::notification::{NotificationStatus, Urgency};
use crate::db::queries::{self, NotificationFilter};

/// Control daemon for olha (org.olha.Daemon)
#[derive(Clone)]
pub struct ControlDaemon {
    /// Access to DB and rules engine
    pub state: Arc<DaemonState>,
}

#[interface(name = "org.olha.Daemon")]
impl ControlDaemon {
    /// List notifications with optional filter (JSON)
    /// Returns JSON array of notifications
    async fn list(&self, filter: String) -> Result<String, zbus::fdo::Error> {
        let notif_filter = parse_filter(&filter);

        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let notifications = queries::query_notifications(&conn, &notif_filter).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Query error: {}", e))
        })?;

        let json = serde_json::to_string(&notifications).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Serialization error: {}", e))
        })?;

        Ok(json)
    }

    /// Count notifications
    /// Returns (unread_count, total_count)
    async fn count(&self) -> Result<(u32, u32), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let unread_filter = NotificationFilter {
            status: Some(NotificationStatus::Unread),
            ..Default::default()
        };
        let unread = queries::count_notifications(&conn, &unread_filter).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Query error: {}", e))
        })? as u32;

        let total_filter = NotificationFilter::default();
        let total = queries::count_notifications(&conn, &total_filter).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Query error: {}", e))
        })? as u32;

        Ok((unread, total))
    }

    /// Mark notifications as read
    /// ids: array of notification row IDs
    async fn mark_read(&self, ids: Vec<u64>) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let row_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        queries::update_statuses(&conn, &row_ids, NotificationStatus::Read).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Update error: {}", e))
        })?;

        tracing::debug!("Marked {} notifications as read", ids.len());
        Ok(())
    }

    /// Clear (dismiss) notifications
    /// ids: array of notification row IDs
    async fn clear(&self, ids: Vec<u64>) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let row_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        queries::update_statuses(&conn, &row_ids, NotificationStatus::Cleared).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Update error: {}", e))
        })?;

        tracing::debug!("Cleared {} notifications", ids.len());
        Ok(())
    }

    /// Delete notifications permanently
    /// ids: array of notification row IDs
    async fn delete(&self, ids: Vec<u64>) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let row_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        queries::delete_notifications(&conn, &row_ids).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Delete error: {}", e))
        })?;

        tracing::debug!("Deleted {} notifications", ids.len());
        Ok(())
    }

    /// Clear all active notifications
    async fn clear_all(&self) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let from = &[NotificationStatus::Unread, NotificationStatus::Read];
        queries::update_all_status(&conn, from, NotificationStatus::Cleared).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Update error: {}", e))
        })?;

        tracing::debug!("Cleared all notifications");
        Ok(())
    }

    /// Mark all unread notifications as read
    async fn mark_read_all(&self) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let from = &[NotificationStatus::Unread];
        queries::update_all_status(&conn, from, NotificationStatus::Read).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Update error: {}", e))
        })?;

        tracing::debug!("Marked all notifications as read");
        Ok(())
    }

    /// Delete all notifications permanently
    async fn delete_all(&self) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        queries::delete_all(&conn).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Delete error: {}", e))
        })?;

        tracing::debug!("Deleted all notifications");
        Ok(())
    }

    /// Get a single notification as JSON
    async fn get_notification(&self, id: u64) -> Result<String, zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let notif = queries::get_notification(&conn, id as i64).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Query error: {}", e))
        })?;

        let json = serde_json::to_string(&notif).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Serialization error: {}", e))
        })?;

        Ok(json)
    }

    /// Invoke an action on a notification
    async fn invoke_action(&self, id: u64, action_key: String) -> Result<(), zbus::fdo::Error> {
        tracing::debug!("Invoking action {} on notification {}", action_key, id);
        // TODO: emit ActionInvoked signal on the freedesktop interface
        Ok(())
    }

    /// Signal emitted when a new notification is received and stored.
    /// The payload is the notification serialized as JSON.
    #[zbus(signal)]
    pub async fn notification_received(
        emitter: &SignalEmitter<'_>,
        notification: &str,
    ) -> zbus::Result<()>;

    /// Get daemon status as JSON
    async fn status(&self) -> Result<String, zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let unread_filter = NotificationFilter {
            status: Some(NotificationStatus::Unread),
            ..Default::default()
        };
        let unread = queries::count_notifications(&conn, &unread_filter).unwrap_or(0);

        let total_filter = NotificationFilter::default();
        let total = queries::count_notifications(&conn, &total_filter).unwrap_or(0);

        let status = serde_json::json!({
            "status": "running",
            "version": "0.1.0",
            "unread": unread,
            "total": total,
            "db_path": self.state.db_path.display().to_string(),
            "rules_count": self.state.config.rules.len(),
        });

        Ok(status.to_string())
    }
}

impl ControlDaemon {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

/// Parse a JSON filter string into a NotificationFilter
fn parse_filter(filter_json: &str) -> NotificationFilter {
    let mut f = NotificationFilter::default();

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(filter_json) {
        if let Some(app) = val.get("app").and_then(|v| v.as_str()) {
            f.app_name = Some(app.to_string());
        }

        if let Some(urgency) = val.get("urgency").and_then(|v| v.as_str()) {
            f.urgency = match urgency {
                "low" => Some(Urgency::Low),
                "normal" => Some(Urgency::Normal),
                "critical" => Some(Urgency::Critical),
                _ => None,
            };
        }

        if let Some(status) = val.get("status").and_then(|v| v.as_str()) {
            f.status = NotificationStatus::from_str(status);
        }

        if let Some(category) = val.get("category").and_then(|v| v.as_str()) {
            f.category = Some(category.to_string());
        }

        if let Some(search) = val.get("search").and_then(|v| v.as_str()) {
            f.search = Some(search.to_string());
        }

        if let Some(since) = val.get("since").and_then(|v| v.as_str()) {
            f.since = Some(since.to_string());
        }

        if let Some(until) = val.get("until").and_then(|v| v.as_str()) {
            f.until = Some(until.to_string());
        }

        if let Some(limit) = val.get("limit").and_then(|v| v.as_i64()) {
            f.limit = Some(limit);
        }
    }

    // Default limit if none specified
    if f.limit.is_none() {
        f.limit = Some(50);
    }

    f
}
