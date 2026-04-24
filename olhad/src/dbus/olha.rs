use serde_json;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing;
use zbus::interface;
use zbus::object_server::{InterfaceRef, SignalEmitter};

use crate::db::queries::{self, NotificationFilter};
use crate::dbus::freedesktop::{NotificationsDaemon, NotificationsDaemonSignals};
use crate::launcher;
use crate::notification::{Notification, NotificationStatus, Urgency};
use crate::DaemonState;

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

        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let notifications =
            queries::query_notifications(&conn, &notif_filter, &self.state.enc_mode())
                .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?;

        let json = serde_json::to_string(&notifications)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Serialization error: {}", e)))?;

        Ok(json)
    }

    /// Count notifications with optional filter (JSON)
    /// Returns JSON with unread and total counts
    async fn count(&self, filter: String) -> Result<String, zbus::fdo::Error> {
        let base_filter = parse_filter(&filter);

        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let mut unread_filter = base_filter.clone();
        unread_filter.status = Some(NotificationStatus::Unread);
        let unread = queries::count_notifications(&conn, &unread_filter, &self.state.enc_mode())
            .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?;

        let total = queries::count_notifications(&conn, &base_filter, &self.state.enc_mode())
            .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?;

        let result = serde_json::json!({
            "unread": unread,
            "total": total,
        });

        Ok(result.to_string())
    }

    /// Mark notifications as read
    /// ids: array of notification row IDs
    async fn mark_read(&self, ids: Vec<u64>) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let row_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        queries::update_statuses(&conn, &row_ids, NotificationStatus::Read)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Update error: {}", e)))?;

        tracing::debug!("Marked {} notifications as read", ids.len());
        Ok(())
    }

    /// Clear (dismiss) notifications
    /// ids: array of notification row IDs
    async fn clear(&self, ids: Vec<u64>) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let row_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        queries::update_statuses(&conn, &row_ids, NotificationStatus::Cleared)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Update error: {}", e)))?;

        tracing::debug!("Cleared {} notifications", ids.len());
        Ok(())
    }

    /// Delete notifications permanently
    /// ids: array of notification row IDs
    async fn delete(&self, ids: Vec<u64>) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let row_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        queries::delete_notifications(&conn, &row_ids)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Delete error: {}", e)))?;

        tracing::debug!("Deleted {} notifications", ids.len());
        Ok(())
    }

    /// Clear all active notifications
    async fn clear_all(&self) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let from = &[NotificationStatus::Unread, NotificationStatus::Read];
        queries::update_all_status(&conn, from, NotificationStatus::Cleared)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Update error: {}", e)))?;

        tracing::debug!("Cleared all notifications");
        Ok(())
    }

    /// Mark all unread notifications as read
    async fn mark_read_all(&self) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let from = &[NotificationStatus::Unread];
        queries::update_all_status(&conn, from, NotificationStatus::Read)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Update error: {}", e)))?;

        tracing::debug!("Marked all notifications as read");
        Ok(())
    }

    /// Delete all notifications permanently
    async fn delete_all(&self) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        queries::delete_all(&conn)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Delete error: {}", e)))?;

        tracing::debug!("Deleted all notifications");
        Ok(())
    }

    /// Get a single notification as JSON
    async fn get_notification(&self, id: u64) -> Result<String, zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let notif = queries::get_notification(&conn, id as i64, &self.state.enc_mode())
            .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?;

        let json = serde_json::to_string(&notif)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Serialization error: {}", e)))?;

        Ok(json)
    }

    /// Invoke an action on a notification.
    ///
    /// Looks up the notification by row id, emits `ActionInvoked` on the
    /// freedesktop Notifications interface so the originating app can run the
    /// handler, and flips the row to `read` (matches GUI-click behavior of
    /// other notification daemons).
    async fn invoke_action(&self, id: u64, action_key: String) -> Result<(), zbus::fdo::Error> {
        tracing::debug!("InvokeAction entry: row_id={} key={}", id, action_key);

        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let notif = queries::get_notification(&conn, id as i64, &self.state.enc_mode())
            .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?
            .ok_or_else(|| {
                tracing::debug!("InvokeAction: notification row_id={} not found", id);
                zbus::fdo::Error::Failed(format!("Notification {} not found", id))
            })?;

        // "default" is implicit per the FDO spec when the `default-action`
        // capability is advertised — callers usually don't include it in the
        // actions array, so accept it regardless.
        let has_action =
            action_key == "default" || notif.actions.iter().any(|a| a.id == action_key);
        if !has_action {
            tracing::debug!(
                "InvokeAction: notification row_id={} has no action '{}' (available: {:?})",
                id,
                action_key,
                notif.actions.iter().map(|a| &a.id).collect::<Vec<_>>(),
            );
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
                zbus::fdo::Error::Failed(format!("Failed to locate Notifications interface: {}", e))
            })?;

        NotificationsDaemonSignals::action_invoked(
            iface_ref.signal_emitter(),
            notif.dbus_id,
            &action_key,
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to emit ActionInvoked: {}", e)))?;

        // Per FDO spec, after an action is invoked the server must also emit
        // NotificationClosed(id, reason=2 "dismissed by user"). libnotify-based
        // apps (Signal Desktop, etc.) treat the action as complete only after
        // this second signal arrives.
        NotificationsDaemonSignals::notification_closed(
            iface_ref.signal_emitter(),
            notif.dbus_id,
            2,
        )
        .await
        .map_err(|e| {
            zbus::fdo::Error::Failed(format!("Failed to emit NotificationClosed: {}", e))
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

        // Local handler: a user rule with an `on_action` entry for this key
        // wins; otherwise, if the invoked action is "default" and the
        // notification carried a `desktop-entry` hint, focus/launch that
        // app via `gtk-launch`. Both paths are best-effort — a failure here
        // never undoes the FDO signal the client already received.
        if let Some((rule_name, cmd)) = self.state.rules_engine.action_command(&notif, &action_key)
        {
            let env = env_from_notif(&notif, &action_key);
            tracing::debug!(
                "rule '{}' -> spawning shell command for action '{}'",
                rule_name,
                action_key,
            );
            if let Err(e) = launcher::spawn_shell_command(&cmd, &env) {
                tracing::warn!("rule '{}' command failed to spawn: {}", rule_name, e,);
            }
        } else if action_key == "default" && !notif.desktop_entry.is_empty() {
            tracing::debug!("desktop-entry activation: {}", notif.desktop_entry);
            if let Err(e) = launcher::activate_desktop_entry(&notif.desktop_entry) {
                tracing::warn!("gtk-launch {} failed: {}", notif.desktop_entry, e,);
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

    /// Dismiss a notification from the GUI without invoking any action.
    ///
    /// Looks up the notification by row id, emits `NotificationClosed(id, 2)`
    /// (reason = "dismissed by user") on the freedesktop interface so that
    /// senders using `notify-send --wait` (or similar libnotify-based
    /// workflows) stop blocking, and marks the row as `cleared`.
    async fn dismiss(&self, id: u64) -> Result<(), zbus::fdo::Error> {
        tracing::debug!("Dismiss entry: row_id={}", id);

        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let notif = queries::get_notification(&conn, id as i64, &self.state.enc_mode())
            .map_err(|e| zbus::fdo::Error::Failed(format!("Query error: {}", e)))?
            .ok_or_else(|| {
                tracing::debug!("Dismiss: notification row_id={} not found", id);
                zbus::fdo::Error::Failed(format!("Notification {} not found", id))
            })?;

        let iface_ref: InterfaceRef<NotificationsDaemon> = self
            .connection
            .object_server()
            .interface("/org/freedesktop/Notifications")
            .await
            .map_err(|e| {
                zbus::fdo::Error::Failed(format!("Failed to locate Notifications interface: {}", e))
            })?;

        NotificationsDaemonSignals::notification_closed(
            iface_ref.signal_emitter(),
            notif.dbus_id,
            2,
        )
        .await
        .map_err(|e| {
            zbus::fdo::Error::Failed(format!("Failed to emit NotificationClosed: {}", e))
        })?;

        if let Some(row_id) = notif.row_id {
            if let Err(e) = queries::update_status(&conn, row_id, NotificationStatus::Cleared) {
                tracing::warn!(
                    "NotificationClosed emitted for row {} but failed to mark cleared: {}",
                    row_id,
                    e
                );
            }
        }

        tracing::debug!(
            "Dismissed notification row {} (dbus_id {})",
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

    /// Return the Do Not Disturb state as JSON:
    /// `{"enabled": bool, "allow_critical": bool}`. `allow_critical`
    /// comes from `[dnd]` in `config.toml` and is static for the life
    /// of the daemon process; `enabled` is runtime-toggleable via
    /// `set_dnd`.
    async fn get_dnd(&self) -> Result<String, zbus::fdo::Error> {
        let payload = serde_json::json!({
            "enabled": self.state.is_dnd(),
            "allow_critical": self.state.config.dnd.allow_critical,
        });
        Ok(payload.to_string())
    }

    /// Set the Do Not Disturb toggle. Persists to the `meta` table
    /// so the state survives daemon restarts, updates the in-memory
    /// atomic, and fires `dnd_changed` so reactive clients (popup,
    /// status bars) can refresh without polling.
    async fn set_dnd(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        enabled: bool,
    ) -> Result<(), zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        queries::set_meta(&conn, "dnd_enabled", if enabled { "true" } else { "false" })
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to persist DND: {}", e)))?;

        self.state.dnd_enabled.store(enabled, Ordering::Relaxed);
        tracing::info!("DND {}", if enabled { "enabled" } else { "disabled" });

        if let Err(e) = Self::dnd_changed(&emitter, enabled).await {
            tracing::warn!("failed to emit dnd_changed: {}", e);
        }

        Ok(())
    }

    /// Signal emitted whenever the DND toggle flips. Payload is the
    /// new `enabled` value.
    #[zbus(signal)]
    pub async fn dnd_changed(emitter: &SignalEmitter<'_>, enabled: bool) -> zbus::Result<()>;

    /// Return whether the daemon currently holds the X25519 secret
    /// key. `false` when encryption is disabled, when nobody has run
    /// `olha unlock`, or after `Lock` / idle auto-lock.
    async fn is_unlocked(&self) -> Result<bool, zbus::fdo::Error> {
        Ok(self.state.encryption.is_unlocked())
    }

    /// Derive the X25519 secret via `pass show` + DEK unwrap, and
    /// hold it in memory until `Lock` / auto-lock. Idempotent — calls
    /// made while already unlocked just bump the idle timer.
    async fn unlock(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), zbus::fdo::Error> {
        use crate::db::encryption::{derive_dek, run_pass_show, unwrap_sk};
        use crate::META_ENC_WRAPPED_SECRET;
        use base64::Engine;

        if !self.state.encryption.is_enabled() {
            return Err(zbus::fdo::Error::NotSupported(
                "encryption is disabled in config.toml".into(),
            ));
        }
        if self.state.encryption.is_unlocked() {
            // Idempotent — still nudge the idle clock.
            self.state.encryption.record_decrypt_activity();
            return Ok(());
        }

        let ikm = run_pass_show(&self.state.config.encryption.pass_entry).map_err(|e| {
            tracing::warn!("Unlock: pass show failed: {e}");
            zbus::fdo::Error::AuthFailed(format!("pass show failed: {e}"))
        })?;
        if ikm.is_empty() {
            return Err(zbus::fdo::Error::AuthFailed("pass entry is empty".into()));
        }
        let dek = derive_dek(&ikm);

        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("db open: {e}")))?;
        let wrapped_b64 = match crate::db::queries::get_meta(&conn, META_ENC_WRAPPED_SECRET)
            .map_err(|e| zbus::fdo::Error::Failed(format!("meta read: {e}")))?
        {
            Some(v) => v,
            None => {
                return Err(zbus::fdo::Error::Failed(
                    "no encryption material on disk — run `olha encryption init`".into(),
                ));
            }
        };
        let wrapped = base64::engine::general_purpose::STANDARD
            .decode(wrapped_b64.trim())
            .map_err(|e| {
                zbus::fdo::Error::Failed(format!(
                    "meta.{}: bad base64: {e}",
                    META_ENC_WRAPPED_SECRET
                ))
            })?;

        let sk = unwrap_sk(&dek, &wrapped).map_err(|e| {
            tracing::warn!("Unlock: wrapped-sk decrypt failed: {e}");
            zbus::fdo::Error::AuthFailed(
                "wrapped secret could not be decrypted — wrong pass entry?".into(),
            )
        })?;
        // DEK dropped here — we only need sk going forward.
        drop(dek);

        self.state.encryption.unlock(sk);
        tracing::info!("encryption unlocked");
        if let Err(e) = Self::locked_changed(&emitter, true).await {
            tracing::warn!("failed to emit locked_changed: {e}");
        }
        Ok(())
    }

    /// Zeroize the in-memory X25519 secret. Idempotent.
    async fn lock(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), zbus::fdo::Error> {
        let was_unlocked = self.state.encryption.lock();
        if was_unlocked {
            tracing::info!("encryption locked");
            if let Err(e) = Self::locked_changed(&emitter, false).await {
                tracing::warn!("failed to emit locked_changed: {e}");
            }
        }
        Ok(())
    }

    /// Signal emitted whenever the lock state flips. Payload is the
    /// new `unlocked` value (true means secret key is in memory).
    #[zbus(signal)]
    pub async fn locked_changed(emitter: &SignalEmitter<'_>, unlocked: bool) -> zbus::Result<()>;

    /// Get daemon status as JSON
    async fn status(&self) -> Result<String, zbus::fdo::Error> {
        let conn = self
            .state
            .open_db()
            .map_err(|e| zbus::fdo::Error::Failed(format!("Database error: {}", e)))?;

        let unread_filter = NotificationFilter {
            status: Some(NotificationStatus::Unread),
            ..Default::default()
        };
        let unread = queries::count_notifications(&conn, &unread_filter, &self.state.enc_mode())
            .unwrap_or(0);

        let total_filter = NotificationFilter::default();
        let total =
            queries::count_notifications(&conn, &total_filter, &self.state.enc_mode()).unwrap_or(0);

        let encryption_payload = if self.state.encryption.is_enabled() {
            let kid = self.state.encryption.key_id;
            serde_json::json!({
                "enabled": true,
                "unlocked": self.state.encryption.is_unlocked(),
                "key_id": format!("{:02x}{:02x}{:02x}{:02x}", kid[0], kid[1], kid[2], kid[3]),
                "idle_until_lock_secs": self.state.encryption.idle_until_lock_secs(),
                "auto_lock_secs": self.state.encryption.auto_lock_secs(),
            })
        } else {
            serde_json::json!({
                "enabled": false,
                "unlocked": false,
            })
        };

        let status = serde_json::json!({
            "status": "running",
            "version": "0.1.0",
            "unread": unread,
            "total": total,
            "db_path": self.state.db_path.display().to_string(),
            "rules_count": self.state.config.rules.len(),
            "dnd": {
                "enabled": self.state.is_dnd(),
                "allow_critical": self.state.config.dnd.allow_critical,
            },
            "encryption": encryption_payload,
        });

        Ok(status.to_string())
    }
}

impl ControlDaemon {
    pub fn new(state: Arc<DaemonState>, connection: zbus::Connection) -> Self {
        Self { state, connection }
    }
}

/// Build the env var list handed to a rule's `on_action` shell command.
fn env_from_notif(notif: &Notification, action_key: &str) -> Vec<(&'static str, String)> {
    let urgency = match notif.urgency {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::Critical => "critical",
    };
    vec![
        ("OLHA_APP_NAME", notif.app_name.clone()),
        ("OLHA_SUMMARY", notif.summary.clone()),
        ("OLHA_BODY", notif.body.clone()),
        ("OLHA_URGENCY", urgency.to_string()),
        ("OLHA_ACTION_KEY", action_key.to_string()),
        ("OLHA_DESKTOP_ENTRY", notif.desktop_entry.clone()),
        (
            "OLHA_NOTIFICATION_ID",
            notif.row_id.map(|r| r.to_string()).unwrap_or_default(),
        ),
    ]
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
