use zbus::interface;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use std::sync::Arc;
use serde_json;
use tracing;

use crate::DaemonState;
use crate::dbus::freedesktop::{NotificationsDaemon, NotificationsDaemonSignals};
use crate::notification::{NotificationStatus, Urgency};
use crate::db::queries::{self, NotificationFilter};

/// Control daemon for olha (org.olha.Daemon)
#[derive(Clone)]
pub struct ControlDaemon {
    /// Access to DB and rules engine
    pub state: Arc<DaemonState>,
    /// D-Bus connection, used to emit signals on the FDO interface
    pub connection: zbus::Connection,
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

    /// Count notifications with optional filter (JSON)
    /// Returns JSON with unread and total counts
    async fn count(&self, filter: String) -> Result<String, zbus::fdo::Error> {
        let base_filter = parse_filter(&filter);

        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let mut unread_filter = base_filter.clone();
        unread_filter.status = Some(NotificationStatus::Unread);
        let unread = queries::count_notifications(&conn, &unread_filter).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Query error: {}", e))
        })?;

        let total = queries::count_notifications(&conn, &base_filter).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Query error: {}", e))
        })?;

        let result = serde_json::json!({
            "unread": unread,
            "total": total,
        });

        Ok(result.to_string())
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

    /// Invoke an action on a notification.
    ///
    /// Looks up the notification by row id, emits `ActionInvoked` on the
    /// freedesktop Notifications interface so the originating app can run the
    /// handler, and flips the row to `read` (matches GUI-click behavior of
    /// other notification daemons).
    async fn invoke_action(&self, id: u64, action_key: String) -> Result<(), zbus::fdo::Error> {
        let conn = self.state.open_db().map_err(|e| {
            zbus::fdo::Error::Failed(format!("Database error: {}", e))
        })?;

        let notif = queries::get_notification(&conn, id as i64)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?
            .ok_or_else(|| {
                zbus::fdo::Error::Failed(format!("Notification {} not found", id))
            })?;

        let has_action = notif.actions.iter().any(|a| a.id == action_key);
        if !has_action {
            return Err(zbus::fdo::Error::Failed(format!(
                "Notification {} has no action '{}'",
                id, action_key
            )));
        }

        let iface_ref: InterfaceRef<NotificationsDaemon> = self
            .connection
            .object_server()
            .interface("/org/freedesktop/Notifications")
            .await
            .map_err(|e| {
                zbus::fdo::Error::Failed(format!(
                    "Failed to locate Notifications interface: {}",
                    e
                ))
            })?;

        NotificationsDaemonSignals::action_invoked(
            iface_ref.signal_emitter(),
            notif.dbus_id,
            &action_key,
        )
        .await
        .map_err(|e| {
            zbus::fdo::Error::Failed(format!("Failed to emit ActionInvoked: {}", e))
        })?;

        if let Some(row_id) = notif.row_id {
            if let Err(e) = queries::update_status(&conn, row_id, NotificationStatus::Read) {
                tracing::warn!(
                    "ActionInvoked emitted for row {} but failed to mark read: {}",
                    row_id,
                    e
                );
            }
        }

        tracing::debug!(
            "Invoked action '{}' on notification row {} (dbus_id {})",
            action_key,
            id,
            notif.dbus_id,
        );
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
    pub fn new(state: Arc<DaemonState>, connection: zbus::Connection) -> Self {
        Self { state, connection }
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

    tracing::debug!("parsed filter: app={:?}, urgency={:?}, status={:?}, category={:?}, search={:?}, since={:?}, until={:?}, limit={:?}",
        f.app_name, f.urgency, f.status, f.category, f.search, f.since, f.until, f.limit);

    f
}
