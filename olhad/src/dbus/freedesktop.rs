use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::interface;
use zbus::object_server::{InterfaceRef, SignalEmitter};

use crate::db::queries;
use crate::dbus::olha::{ControlDaemon, ControlDaemonSignals};
use crate::notification::{Action, Notification, NotificationStatus, Urgency};
use crate::rules::RuleAction;
use crate::DaemonState;

/// Whether DND, if active, should suppress a notification with the
/// given urgency. `allow_critical` comes from `[dnd]` config; when
/// true, critical notifications bypass DND.
fn dnd_suppresses(urgency: Urgency, allow_critical: bool) -> bool {
    !(allow_critical && urgency == Urgency::Critical)
}

/// Freedesktop notification daemon (org.freedesktop.Notifications)
#[derive(Clone)]
pub struct NotificationsDaemon {
    /// Shared state with the main daemon
    pub inner: Arc<RwLock<NotificationsDaemonInner>>,
    /// Access to DB and rules engine
    pub state: Arc<DaemonState>,
    /// D-Bus connection for emitting signals on the control interface
    pub connection: zbus::Connection,
}

pub struct NotificationsDaemonInner {
    /// Counter for assigning notification IDs
    next_id: u32,
    /// Track which D-Bus IDs are "active" (not yet closed)
    active_ids: HashMap<u32, bool>,
}

impl Clone for NotificationsDaemonInner {
    fn clone(&self) -> Self {
        Self {
            next_id: self.next_id,
            active_ids: self.active_ids.clone(),
        }
    }
}

impl NotificationsDaemonInner {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            active_ids: HashMap::new(),
        }
    }

    pub fn next_notification_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1; // skip 0
        }
        id
    }

    pub fn mark_closed(&mut self, id: u32) {
        self.active_ids.remove(&id);
    }

    pub fn mark_active(&mut self, id: u32) {
        self.active_ids.insert(id, true);
    }
}

/// Extract urgency from D-Bus hints (borrowed values)
fn extract_urgency(hints: &HashMap<String, zbus::zvariant::Value<'_>>) -> Urgency {
    if let Some(val) = hints.get("urgency") {
        if let Ok(u8_val) = <&u8>::try_from(val) {
            Urgency::from_u8(*u8_val)
        } else {
            Urgency::Normal
        }
    } else {
        Urgency::Normal
    }
}

