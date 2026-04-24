mod config;
mod db;
mod dbus;
mod launcher;
mod notification;
mod rules;

use base64::Engine;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber;
use x25519_dalek::PublicKey;
use zbus::Connection;

use config::Config;
use db::encryption::{EncMode, EncryptionState, X25519_KEY_LEN};
use db::DbResult;
use dbus::{ControlDaemon, ControlDaemonSignals, NotificationsDaemon};
use rules::RulesEngine;

/// `meta` key where the long-lived X25519 public key is stored
/// (base64). Read at daemon startup when encryption is enabled.
pub const META_ENC_PUBLIC_KEY: &str = "enc_public_key";
/// `meta` key holding the DEK-wrapped X25519 secret. Read only
/// during `Unlock`.
pub const META_ENC_WRAPPED_SECRET: &str = "enc_wrapped_secret";
/// `meta` key holding the hex-encoded fingerprint of the public key.
pub const META_ENC_KEY_ID: &str = "enc_key_id";
/// `meta` key holding the hex-encoded DEK fingerprint (reporting only).
pub const META_ENC_DEK_KID: &str = "enc_dek_kid";

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Increase output verbosity (-v for warning, -vv for info, -vvv for debug)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// Shared daemon state accessible by all D-Bus handlers.
pub struct DaemonState {
    pub config: Config,
    pub db_path: PathBuf,
    pub rules_engine: RulesEngine,
    /// Always present. `EncryptionState::plaintext()` when encryption
    /// is disabled in config; `with_public_key(...)` otherwise. The
    /// actual secret key is populated only after a successful
    /// `Unlock` (and zeroized on `Lock` / auto-lock).
    pub encryption: Arc<EncryptionState>,
    /// Runtime Do Not Disturb toggle. Persisted to the `meta` table.
    pub dnd_enabled: AtomicBool,
}

impl DaemonState {
    pub fn open_db(&self) -> Result<rusqlite::Connection, db::DbError> {
        Ok(rusqlite::Connection::open(&self.db_path)?)
    }

    /// Build a per-call `EncMode` for threading into DB queries.
    pub fn enc_mode(&self) -> EncMode {
        self.encryption.enc_mode()
    }

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

    let config = Config::load(None)?;
    let db_path = config.db_path();

    tracing::info!("olhad starting...");
    tracing::debug!("Database path: {}", db_path.display());
    tracing::debug!("Config: {:?}", config);

    launcher::init_session_env();

    let init_conn = db::init(&db_path)?;
    tracing::info!("Database initialized");

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

