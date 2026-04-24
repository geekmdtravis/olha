use crate::db::DbResult;
use rusqlite::Connection;

/// Latest schema version. Bump this whenever you add new columns
/// below and a corresponding `migrate_*` step in `init_schema`.
const SCHEMA_VERSION: i64 = 2;

/// Initialize the database schema.
///
/// The base `notifications` table is created if missing, then each
/// migration step runs in order up to `SCHEMA_VERSION`. Progress is
/// tracked via SQLite's built-in `PRAGMA user_version`, so upgrades
/// are idempotent and safe across daemon restarts.
pub fn init_schema(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS notifications (
            id INTEGER PRIMARY KEY,
            dbus_id INTEGER NOT NULL,
            app_name TEXT NOT NULL DEFAULT '',
            app_icon TEXT NOT NULL DEFAULT '',
            summary TEXT NOT NULL DEFAULT '',
            body TEXT NOT NULL DEFAULT '',
            urgency INTEGER NOT NULL DEFAULT 1,
            category TEXT NOT NULL DEFAULT '',
            desktop_entry TEXT NOT NULL DEFAULT '',
            actions TEXT NOT NULL DEFAULT '[]',
            hints TEXT NOT NULL DEFAULT '{}',
            status TEXT NOT NULL DEFAULT 'unread',
            expire_timeout INTEGER NOT NULL DEFAULT -1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            closed_reason INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_notifications_status ON notifications(status);
        CREATE INDEX IF NOT EXISTS idx_notifications_app_name ON notifications(app_name);
        CREATE INDEX IF NOT EXISTS idx_notifications_urgency ON notifications(urgency);
        CREATE INDEX IF NOT EXISTS idx_notifications_created_at ON notifications(created_at);
        CREATE INDEX IF NOT EXISTS idx_notifications_category ON notifications(category);
        CREATE INDEX IF NOT EXISTS idx_notifications_dbus_id ON notifications(dbus_id);

        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;

    let current_version: i64 =
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current_version < 2 {
        migrate_to_v2(conn)?;
    }

    if current_version != SCHEMA_VERSION {
        conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION))?;
    }

    Ok(())
}

/// v2: at-rest encryption columns. `summary_enc`, `body_enc`, `hints_enc`
/// hold `nonce(24) || ciphertext || tag(16)` when `enc_version > 0`;
/// otherwise the plaintext TEXT columns are authoritative.
fn migrate_to_v2(conn: &Connection) -> DbResult<()> {
    // Each ALTER is wrapped so the migration is re-runnable on a DB
    // that was manually patched (columns already present). SQLite only
    // surfaces "duplicate column name" which we treat as a no-op.
    for stmt in [
        "ALTER TABLE notifications ADD COLUMN summary_enc BLOB",
        "ALTER TABLE notifications ADD COLUMN body_enc BLOB",
        "ALTER TABLE notifications ADD COLUMN hints_enc BLOB",
        "ALTER TABLE notifications ADD COLUMN enc_version INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE notifications ADD COLUMN key_id BLOB",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e.into());
            }
        }
    }
    Ok(())
}