/// Extract a string hint from D-Bus hints
fn extract_string_hint(hints: &HashMap<String, zbus::zvariant::Value<'_>>, key: &str) -> String {
    if let Some(val) = hints.get(key) {
        if let Ok(s) = <&str>::try_from(val) {
            s.to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

/// Convert D-Bus hints to owned JSON for storage
fn hints_to_json(hints: &HashMap<String, zbus::zvariant::Value<'_>>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (k, v) in hints {
        obj.insert(k.clone(), serde_json::Value::String(v.to_string()));
    }
    serde_json::Value::Object(obj)
}

/// Parse D-Bus actions array (alternating id, label pairs) into Action structs
fn parse_actions(actions: &[String]) -> Vec<Action> {
    actions
        .chunks(2)
        .filter_map(|chunk| {
            if chunk.len() == 2 {
                Some(Action {
                    id: chunk[0].clone(),
                    label: chunk[1].clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationsDaemon {
    /// Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)
    /// -> notification_id
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::Value<'_>>,
        expire_timeout: i32,
    ) -> Result<u32, zbus::fdo::Error> {
        let mut inner = self.inner.write().await;
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            inner.next_notification_id()
        };
        inner.mark_active(id);
        drop(inner); // release lock before DB operations

        // Extract fields from borrowed hints before they go out of scope
        let urgency = extract_urgency(&hints);
        let category = extract_string_hint(&hints, "category");
        let desktop_entry = {
            let de = extract_string_hint(&hints, "desktop-entry");
            if de.is_empty() {
                extract_string_hint(&hints, "desktop_entry")
            } else {
                de
            }
        };
        let hints_json = hints_to_json(&hints);
        let parsed_actions = parse_actions(&actions);

        let now = chrono::Utc::now();
        let notif = Notification {
            row_id: None,
            dbus_id: id,
            app_name: app_name.clone(),
            app_icon,
            summary: summary.clone(),
            body,
            urgency,
            category,
            desktop_entry,
            actions: parsed_actions,
            hints: hints_json,
            status: NotificationStatus::Unread,
            expire_timeout,
            created_at: now,
            updated_at: now,
            closed_reason: None,
        };

        // Run through rules engine
        let rule_result = self.state.rules_engine.evaluate(&notif);

        match rule_result.action {
            Some(RuleAction::Ignore) => {
                tracing::debug!(
                    "Notification ignored by rule '{}': app={}, summary={}",
                    rule_result.matching_rule.unwrap_or_default(),
                    app_name,
                    summary,
                );
                return Ok(id);
            }
            Some(RuleAction::Clear) => {
                // Store but auto-clear
                let mut notif = notif;
                notif.status = NotificationStatus::Cleared;
                tracing::debug!(
                    "Notification auto-cleared by rule '{}': app={}, summary={}",
                    rule_result.matching_rule.unwrap_or_default(),
                    app_name,
                    summary,
                );
                match self.store_notification(&notif) {
                    Ok(row_id) => {
                        notif.row_id = Some(row_id);
                        self.emit_notification_signal(&notif).await;
                    }
                    Err(e) => tracing::error!("Failed to store notification: {}", e),
                }
                return Ok(id);
            }
            Some(RuleAction::None) | None => {
                // Normal flow: store as unread. A `None` rule matched but
                // only carries on_action handlers, which fire at invoke time.
            }
        }

        tracing::debug!(
            "Notification received: app={}, id={}, summary={}",
            app_name,
            id,
            summary,
        );

        let mut notif = notif;
        match self.store_notification(&notif) {
            Ok(row_id) => {
                notif.row_id = Some(row_id);
                self.emit_notification_signal(&notif).await;
            }
            Err(e) => tracing::error!("Failed to store notification: {}", e),
        }

        Ok(id)
    }

    /// CloseNotification(id) -> ()
    async fn close_notification(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        id: u32,
    ) -> Result<(), zbus::fdo::Error> {
        let mut inner = self.inner.write().await;
        inner.mark_closed(id);
        drop(inner);

        // Update in DB: find notification by dbus_id and mark as cleared
        match self.state.open_db() {
            Ok(conn) => {
                match queries::get_notification_by_dbus_id(&conn, id, &self.state.enc_mode()) {
                    Ok(Some(notif)) => {
                        if let Some(row_id) = notif.row_id {
                            if let Err(e) =
                                queries::update_status(&conn, row_id, NotificationStatus::Cleared)
                            {
                                tracing::error!("Failed to update notification status: {}", e);
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::debug!("CloseNotification: dbus_id={} not found in DB", id);
                    }
                    Err(e) => {
                        tracing::error!("Failed to look up notification dbus_id={}: {}", id, e);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to open DB for CloseNotification: {}", e);
            }
        }

        if let Err(e) = Self::notification_closed(&emitter, id, 3).await {
            tracing::error!(
                "Failed to emit NotificationClosed(id={}, reason=3): {}",
                id,
                e
            );
        }

        tracing::debug!("Notification closed: id={}", id);
        Ok(())
    }

    /// GetCapabilities() -> capabilities
    async fn get_capabilities(&self) -> Result<Vec<String>, zbus::fdo::Error> {
        Ok(vec![
            "actions".to_string(),
            "body".to_string(),
            "body-markup".to_string(),
            "default-action".to_string(),
            "icon-static".to_string(),
            "persistence".to_string(),
            "sound".to_string(),
        ])
    }

    /// GetServerInformation() -> (name, vendor, version, spec_version)
    async fn get_server_information(
        &self,
    ) -> Result<(String, String, String, String), zbus::fdo::Error> {
        Ok((
            "olha".to_string(),
            "olha".to_string(),
            "0.1.0".to_string(),
            "1.2".to_string(),
        ))
    }

    /// Emitted when an action on a notification is invoked (by GUI click or
    /// by `olha invoke`). The originating application listens for this signal
    /// and runs the handler that corresponds to `action_key`.
    #[zbus(signal)]
    pub async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    /// Emitted when a notification is closed. `reason` follows the freedesktop
    /// Notifications spec: 1 = expired, 2 = dismissed by user, 3 = closed by
    /// call to CloseNotification, 4 = undefined/reserved.
    ///
    /// Senders (e.g. libnotify-based apps) commonly wait for this signal
    /// after `ActionInvoked` before considering the action complete.
    #[zbus(signal)]
    pub async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

impl NotificationsDaemon {
    pub fn new(state: Arc<DaemonState>, connection: zbus::Connection) -> Self {
        Self {
            inner: Arc::new(RwLock::new(NotificationsDaemonInner::new())),
            state,
            connection,
        }
    }

    /// Store a notification in the database. Writes are sealed
    /// against the public key whenever encryption is enabled — works
    /// even with the daemon locked (no sk required for writes).
    fn store_notification(&self, notif: &Notification) -> Result<i64, crate::db::DbError> {
        let conn = self.state.open_db()?;
        queries::insert_notification(&conn, notif, &self.state.enc_mode())
    }

    /// Emit a NotificationReceived signal on the org.olha.Daemon interface.
    ///
    /// Under DND, we swallow the signal so subscribers (olha-popup,
    /// `olha subscribe`) stay quiet, but storage has already happened
    /// at the call site — the notification is still in `olha list`.
    /// Critical urgency bypasses DND only when `[dnd].allow_critical` is on.
    async fn emit_notification_signal(&self, notif: &Notification) {
        if self.state.is_dnd()
            && dnd_suppresses(notif.urgency, self.state.config.dnd.allow_critical)
        {
            tracing::debug!(
                "DND active — suppressing notification_received for app={} urgency={:?}",
                notif.app_name,
                notif.urgency,
            );
            return;
        }

        let json = match serde_json::to_string(notif) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("Failed to serialize notification for signal: {}", e);
                return;
            }
        };

        let iface_ref: InterfaceRef<ControlDaemon> = match self
            .connection
            .object_server()
            .interface("/org/olha/Daemon")
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to get ControlDaemon interface for signal: {}", e);
                return;
            }
        };

        if let Err(e) =
            ControlDaemonSignals::notification_received(iface_ref.signal_emitter(), &json).await
        {
            tracing::error!("Failed to emit NotificationReceived signal: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_lets_critical_through_when_explicitly_allowed() {
        assert!(!dnd_suppresses(Urgency::Critical, true));
        assert!(dnd_suppresses(Urgency::Normal, true));
        assert!(dnd_suppresses(Urgency::Low, true));
    }

    #[test]
    fn dnd_silences_everything_when_allow_critical_off() {
        assert!(dnd_suppresses(Urgency::Critical, false));
        assert!(dnd_suppresses(Urgency::Normal, false));
        assert!(dnd_suppresses(Urgency::Low, false));
    }
}
