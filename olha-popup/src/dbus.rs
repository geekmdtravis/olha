use zbus::{proxy, Connection};

use crate::model::Notification;

#[proxy(
    interface = "org.olha.Daemon",
    default_service = "org.olha.Daemon",
    default_path = "/org/olha/Daemon"
)]
pub trait ControlDaemon {
    fn invoke_action(&self, id: u64, action_key: &str) -> zbus::Result<()>;

    fn dismiss(&self, id: u64) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_received(&self, notification: &str) -> zbus::Result<()>;
}

pub async fn connect() -> zbus::Result<ControlDaemonProxy<'static>> {
    let connection = Connection::session().await?;
    ControlDaemonProxy::new(&connection).await
}

pub async fn invoke_action(row_id: i64, action_key: String) -> Result<(), String> {
    tracing::debug!("D-Bus invoke_action row_id={} key={}", row_id, action_key);
    let proxy = connect().await.map_err(|e| e.to_string())?;
    proxy
        .invoke_action(row_id as u64, &action_key)
        .await
        .map_err(|e| e.to_string())
}

pub async fn dismiss(row_id: i64) -> Result<(), String> {
    tracing::debug!("D-Bus dismiss row_id={}", row_id);
    let proxy = connect().await.map_err(|e| e.to_string())?;
    proxy
        .dismiss(row_id as u64)
        .await
        .map_err(|e| e.to_string())
}

pub fn parse_signal_payload(json: &str) -> Option<Notification> {
    match serde_json::from_str(json) {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!("failed to parse notification JSON: {e}");
            None
        }
    }
}