    // Build EncryptionState. Three outcomes:
    //   - encryption disabled in config → plaintext mode
    //   - enabled + pk in meta          → locked mode (writes seal, reads need unlock)
    //   - enabled + missing pk          → fail closed with a clear diagnostic
    let encryption = if config.encryption.enabled {
        let raw = db::queries::get_meta(&init_conn, META_ENC_PUBLIC_KEY)?;
        match raw {
            Some(b64) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .map_err(|e| -> Box<dyn std::error::Error> {
                        format!(
                            "encryption.enabled=true but meta.{} is not valid base64: {e}",
                            META_ENC_PUBLIC_KEY
                        )
                        .into()
                    })?;
                if bytes.len() != X25519_KEY_LEN {
                    return Err(format!(
                        "encryption.enabled=true but meta.{} is {} bytes, expected {}",
                        META_ENC_PUBLIC_KEY,
                        bytes.len(),
                        X25519_KEY_LEN,
                    )
                    .into());
                }
                let mut pk_arr = [0u8; X25519_KEY_LEN];
                pk_arr.copy_from_slice(&bytes);
                let pk = PublicKey::from(pk_arr);
                let state = EncryptionState::with_public_key(pk, config.encryption.auto_lock_secs);
                let kid = state.key_id;
                tracing::info!(
                    "encryption enabled; loaded public key (key_id={:02x}{:02x}{:02x}{:02x}); daemon starts locked",
                    kid[0], kid[1], kid[2], kid[3],
                );
                Arc::new(state)
            }
            None => {
                tracing::error!(
                    "encryption is enabled in config but no key material found in the DB. \
                     Run `olha encryption init` first (or set `[encryption].enabled = false`)."
                );
                return Err("missing encryption key material".into());
            }
        }
    } else {
        Arc::new(EncryptionState::plaintext())
    };

    drop(init_conn);

    let rules_engine = RulesEngine::new(&config.rules).map_err(|e| {
        tracing::error!("Failed to compile rules: {}", e);
        e
    })?;
    tracing::info!("Rules engine initialized with {} rules", config.rules.len());

    let state = Arc::new(DaemonState {
        config: config.clone(),
        db_path: db_path.clone(),
        rules_engine,
        encryption: Arc::clone(&encryption),
        dnd_enabled: AtomicBool::new(dnd_enabled),
    });

    let connection = Connection::session().await?;

    let notif_daemon = NotificationsDaemon::new(Arc::clone(&state), connection.clone());
    let control_daemon = ControlDaemon::new(Arc::clone(&state), connection.clone());

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

    if let Err(e) = connection
        .request_name("org.freedesktop.Notifications")
        .await
    {
        let owner_info = match zbus::fdo::DBusProxy::new(&connection).await {
            Ok(proxy) => match proxy
                .get_name_owner("org.freedesktop.Notifications".try_into().unwrap())
                .await
            {
                Ok(owner) => match proxy
                    .get_connection_unix_process_id(owner.clone().into())
                    .await
                {
                    Ok(pid) => {
                        let proc_name = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|_| "unknown".to_string());
                        format!(
                            " Currently owned by '{}' (PID {}, bus name {}).",
                            proc_name, pid, owner
                        )
                    }
                    Err(_) => format!(" Currently owned by bus name {}.", owner),
                },
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

    let db_path_clone = db_path.clone();
    let config_clone = config.clone();
    tokio::spawn(async move {
        cleanup_loop(db_path_clone, config_clone).await;
    });

    // Idle auto-lock task: polls every 30s and calls Lock when the
    // idle threshold has elapsed. Enabled only when the daemon has
    // encryption material and auto_lock_secs > 0.
    if encryption.is_enabled() && encryption.auto_lock_secs() > 0 {
        let enc_for_task = Arc::clone(&encryption);
        let conn_for_task = connection.clone();
        tokio::spawn(async move {
            auto_lock_loop(enc_for_task, conn_for_task).await;
        });
        tracing::info!(
            "Auto-lock enabled: sk will evict after {}s of idle",
            encryption.auto_lock_secs(),
        );
    }

    tracing::info!("olhad ready and listening");

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

async fn cleanup_loop(db_path: PathBuf, config: Config) {
    let cleanup_interval = config.retention.cleanup_interval_secs();
    let max_age = config.retention.max_age_secs();
    let max_count = config.retention.max_count;

    loop {
        tokio::time::sleep(Duration::from_secs(cleanup_interval)).await;

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

fn cleanup_notifications(
    db_path: &std::path::Path,
    max_age_secs: u64,
    max_count: i64,
) -> DbResult<i64> {
    let conn = rusqlite::Connection::open(db_path)?;
    db::queries::cleanup_old(&conn, max_age_secs, max_count)
}

/// Background task driving idle auto-lock. Wakes every 30s and, when
/// `should_auto_lock()` returns true, zeroizes the in-memory secret
/// and emits `locked_changed(false)`.
async fn auto_lock_loop(encryption: Arc<EncryptionState>, connection: zbus::Connection) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        if !encryption.should_auto_lock() {
            continue;
        }
        let locked_something = encryption.lock();
        if !locked_something {
            // Race — something else already locked it.
            continue;
        }
        tracing::info!("auto-locked encryption state (idle timeout reached)");
        emit_locked_changed(&connection, false).await;
    }
}

/// Emit `locked_changed(unlocked)` on `org.olha.Daemon`. Shared with
/// the dbus handlers so manual and automatic locks take the same path.
pub async fn emit_locked_changed(connection: &zbus::Connection, unlocked: bool) {
    use zbus::object_server::InterfaceRef;
    let iface_ref: InterfaceRef<ControlDaemon> = match connection
        .object_server()
        .interface("/org/olha/Daemon")
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("failed to locate ControlDaemon interface for locked_changed: {e}");
            return;
        }
    };
    if let Err(e) = ControlDaemonSignals::locked_changed(iface_ref.signal_emitter(), unlocked).await
    {
        tracing::warn!("failed to emit locked_changed: {e}");
    }
}
