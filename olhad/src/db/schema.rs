use crate::db::DbResult;
use rusqlite::Connection;

/// Initialize the database schema
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
        ",
    )?;

    Ok(())
}
