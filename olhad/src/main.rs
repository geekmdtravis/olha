mod notification;
mod config;
mod db;
mod rules;
mod dbus;

use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber;
use zbus::Connection;

use config::Config;
use db::DbResult;
use dbus::{NotificationsDaemon, ControlDaemon};
use rules::RulesEngine;

/// Shared daemon state accessible by all D-Bus handlers
pub struct DaemonState {
    pub config: Config,
    pub db_path: PathBuf,
    pub rules_engine: RulesEngine,
}

impl DaemonState {
    /// Open a new database connection (cheap with bundled SQLite)
    pub fn open_db(&self) -> Result<rusqlite::Connection, db::DbError> {
        Ok(rusqlite::Connection::open(&self.db_path)?)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing_subscriber::filter::LevelFilter::INFO.into()),
        )
        .init();

    // Load configuration
    let config = Config::load(None)?;
    let db_path = config.db_path();

    tracing::info!("olhad starting...");
    tracing::debug!("Database path: {}", db_path.display());
    tracing::debug!("Config: {:?}", config);

    // Initialize database
    let _conn = db::init(&db_path)?;
    tracing::info!("Database initialized");

    // Create rules engine
    let rules_engine = RulesEngine::new(&config.rules).map_err(|e| {
        tracing::error!("Failed to compile rules: {}", e);
        e
    })?;
    tracing::info!("Rules engine initialized with {} rules", config.rules.len());

    // Create shared state
    let state = Arc::new(DaemonState {
        config: config.clone(),
        db_path: db_path.clone(),
        rules_engine,
    });

    // Create D-Bus connection
    let connection = Connection::session().await?;

    // Create the freedesktop notifications daemon
    let notif_daemon = NotificationsDaemon::new(Arc::clone(&state), connection.clone());
    let control_daemon = ControlDaemon::new(Arc::clone(&state));

    // Register both interfaces
    connection
        .object_server()
        .at("/org/freedesktop/Notifications", notif_daemon)
        .await
        .map_err(|e| {
            tracing::error!("Failed to register freedesktop interface: {}", e);
            e
        })?;

    connection
        .object_server()
        .at("/org/olha/Daemon", control_daemon)
        .await
        .map_err(|e| {
            tracing::error!("Failed to register olha control interface: {}", e);
            e
        })?;

    // Request D-Bus names
    if let Err(e) = connection
        .request_name("org.freedesktop.Notifications")
        .await
    {
        // Check who currently owns the name
        let owner_info = match zbus::fdo::DBusProxy::new(&connection).await {
            Ok(proxy) => match proxy.get_name_owner("org.freedesktop.Notifications".try_into().unwrap()).await {
                Ok(owner) => {
                    // Try to get the PID of the owner
                    match proxy.get_connection_unix_process_id(owner.clone().into()).await {
                        Ok(pid) => {
                            // Try to read the process name from /proc
                            let proc_name = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                                .map(|s| s.trim().to_string())
                                .unwrap_or_else(|_| "unknown".to_string());
                            format!(" Currently owned by '{}' (PID {}, bus name {}).", proc_name, pid, owner)
                        }
                        Err(_) => format!(" Currently owned by bus name {}.", owner),
                    }
                }
                Err(_) => String::new(),
            },
            Err(_) => String::new(),
        };
        tracing::error!(
            "Failed to claim org.freedesktop.Notifications: {}.{}",
            e,
            owner_info,
        );
        tracing::error!(
            "Another notification daemon is running. Stop it first (e.g. 'systemctl --user stop swaync') \
             or test olhad in an isolated session with 'dbus-run-session -- bash'."
        );
        return Err(e.into());
    }
    tracing::info!("Registered org.freedesktop.Notifications");

    if let Err(e) = connection.request_name("org.olha.Daemon").await {
        tracing::error!("Failed to claim org.olha.Daemon: {}. Is another olhad instance running?", e);
        return Err(e.into());
    }
    tracing::info!("Registered org.olha.Daemon");

    // Start background cleanup task
    let db_path_clone = db_path.clone();
    let config_clone = config.clone();
    tokio::spawn(async move {
        cleanup_loop(db_path_clone, config_clone).await;
    });

    tracing::info!("olhad ready and listening");

    // Keep the daemon running
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
}

/// Background cleanup task for old notifications
async fn cleanup_loop(db_path: PathBuf, config: Config) {
    let cleanup_interval = config.retention.cleanup_interval_secs();
    let max_age = config.retention.max_age_secs();
    let max_count = config.retention.max_count;

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(cleanup_interval)).await;

        match cleanup_notifications(&db_path, max_age, max_count) {
            Ok(deleted) => {
                if deleted > 0 {
                    tracing::debug!("Cleanup: deleted {} old notifications", deleted);
                }
            }
            Err(e) => {
                tracing::error!("Cleanup failed: {}", e);
            }
        }
    }
}

/// Run cleanup on old notifications
fn cleanup_notifications(db_path: &std::path::Path, max_age_secs: u64, max_count: i64) -> DbResult<i64> {
    let conn = rusqlite::Connection::open(db_path)?;
    db::queries::cleanup_old(&conn, max_age_secs, max_count)
}
