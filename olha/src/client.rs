use futures_util::StreamExt;
use zbus::proxy;
use serde_json::json;

use crate::output;

#[proxy(
    interface = "org.olha.Daemon",
    default_service = "org.olha.Daemon",
    default_path = "/org/olha/Daemon"
)]
pub trait ControlDaemon {
    fn list(&self, filter: &str) -> zbus::Result<String>;
    fn count(&self) -> zbus::Result<(u32, u32)>;
    fn mark_read(&self, ids: &[u64]) -> zbus::Result<()>;
    fn mark_read_all(&self) -> zbus::Result<()>;
    fn clear(&self, ids: &[u64]) -> zbus::Result<()>;
    fn delete(&self, ids: &[u64]) -> zbus::Result<()>;
    fn clear_all(&self) -> zbus::Result<()>;
    fn delete_all(&self) -> zbus::Result<()>;
    fn get_notification(&self, id: u64) -> zbus::Result<String>;
    fn invoke_action(&self, id: u64, action_key: &str) -> zbus::Result<()>;
    fn status(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn notification_received(&self, notification: &str) -> zbus::Result<()>;
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

pub async fn list(filter: ListFilter) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

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

    let result = proxy.list(filter_json.to_string().as_str()).await?;

    if filter.json {
        println!("{}", result);
    } else {
        print!("{}", output::format_notification_table(&result));
    }

    Ok(())
}

pub async fn count(_status: Option<String>, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    let (unread, total) = proxy.count().await?;

    if json {
        println!("{{\"unread\":{},\"total\":{}}}", unread, total);
    } else {
        println!("Notifications: {} unread, {} total", unread, total);
    }

    Ok(())
}

pub async fn show(id: u64, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    let result = proxy.get_notification(id).await?;

    if json {
        println!("{}", result);
    } else {
        print!("{}", output::format_notification_detail(&result));
    }

    Ok(())
}

pub async fn mark_read(ids: Vec<u64>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    if all {
        proxy.mark_read_all().await?;
        println!("Marked all notifications as read");
    } else if !ids.is_empty() {
        proxy.mark_read(&ids).await?;
        println!("Marked {} notification(s) as read", ids.len());
    }

    Ok(())
}

pub async fn clear(ids: Vec<u64>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    if all {
        proxy.clear_all().await?;
        println!("Cleared all notifications");
    } else if !ids.is_empty() {
        proxy.clear(&ids).await?;
        println!("Cleared {} notification(s)", ids.len());
    }

    Ok(())
}

pub async fn delete(ids: Vec<u64>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    if all {
        proxy.delete_all().await?;
        println!("Deleted all notifications");
    } else if !ids.is_empty() {
        proxy.delete(&ids).await?;
        println!("Deleted {} notification(s)", ids.len());
    }

    Ok(())
}

pub async fn invoke(id: u64, action_key: String) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    proxy.invoke_action(id, &action_key).await?;
    println!("Invoked action: {}", action_key);

    Ok(())
}

pub async fn subscribe(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    let mut stream = proxy.receive_notification_received().await?;

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
                let urgency = val.get("urgency").and_then(|v| v.as_str()).unwrap_or("normal");

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

pub async fn status(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::session().await?;
    let proxy = ControlDaemonProxy::new(&connection).await?;

    let result = proxy.status().await?;

    if json {
        println!("{}", result);
    } else {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result) {
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
        } else {
            println!("{}", result);
        }
    }

    Ok(())
}


