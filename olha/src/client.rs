use futures_util::StreamExt;
use serde_json::json;
use zbus::proxy;

use crate::output;

#[proxy(
    interface = "org.olha.Daemon",
    default_service = "org.olha.Daemon",
    default_path = "/org/olha/Daemon"
)]
pub trait ControlDaemon {
    fn list(&self, filter: &str) -> zbus::Result<String>;
    fn count(&self, filter: &str) -> zbus::Result<String>;
    fn mark_read(&self, ids: &[u64]) -> zbus::Result<()>;
    fn mark_read_all(&self) -> zbus::Result<()>;
    fn clear(&self, ids: &[u64]) -> zbus::Result<()>;
    fn delete(&self, ids: &[u64]) -> zbus::Result<()>;
    fn clear_all(&self) -> zbus::Result<()>;
    fn delete_all(&self) -> zbus::Result<()>;
    fn get_notification(&self, id: u64) -> zbus::Result<String>;
    fn invoke_action(&self, id: u64, action_key: &str) -> zbus::Result<()>;
    fn status(&self) -> zbus::Result<String>;
    fn get_dnd(&self) -> zbus::Result<String>;
    fn set_dnd(&self, enabled: bool) -> zbus::Result<()>;
    fn unlock(&self) -> zbus::Result<()>;
    fn lock(&self) -> zbus::Result<()>;
    fn is_unlocked(&self) -> zbus::Result<bool>;

    #[zbus(signal)]
    fn notification_received(&self, notification: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn locked_changed(&self, unlocked: bool) -> zbus::Result<()>;
}

pub struct ListFilter {
    pub app: Option<String>,
    pub urgency: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
    pub search: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: i64,
    pub json: bool,
}

pub struct CountFilter {
    pub app: Option<String>,
    pub urgency: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
    pub search: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub json: bool,
}

/// Open a session D-Bus connection and build the ControlDaemon proxy.
/// Maps the common "daemon not running" failure mode into a friendly message.
async fn connect() -> Result<ControlDaemonProxy<'static>, Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection)
        .await
        .map_err(map_proxy_error)?;
    Ok(proxy)
}

fn map_proxy_error(err: zbus::Error) -> Box<dyn std::error::Error> {
    if is_service_unknown(&err) {
        daemon_not_running_error()
    } else {
        Box::new(err)
    }
}

fn is_service_unknown(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::FDO(fdo) => matches!(**fdo, zbus::fdo::Error::ServiceUnknown(_)),
        zbus::Error::MethodError(name, _, _) => {
            name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner"
        }
        _ => false,
    }
}

fn daemon_not_running_error() -> Box<dyn std::error::Error> {
    Box::<dyn std::error::Error>::from(
        "olhad is not running. Start it with `olhad` (or via your service manager).",
    )
}

/// Map an error that came back from a method call on the proxy. The proxy
/// itself might have been created successfully before the daemon exited, so
/// the service-unknown check still applies at call time.
fn map_call_error(err: zbus::Error) -> Box<dyn std::error::Error> {
    if is_service_unknown(&err) {
        daemon_not_running_error()
    } else {
        Box::new(err)
    }
}

pub async fn list(filter: ListFilter) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    let filter_json = json!({
        "app": filter.app,
        "urgency": filter.urgency,
        "status": filter.status,
        "category": filter.category,
        "search": filter.search,
        "since": filter.since,
        "until": filter.until,
        "limit": filter.limit,
    });

    let result = proxy
        .list(filter_json.to_string().as_str())
        .await
        .map_err(map_call_error)?;

    if filter.json {
        println!("{}", result);
    } else {
        print!("{}", output::format_notification_table(&result));
    }

    Ok(())
}

pub async fn count(filter: CountFilter) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    let filter_json = json!({
        "app": filter.app,
        "urgency": filter.urgency,
        "status": filter.status,
        "category": filter.category,
        "search": filter.search,
        "since": filter.since,
        "until": filter.until,
    });

    let result = proxy
        .count(filter_json.to_string().as_str())
        .await
        .map_err(map_call_error)?;

    if filter.json {
        println!("{}", result);
    } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result) {
        let unread = val.get("unread").and_then(|v| v.as_i64()).unwrap_or(0);
        let total = val.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
        println!("Notifications: {} unread, {} total", unread, total);
    } else {
        println!("{}", result);
    }

    Ok(())
}

pub async fn show(id: u64, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    let result = proxy.get_notification(id).await.map_err(map_call_error)?;

    if json {
        println!("{}", result);
    } else {
        print!("{}", output::format_notification_detail(&result));
    }

    Ok(())
}

pub async fn mark_read(ids: Vec<u64>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !all && ids.is_empty() {
        return Err("Specify notification IDs or use --all".into());
    }

    let proxy = connect().await?;

    if all {
        proxy.mark_read_all().await.map_err(map_call_error)?;
        println!("Marked all notifications as read");
    } else {
        proxy.mark_read(&ids).await.map_err(map_call_error)?;
        println!("Marked {} notification(s) as read", ids.len());
    }

    Ok(())
}

