mod config;
mod db;
mod dbus;
mod launcher;
mod notification;
mod rules;

use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing_subscriber;
use zbus::Connection;

use config::Config;
use db::encryption::EncryptionContext;
use db::DbResult;
use dbus::{ControlDaemon, NotificationsDaemon};
use rules::RulesEngine;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Increase output verbosity (-v for warning, -vv for info, -vvv for debug)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Start without unlocking the data encryption key. Encrypted rows
    /// are served as `[encrypted]` placeholders and any incoming
    /// notification that would require encryption is rejected. Intended
    /// only for recovery — e.g. if `pass show` is wedged and you need
    /// to inspect or delete old rows.
    #[arg(long)]
    allow_degraded_read: bool,
}

/// Shared daemon state accessible by all D-Bus handlers
pub struct DaemonState {
    pub config: Config,
    pub db_path: PathBuf,
    pub rules_engine: RulesEngine,
    /// Data encryption context when `[encryption].enabled = true` in
    /// config AND the daemon successfully unlocked the DEK at startup.
    /// `None` means plaintext mode — either encryption is off, or we
    /// started with `--allow-degraded-read` for recovery.
    pub encryption: Option<Arc<EncryptionContext>>,
    /// Runtime Do Not Disturb toggle. Persisted to the `meta` table in
    /// SQLite so it survives daemon restarts. Read on every incoming
    /// notification, so it's an atomic rather than a lock.
    pub dnd_enabled: AtomicBool,
}

impl DaemonState {
    /// Open a new database connection (cheap with bundled SQLite)
    pub fn open_db(&self) -> Result<rusqlite::Connection, db::DbError> {
        Ok(rusqlite::Connection::open(&self.db_path)?)
    }

    /// Borrow the encryption context (if loaded) for threading into
    /// query calls. Most DB ops require an `Option<&EncryptionContext>`.
    pub fn enc(&self) -> Option<&EncryptionContext> {
        self.encryption.as_deref()
    }

    /// True when encryption is configured but no DEK is loaded — i.e.
    /// the daemon was started with `--allow-degraded-read`. In this
    /// mode, new notifications that would be encrypted must be
    /// rejected rather than stored plaintext.
    pub fn is_degraded(&self) -> bool {
        self.config.encryption.enabled && self.encryption.is_none()
    }

    /// Cheap read of the current DND state.
    pub fn is_dnd(&self) -> bool {
        self.dnd_enabled.load(Ordering::Relaxed)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => tracing_subscriber::filter::LevelFilter::INFO,
        1 => tracing_subscriber::filter::LevelFilter::WARN,
        2 => tracing_subscriber::filter::LevelFilter::INFO,
        _ => tracing_subscriber::filter::LevelFilter::DEBUG,
    };

    tracing_subscriber::fmt().with_max_level(log_level).init();

    // Load configuration
    let config = Config::load(None)?;
    let db_path = config.db_path();

    tracing::info!("olhad starting...");
    tracing::debug!("Database path: {}", db_path.display());
    tracing::debug!("Config: {:?}", config);

    // Snapshot the session env (WAYLAND_DISPLAY, GDK_SCALE, …) so spawned
    // click handlers inherit GUI-correct vars regardless of how `olhad`
    // itself was started.
    launcher::init_session_env();

    // Initialize database
    let init_conn = db::init(&db_path)?;
    tracing::info!("Database initialized");

    // Load persisted DND state. Missing or malformed → default off.
    let dnd_enabled = match db::queries::get_meta(&init_conn, "dnd_enabled") {
        Ok(Some(v)) => v == "true",
        Ok(None) => false,
        Err(e) => {
            tracing::warn!("failed to read dnd_enabled from meta: {e}; defaulting to off");
            false
        }
    };
    if dnd_enabled {
        tracing::info!("DND is enabled (persisted from previous run)");
    }
    drop(init_conn);

    // Unlock the DEK before anything else gets a chance to write an
    // unencrypted notification. Three outcomes:
    //   - encryption disabled in config → no DEK, plaintext mode
    //   - enabled + pass unlocks        → DEK loaded, full encryption
    //   - enabled + pass fails + flag   → degraded read-only mode
    //   - enabled + pass fails + !flag  → fail closed (exit)
    let encryption = if config.encryption.enabled {
        match EncryptionContext::load_from_pass(&config.encryption.pass_entry) {
            Ok(ctx) => {
                let kid = ctx.key_id();
                tracing::info!(
                    "encryption enabled; loaded DEK from pass entry '{}' (key_id={:02x}{:02x}{:02x}{:02x})",
                    config.encryption.pass_entry,
                    kid[0], kid[1], kid[2], kid[3],
                );
                Some(Arc::new(ctx))
            }
            Err(e) if cli.allow_degraded_read => {
                tracing::warn!(
                    "failed to load DEK ({e}); continuing in --allow-degraded-read mode. \
                     New notifications will be rejected and encrypted rows will be opaque."
                );
                None
            }
            Err(e) => {
                tracing::error!(
                    "encryption is enabled in config but the DEK could not be loaded: {e}\n\
                     Either run `olha encryption init` + `olha encryption enable` first, or \
                     start olhad once with `--allow-degraded-read` for recovery."
                );
                return Err(e.into());
            }
        }
    } else {
        None
    };

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
        encryption,
        dnd_enabled: AtomicBool::new(dnd_enabled),
    });

    // Create D-Bus connection
    let connection = Connection::session().await?;

    // Create the freedesktop notifications daemon
    let notif_daemon = NotificationsDaemon::new(Arc::clone(&state), connection.clone());
    let control_daemon = ControlDaemon::new(Arc::clone(&state), connection.clone());

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
            Ok(proxy) => match proxy
                .get_name_owner("org.freedesktop.Notifications".try_into().unwrap())
                .await
            {
                Ok(owner) => {
                    // Try to get the PID of the owner
                    match proxy
                        .get_connection_unix_process_id(owner.clone().into())
                        .await
                    {
                        Ok(pid) => {
                            // Try to read the process name from /proc
                            let proc_name = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                                .map(|s| s.trim().to_string())
                                .unwrap_or_else(|_| "unknown".to_string());
                            format!(
                                " Currently owned by '{}' (PID {}, bus name {}).",
                                proc_name, pid, owner
                            )
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
        tracing::error!(
            "Failed to claim org.olha.Daemon: {}. Is another olhad instance running?",
            e
        );
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
fn cleanup_notifications(
    db_path: &std::path::Path,
    max_age_secs: u64,
    max_count: i64,
) -> DbResult<i64> {
    let conn = rusqlite::Connection::open(db_path)?;
    db::queries::cleanup_old(&conn, max_age_secs, max_count)
}
