use crate::db::DbResult;
use crate::notification::{ClosedReason, Notification, NotificationStatus, Urgency};
use chrono::Utc;
use rusqlite::{params, Connection, Row};
use tracing;

/// Insert a new notification into the database
pub fn insert_notification(conn: &Connection, notif: &Notification) -> DbResult<i64> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO notifications (
            dbus_id, app_name, app_icon, summary, body, urgency, category,
            desktop_entry, actions, hints, status, expire_timeout, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            notif.dbus_id,
            &notif.app_name,
            &notif.app_icon,
            &notif.summary,
            &notif.body,
            notif.urgency.as_u32(),
            &notif.category,
            &notif.desktop_entry,
            serde_json::to_string(&notif.actions).unwrap_or_default(),
            notif.hints.to_string(),
            notif.status.as_str(),
            notif.expire_timeout,
            &notif.created_at.to_rfc3339(),
            &now,
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Fetch a notification by row ID
pub fn get_notification(conn: &Connection, row_id: i64) -> DbResult<Option<Notification>> {
    let mut stmt = conn.prepare(
        "SELECT id, dbus_id, app_name, app_icon, summary, body, urgency, category,
                desktop_entry, actions, hints, status, expire_timeout, created_at, updated_at,
                closed_reason
         FROM notifications WHERE id = ?1",
    )?;

    let result = stmt.query_row(params![row_id], |row| notification_from_row(row));

    match result {
        Ok(notif) => Ok(Some(notif)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Fetch notifications with optional filtering
#[derive(Default, Clone)]
pub struct NotificationFilter {
    pub app_name: Option<String>,
    pub urgency: Option<Urgency>,
    pub status: Option<NotificationStatus>,
    pub category: Option<String>,
    pub search: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl NotificationFilter {
    /// Build SQL query and params for filtering
    fn build_query(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut query = "SELECT id, dbus_id, app_name, app_icon, summary, body, urgency, category,
                          desktop_entry, actions, hints, status, expire_timeout, created_at, updated_at,
                          closed_reason FROM notifications WHERE 1=1".to_string();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref app) = self.app_name {
            query.push_str(" AND app_name = ?");
            params.push(Box::new(app.clone()));
        }

        if let Some(urgency) = self.urgency {
            query.push_str(" AND urgency = ?");
            params.push(Box::new(urgency.as_u32() as i64));
        }

        if let Some(status) = self.status {
            query.push_str(" AND status = ?");
            params.push(Box::new(status.as_str().to_string()));
        }

        if let Some(ref cat) = self.category {
            query.push_str(" AND category = ?");
            params.push(Box::new(cat.clone()));
        }

        if let Some(ref search) = self.search {
            query.push_str(" AND (summary LIKE ? OR body LIKE ?)");
            let pattern = format!("%{}%", search);
            params.push(Box::new(pattern.clone()));
            params.push(Box::new(pattern));
        }

        if let Some(ref since) = self.since {
            query.push_str(" AND created_at >= ?");
            params.push(Box::new(since.clone()));
        }

        if let Some(ref until) = self.until {
            query.push_str(" AND created_at <= ?");
            params.push(Box::new(until.clone()));
        }

        query.push_str(" ORDER BY created_at DESC");

        if let Some(limit) = self.limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = self.offset {
            query.push_str(&format!(" OFFSET {}", offset));
        }

        (query, params)
    }

    /// Build SQL query for COUNT(*) with same filtering
    fn build_count_query(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut query = "SELECT COUNT(*) FROM notifications WHERE 1=1".to_string();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref app) = self.app_name {
            query.push_str(" AND app_name = ?");
            params.push(Box::new(app.clone()));
        }

        if let Some(urgency) = self.urgency {
            query.push_str(" AND urgency = ?");
            params.push(Box::new(urgency.as_u32() as i64));
        }

        if let Some(status) = self.status {
            query.push_str(" AND status = ?");
            params.push(Box::new(status.as_str().to_string()));
        }

        if let Some(ref cat) = self.category {
            query.push_str(" AND category = ?");
            params.push(Box::new(cat.clone()));
        }

        if let Some(ref search) = self.search {
            query.push_str(" AND (summary LIKE ? OR body LIKE ?)");
            let pattern = format!("%{}%", search);
            params.push(Box::new(pattern.clone()));
            params.push(Box::new(pattern));
        }

        if let Some(ref since) = self.since {
            query.push_str(" AND created_at >= ?");
            params.push(Box::new(since.clone()));
        }

        if let Some(ref until) = self.until {
            query.push_str(" AND created_at <= ?");
            params.push(Box::new(until.clone()));
        }

        tracing::debug!("count query: {}, param count: {}", query, params.len());

        (query, params)
    }
}

/// Query notifications with filtering
pub fn query_notifications(
    conn: &Connection,
    filter: &NotificationFilter,
) -> DbResult<Vec<Notification>> {
    let (query, params) = filter.build_query();

    let mut stmt = conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut rows = stmt.query(param_refs.as_slice())?;

    let mut notifications = Vec::new();
    while let Some(row) = rows.next()? {
        notifications.push(notification_from_row(row)?);
    }

    Ok(notifications)
}

/// Count notifications with filtering
pub fn count_notifications(conn: &Connection, filter: &NotificationFilter) -> DbResult<i64> {
    let (query, params) = filter.build_count_query();

    let mut stmt = conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let count: i64 = match stmt.query_row(param_refs.as_slice(), |row| row.get(0)) {
        Ok(c) => c,
        Err(rusqlite::Error::QueryReturnedNoRows) => 0,
        Err(e) => return Err(e.into()),
    };

    tracing::debug!("count result: {}", count);

    Ok(count)
}

/// Update all notifications matching a given status to a new status
pub fn update_all_status(
    conn: &Connection,
    from_statuses: &[NotificationStatus],
    to_status: NotificationStatus,
) -> DbResult<usize> {
    if from_statuses.is_empty() {
        return Ok(0);
    }

    let now = Utc::now().to_rfc3339();
    let placeholders = (0..from_statuses.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");

    let query = format!(
        "UPDATE notifications SET status = ?, updated_at = ? WHERE status IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;
    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, to_status.as_str())?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, &now)?;
    param_idx += 1;

    for s in from_statuses {
        stmt.raw_bind_parameter(param_idx, s.as_str())?;
        param_idx += 1;
    }

    let changed = stmt.raw_execute()?;
    Ok(changed)
}

/// Delete all notifications
pub fn delete_all(conn: &Connection) -> DbResult<usize> {
    let deleted = conn.execute("DELETE FROM notifications", params![])?;
    Ok(deleted)
}

/// Find a notification by its D-Bus ID (the most recent one)
pub fn get_notification_by_dbus_id(
    conn: &Connection,
    dbus_id: u32,
) -> DbResult<Option<Notification>> {
    let mut stmt = conn.prepare(
        "SELECT id, dbus_id, app_name, app_icon, summary, body, urgency, category,
                desktop_entry, actions, hints, status, expire_timeout, created_at, updated_at,
                closed_reason
         FROM notifications WHERE dbus_id = ?1 ORDER BY created_at DESC LIMIT 1",
    )?;

    let result = stmt.query_row(params![dbus_id], |row| notification_from_row(row));

    match result {
        Ok(notif) => Ok(Some(notif)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Update notification status
pub fn update_status(conn: &Connection, row_id: i64, status: NotificationStatus) -> DbResult<()> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "UPDATE notifications SET status = ?, updated_at = ? WHERE id = ?",
        params![status.as_str(), &now, row_id],
    )?;

    Ok(())
}

/// Update multiple notifications' status
pub fn update_statuses(
    conn: &Connection,
    row_ids: &[i64],
    status: NotificationStatus,
) -> DbResult<()> {
    if row_ids.is_empty() {
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    let placeholders = (0..row_ids.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");

    let query = format!(
        "UPDATE notifications SET status = ?, updated_at = ? WHERE id IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;
    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, status.as_str())?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, &now)?;
    param_idx += 1;

    for row_id in row_ids {
        stmt.raw_bind_parameter(param_idx, row_id)?;
        param_idx += 1;
    }

    stmt.raw_execute()?;
    Ok(())
}

/// Delete notifications by row ID
pub fn delete_notifications(conn: &Connection, row_ids: &[i64]) -> DbResult<()> {
    if row_ids.is_empty() {
        return Ok(());
    }

    let placeholders = (0..row_ids.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");

    let query = format!("DELETE FROM notifications WHERE id IN ({})", placeholders);

    let mut stmt = conn.prepare(&query)?;
    let mut param_idx = 1;

    for row_id in row_ids {
        stmt.raw_bind_parameter(param_idx, row_id)?;
        param_idx += 1;
    }

    stmt.raw_execute()?;
    Ok(())
}

/// Delete old notifications based on retention policy
pub fn cleanup_old(conn: &Connection, max_age_secs: u64, max_count: i64) -> DbResult<i64> {
    let mut deleted = 0i64;

    // Delete by age
    let cutoff = Utc::now() - chrono::Duration::seconds(max_age_secs as i64);
    deleted += conn.execute(
        "DELETE FROM notifications WHERE created_at < ?",
        params![cutoff.to_rfc3339()],
    )? as i64;

    // Delete by count (keep only newest)
    deleted += conn.execute(
        "DELETE FROM notifications WHERE id NOT IN (
            SELECT id FROM notifications ORDER BY created_at DESC LIMIT ?
        )",
        params![max_count],
    )? as i64;

    Ok(deleted)
}

/// Helper: convert a Row to a Notification
fn notification_from_row(row: &Row) -> rusqlite::Result<Notification> {
    let urgency_u32: u32 = row.get(6)?;
    let status_str: String = row.get(11)?;
    let closed_reason_opt: Option<u32> = row.get(15)?;

    let actions_str: String = row.get(9)?;
    let actions = serde_json::from_str(&actions_str).unwrap_or_default();

    let hints_str: String = row.get(10)?;
    let hints = serde_json::from_str(&hints_str).unwrap_or(serde_json::json!({}));

    let created_at_str: String = row.get(13)?;
    let updated_at_str: String = row.get(14)?;

    Ok(Notification {
        row_id: Some(row.get(0)?),
        dbus_id: row.get(1)?,
        app_name: row.get(2)?,
        app_icon: row.get(3)?,
        summary: row.get(4)?,
        body: row.get(5)?,
        urgency: Urgency::from_u8((urgency_u32 & 0xFF) as u8),
        category: row.get(7)?,
        desktop_entry: row.get(8)?,
        actions,
        hints,
        status: NotificationStatus::from_str(&status_str).unwrap_or(NotificationStatus::Unread),
        expire_timeout: row.get(12)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now),
        closed_reason: closed_reason_opt.and_then(|code| match code {
            1 => Some(ClosedReason::Expired),
            2 => Some(ClosedReason::Dismissed),
            3 => Some(ClosedReason::ClosedByCall),
            4 => Some(ClosedReason::Undefined),
            _ => None,
        }),
    })
}