pub async fn clear(ids: Vec<u64>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !all && ids.is_empty() {
        return Err("Specify notification IDs or use --all".into());
    }

    let proxy = connect().await?;

    if all {
        proxy.clear_all().await.map_err(map_call_error)?;
        println!("Cleared all notifications");
    } else {
        proxy.clear(&ids).await.map_err(map_call_error)?;
        println!("Cleared {} notification(s)", ids.len());
    }

    Ok(())
}

pub async fn delete(ids: Vec<u64>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !all && ids.is_empty() {
        return Err("Specify notification IDs or use --all".into());
    }

    let proxy = connect().await?;

    if all {
        proxy.delete_all().await.map_err(map_call_error)?;
        println!("Deleted all notifications");
    } else {
        proxy.delete(&ids).await.map_err(map_call_error)?;
        println!("Deleted {} notification(s)", ids.len());
    }

    Ok(())
}

pub async fn invoke(id: u64, action_key: String) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    proxy
        .invoke_action(id, &action_key)
        .await
        .map_err(map_call_error)?;
    println!("Invoked action: {}", action_key);

    Ok(())
}

pub async fn subscribe(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    let mut stream = proxy
        .receive_notification_received()
        .await
        .map_err(map_call_error)?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let notification_json = args.notification();

        if json {
            println!("{}", notification_json);
        } else {
            // One-line summary: [urgency] [App] Summary — Body
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(notification_json) {
                let app = val.get("app_name").and_then(|v| v.as_str()).unwrap_or("");
                let summary = val.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                let body = val.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let urgency = val
                    .get("urgency")
                    .and_then(|v| v.as_str())
                    .unwrap_or("normal");

                if body.is_empty() {
                    println!("[{}] [{}] {}", urgency, app, summary);
                } else {
                    println!("[{}] [{}] {} — {}", urgency, app, summary, body);
                }
            } else {
                println!("{}", notification_json);
            }
        }
    }

    Ok(())
}

/// What the user typed as the argument to `olha dnd`.
#[derive(Debug, Clone, Copy)]
pub enum DndAction {
    Status,
    On,
    Off,
    Toggle,
}

pub async fn dnd(action: DndAction, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    let target: Option<bool> = match action {
        DndAction::Status => None,
        DndAction::On => Some(true),
        DndAction::Off => Some(false),
        DndAction::Toggle => {
            let current = read_dnd_enabled(&proxy).await?;
            Some(!current)
        }
    };

    if let Some(enabled) = target {
        proxy.set_dnd(enabled).await.map_err(map_call_error)?;
    }

    let raw = proxy.get_dnd().await.map_err(map_call_error)?;

    if json {
        println!("{}", raw);
        return Ok(());
    }

    let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
    let enabled = val
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let allow_critical = val
        .get("allow_critical")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let state = if enabled { "on" } else { "off" };
    println!("DND: {}", state);
    if enabled {
        if allow_critical {
            println!("  Critical urgency notifications still break through.");
        } else {
            println!("  All notifications are silenced.");
        }
    }

    Ok(())
}

async fn read_dnd_enabled(
    proxy: &ControlDaemonProxy<'_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let raw = proxy.get_dnd().await.map_err(map_call_error)?;
    let val: serde_json::Value = serde_json::from_str(&raw)?;
    Ok(val
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn unlock() -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;
    proxy.unlock().await.map_err(map_call_error)?;
    let raw = proxy.status().await.map_err(map_call_error)?;
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(kid) = val.pointer("/encryption/key_id").and_then(|v| v.as_str()) {
            println!("unlocked (key_id: {})", kid);
            return Ok(());
        }
    }
    println!("unlocked");
    Ok(())
}

pub async fn lock() -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;
    proxy.lock().await.map_err(map_call_error)?;
    println!("locked");
    Ok(())
}

pub async fn status(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = connect().await?;

    let result = proxy.status().await.map_err(map_call_error)?;

    if json {
        println!("{}", result);
    } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result) {
        println!("olhad status:");
        if let Some(s) = val.get("status").and_then(|v| v.as_str()) {
            println!("  Status: {}", s);
        }
        if let Some(v) = val.get("version").and_then(|v| v.as_str()) {
            println!("  Version: {}", v);
        }
        if let Some(n) = val.get("unread").and_then(|v| v.as_i64()) {
            println!("  Unread: {}", n);
        }
        if let Some(n) = val.get("total").and_then(|v| v.as_i64()) {
            println!("  Total: {}", n);
        }
        if let Some(p) = val.get("db_path").and_then(|v| v.as_str()) {
            println!("  DB Path: {}", p);
        }
        if let Some(n) = val.get("rules_count").and_then(|v| v.as_i64()) {
            println!("  Rules: {}", n);
        }
        if let Some(dnd) = val.get("dnd") {
            let enabled = dnd
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!("  DND: {}", if enabled { "on" } else { "off" });
        }
        if let Some(enc) = val.get("encryption") {
            let enabled = enc
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if enabled {
                let unlocked = enc
                    .get("unlocked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let kid = enc.get("key_id").and_then(|v| v.as_str()).unwrap_or("?");
                println!(
                    "  Encryption: {} (key_id {})",
                    if unlocked { "unlocked" } else { "locked" },
                    kid
                );
            } else {
                println!("  Encryption: off");
            }
        }
    } else {
        println!("{}", result);
    }

    Ok(())
}
