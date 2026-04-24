use crate::db::encryption::{EncryptionContext, FieldTag, ENC_VERSION_CURRENT};
use crate::db::DbResult;
use crate::notification::{ClosedReason, Notification, NotificationStatus, Urgency};
use chrono::Utc;
use rusqlite::{params, Connection, Row};
use tracing;

/// Placeholder shown in place of a decrypted field when the current
/// DEK can't decrypt it (daemon in `--allow-degraded-read` mode, or
/// the row was encrypted under a retired key).
const PLACEHOLDER_ENCRYPTED: &str = "[encrypted]";
const PLACEHOLDER_KEY_UNAVAILABLE: &str = "[key unavailable]";

/// Insert a new notification into the database.
///
/// If `enc` is `Some`, the `summary`, `body`, and `hints` fields are
/// encrypted into the `_enc` BLOB columns and the matching TEXT
/// columns are stored as empty strings. Otherwise they're stored as
/// plaintext (legacy path, `enc_version = 0`).
pub fn insert_notification(
    conn: &Connection,
    notif: &Notification,
    enc: Option<&EncryptionContext>,
) -> DbResult<i64> {
    let now = Utc::now().to_rfc3339();
    let hints_json = notif.hints.to_string();

    match enc {
        Some(ctx) => {
            let summary_enc = ctx
                .encrypt_field(FieldTag::Summary, notif.summary.as_bytes())
                .map_err(encryption_to_db_err)?;
            let body_enc = ctx
                .encrypt_field(FieldTag::Body, notif.body.as_bytes())
                .map_err(encryption_to_db_err)?;
            let hints_enc = ctx
                .encrypt_field(FieldTag::Hints, hints_json.as_bytes())
                .map_err(encryption_to_db_err)?;

            conn.execute(
                "INSERT INTO notifications (
                    dbus_id, app_name, app_icon, summary, body, urgency, category,
                    desktop_entry, actions, hints, status, expire_timeout, created_at, updated_at,
                    summary_enc, body_enc, hints_enc, enc_version, key_id
                 ) VALUES (?1, ?2, ?3, '', '', ?4, ?5, ?6, ?7, '', ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    notif.dbus_id,
                    &notif.app_name,
                    &notif.app_icon,
                    notif.urgency.as_u32(),
                    &notif.category,
                    &notif.desktop_entry,
                    serde_json::to_string(&notif.actions).unwrap_or_default(),
                    notif.status.as_str(),
                    notif.expire_timeout,
                    &notif.created_at.to_rfc3339(),
                    &now,
                    summary_enc,
                    body_enc,
                    hints_enc,
                    ENC_VERSION_CURRENT,
                    ctx.key_id().to_vec(),
                ],
            )?;
        }
        None => {
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
                    hints_json,
                    notif.status.as_str(),
                    notif.expire_timeout,
                    &notif.created_at.to_rfc3339(),
                    &now,
                ],
            )?;
        }
    }

    Ok(conn.last_insert_rowid())
}

/// Fetch a notification by row ID. See [`notification_from_row`] for
/// how encryption is applied at read time.
pub fn get_notification(
    conn: &Connection,
    row_id: i64,
    enc: Option<&EncryptionContext>,
) -> DbResult<Option<Notification>> {
    let query = format!("{} WHERE id = ?1", SELECT_COLUMNS);
    let mut stmt = conn.prepare(&query)?;

    let result = stmt.query_row(params![row_id], |row| Ok(notification_from_row(row, enc)));

    match result {
        Ok(n) => n.map(Some).map_err(Into::into),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Fetch notifications with optional filtering.
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

/// SELECT column list used by every row-returning query, in the order
/// expected by `notification_from_row`. Keeping this in one place means
/// adding a column only touches two spots: the tuple builder and the
/// row decoder.
const SELECT_COLUMNS: &str = "SELECT id, dbus_id, app_name, app_icon, summary, body, urgency, category,
        desktop_entry, actions, hints, status, expire_timeout, created_at, updated_at,
        closed_reason, summary_enc, body_enc, hints_enc, enc_version, key_id
        FROM notifications";

impl NotificationFilter {
    /// Build SQL query and params for filtering.
    ///
    /// When `skip_search_in_sql` is true, the `search` clause is
    /// omitted so the caller can post-filter over decrypted rows in
    /// Rust. `limit` / `offset` are likewise held back in that case
    /// since applying them before post-filter would under-return.
    fn build_query(
        &self,
        skip_search_in_sql: bool,
    ) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut query = format!("{} WHERE 1=1", SELECT_COLUMNS);
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
            if !skip_search_in_sql {
                query.push_str(" AND (summary LIKE ? OR body LIKE ?)");
                let pattern = format!("%{}%", search);
                params.push(Box::new(pattern.clone()));
                params.push(Box::new(pattern));
            }
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

        if !skip_search_in_sql {
            if let Some(limit) = self.limit {
                query.push_str(&format!(" LIMIT {}", limit));
            }

            if let Some(offset) = self.offset {
                query.push_str(&format!(" OFFSET {}", offset));
            }
        }

        (query, params)
    }

    /// Build SQL query for COUNT(*) with same filtering (non-search only).
    fn build_count_query(
        &self,
        skip_search_in_sql: bool,
    ) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
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
            if !skip_search_in_sql {
                query.push_str(" AND (summary LIKE ? OR body LIKE ?)");
                let pattern = format!("%{}%", search);
                params.push(Box::new(pattern.clone()));
                params.push(Box::new(pattern));
            }
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

/// Query notifications with filtering. When encryption is enabled and
/// the filter has a `search` term, the SQL `LIKE` is skipped and the
/// matching is re-run in Rust over decrypted fields.
pub fn query_notifications(
    conn: &Connection,
    filter: &NotificationFilter,
    enc: Option<&EncryptionContext>,
) -> DbResult<Vec<Notification>> {
    let use_post_filter = enc.is_some() && filter.search.is_some();
    let (query, params) = filter.build_query(use_post_filter);

    let mut stmt = conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut rows = stmt.query(param_refs.as_slice())?;

    let mut notifications = Vec::new();
    while let Some(row) = rows.next()? {
        notifications.push(notification_from_row(row, enc)?);
    }

    if use_post_filter {
        if let Some(ref needle) = filter.search {
            let needle_lower = needle.to_lowercase();
            notifications.retain(|n| {
                n.summary.to_lowercase().contains(&needle_lower)
                    || n.body.to_lowercase().contains(&needle_lower)
            });
            if let Some(offset) = filter.offset {
                let off = offset.max(0) as usize;
                if off >= notifications.len() {
                    notifications.clear();
                } else {
                    notifications.drain(0..off);
                }
            }
            if let Some(limit) = filter.limit {
                notifications.truncate(limit.max(0) as usize);
            }
        }
    }

    Ok(notifications)
}

/// Count notifications with filtering. Uses the same post-filter path
/// as `query_notifications` when encryption + search are combined.
pub fn count_notifications(
    conn: &Connection,
    filter: &NotificationFilter,
    enc: Option<&EncryptionContext>,
) -> DbResult<i64> {
    let use_post_filter = enc.is_some() && filter.search.is_some();

    if use_post_filter {
        // Re-use the full query path so search matches whatever a
        // subsequent list() call will produce.
        let notifications = query_notifications(
            conn,
            &NotificationFilter {
                limit: None,
                offset: None,
                ..filter.clone()
            },
            enc,
        )?;
        return Ok(notifications.len() as i64);
    }

    let (query, params) = filter.build_count_query(false);

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
    enc: Option<&EncryptionContext>,
) -> DbResult<Option<Notification>> {
    let query = format!("{} WHERE dbus_id = ?1 ORDER BY created_at DESC LIMIT 1", SELECT_COLUMNS);
    let mut stmt = conn.prepare(&query)?;

    let result = stmt.query_row(params![dbus_id], |row| Ok(notification_from_row(row, enc)));

    match result {
        Ok(n) => n.map(Some).map_err(Into::into),
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

/// Helper: convert a Row to a Notification. When `enc_version > 0`,
/// the three sensitive fields come from the `_enc` BLOBs and are
/// decrypted in place. If decryption is impossible (daemon has no
/// DEK, or the stored row was encrypted under a different key),
/// placeholder strings are substituted so callers never see an error
/// for a single unreadable row.
fn notification_from_row(
    row: &Row,
    enc: Option<&EncryptionContext>,
) -> rusqlite::Result<Notification> {
    let urgency_u32: u32 = row.get(6)?;
    let status_str: String = row.get(11)?;
    let closed_reason_opt: Option<u32> = row.get(15)?;

    let actions_str: String = row.get(9)?;
    let actions = serde_json::from_str(&actions_str).unwrap_or_default();

    let created_at_str: String = row.get(13)?;
    let updated_at_str: String = row.get(14)?;

    let enc_version: i64 = row.get(19)?;

    let (summary, body, hints) = if enc_version == 0 {
        let summary: String = row.get(4)?;
        let body: String = row.get(5)?;
        let hints_str: String = row.get(10)?;
        let hints = serde_json::from_str(&hints_str).unwrap_or(serde_json::json!({}));
        (summary, body, hints)
    } else {
        let row_id_preview: i64 = row.get(0)?;
        let summary_enc: Option<Vec<u8>> = row.get(16)?;
        let body_enc: Option<Vec<u8>> = row.get(17)?;
        let hints_enc: Option<Vec<u8>> = row.get(18)?;
        let stored_key_id: Option<Vec<u8>> = row.get(20)?;

        let key_matches = enc.map_or(false, |ctx| {
            stored_key_id.as_deref().map_or(false, |k| k == ctx.key_id())
        });

        if !key_matches {
            // Either daemon has no DEK, or the row was encrypted under
            // a different key (pre-rotation, or --allow-degraded-read).
            // Serve placeholders rather than erroring the whole query.
            let reason = if enc.is_none() {
                PLACEHOLDER_KEY_UNAVAILABLE
            } else {
                tracing::warn!(
                    "row {} encrypted under unknown key_id={:?}",
                    row_id_preview,
                    stored_key_id
                );
                PLACEHOLDER_ENCRYPTED
            };
            (
                PLACEHOLDER_ENCRYPTED.to_string(),
                reason.to_string(),
                serde_json::json!({}),
            )
        } else {
            let ctx = enc.expect("key_matches implied enc is Some");
            let summary = decrypt_text(ctx, FieldTag::Summary, summary_enc.as_deref());
            let body = decrypt_text(ctx, FieldTag::Body, body_enc.as_deref());
            let hints = match hints_enc.as_deref() {
                Some(blob) => match ctx.decrypt_field(FieldTag::Hints, blob) {
                    Ok(bytes) => serde_json::from_slice(&bytes)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    Err(e) => {
                        tracing::warn!("hints decrypt failed for row {}: {}", row_id_preview, e);
                        serde_json::json!({})
                    }
                },
                None => serde_json::json!({}),
            };
            (summary, body, hints)
        }
    };

    Ok(Notification {
        row_id: Some(row.get(0)?),
        dbus_id: row.get(1)?,
        app_name: row.get(2)?,
        app_icon: row.get(3)?,
        summary,
        body,
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

fn decrypt_text(ctx: &EncryptionContext, field: FieldTag, blob: Option<&[u8]>) -> String {
    match blob {
        Some(b) => match ctx.decrypt_field(field, b) {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| PLACEHOLDER_ENCRYPTED.into()),
            Err(_) => PLACEHOLDER_ENCRYPTED.into(),
        },
        None => String::new(),
    }
}

fn encryption_to_db_err(e: crate::db::encryption::EncryptionError) -> crate::db::DbError {
    crate::db::DbError::Encryption(e.to_string())
}

/// Read a value from the `meta` key-value table. Returns `None` if the
/// key is absent. Used for small daemon-level state (DND toggle, etc.)
/// that must survive restarts but isn't per-notification data.
pub fn get_meta(conn: &Connection, key: &str) -> DbResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    match stmt.query_row(params![key], |row| row.get::<_, String>(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Upsert a value into the `meta` key-value table.
pub fn set_meta(conn: &Connection, key: &str, value: &str) -> DbResult<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::encryption::{EncryptionContext, DEK_LEN};
    use crate::db::schema::init_schema;
    use crate::notification::{NotificationStatus, Urgency};
    use chrono::Utc;
    use zeroize::Zeroizing;

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn test_ctx() -> EncryptionContext {
        EncryptionContext::from_dek(Zeroizing::new([0x42u8; DEK_LEN]))
    }

    fn sample_notif(dbus_id: u32, summary: &str, body: &str) -> Notification {
        let now = Utc::now();
        Notification {
            row_id: None,
            dbus_id,
            app_name: "testapp".into(),
            app_icon: String::new(),
            summary: summary.into(),
            body: body.into(),
            urgency: Urgency::Normal,
            category: String::new(),
            desktop_entry: String::new(),
            actions: vec![],
            hints: serde_json::json!({"foo": "bar"}),
            status: NotificationStatus::Unread,
            expire_timeout: -1,
            created_at: now,
            updated_at: now,
            closed_reason: None,
        }
    }

    #[test]
    fn plaintext_roundtrip() {
        let conn = fresh_conn();
        let notif = sample_notif(1, "hi", "plaintext body");
        let row_id = insert_notification(&conn, &notif, None).unwrap();
        let fetched = get_notification(&conn, row_id, None).unwrap().unwrap();
        assert_eq!(fetched.summary, "hi");
        assert_eq!(fetched.body, "plaintext body");
        assert_eq!(fetched.hints, serde_json::json!({"foo": "bar"}));
    }

    #[test]
    fn encrypted_roundtrip() {
        let conn = fresh_conn();
        let ctx = test_ctx();
        let notif = sample_notif(2, "secret", "s3cr3t body");
        let row_id = insert_notification(&conn, &notif, Some(&ctx)).unwrap();

        // Raw DB should show empty TEXT cols and populated BLOBs
        let (enc_version, summary_text, body_blob_len): (i64, String, i64) = conn
            .query_row(
                "SELECT enc_version, summary, length(body_enc) FROM notifications WHERE id=?",
                [row_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(enc_version, ENC_VERSION_CURRENT);
        assert_eq!(summary_text, "");
        assert!(body_blob_len > 0);

        let fetched = get_notification(&conn, row_id, Some(&ctx)).unwrap().unwrap();
        assert_eq!(fetched.summary, "secret");
        assert_eq!(fetched.body, "s3cr3t body");
        assert_eq!(fetched.hints, serde_json::json!({"foo": "bar"}));
    }

    #[test]
    fn read_without_key_returns_placeholder() {
        let conn = fresh_conn();
        let ctx = test_ctx();
        let notif = sample_notif(3, "hidden", "hidden body");
        let row_id = insert_notification(&conn, &notif, Some(&ctx)).unwrap();

        let fetched = get_notification(&conn, row_id, None).unwrap().unwrap();
        assert_eq!(fetched.summary, PLACEHOLDER_ENCRYPTED);
        assert_eq!(fetched.body, PLACEHOLDER_KEY_UNAVAILABLE);
    }

    #[test]
    fn read_with_wrong_key_returns_placeholder() {
        let conn = fresh_conn();
        let ctx1 = EncryptionContext::from_dek(Zeroizing::new([0x01u8; DEK_LEN]));
        let ctx2 = EncryptionContext::from_dek(Zeroizing::new([0x02u8; DEK_LEN]));
        let notif = sample_notif(4, "hidden", "hidden body");
        let row_id = insert_notification(&conn, &notif, Some(&ctx1)).unwrap();

        let fetched = get_notification(&conn, row_id, Some(&ctx2)).unwrap().unwrap();
        assert_eq!(fetched.summary, PLACEHOLDER_ENCRYPTED);
    }

    #[test]
    fn mixed_rows_coexist() {
        let conn = fresh_conn();
        let ctx = test_ctx();
        let plain = sample_notif(10, "plain-one", "p-body");
        let enc = sample_notif(11, "enc-one", "e-body");
        insert_notification(&conn, &plain, None).unwrap();
        insert_notification(&conn, &enc, Some(&ctx)).unwrap();

        let all = query_notifications(&conn, &NotificationFilter::default(), Some(&ctx)).unwrap();
        assert_eq!(all.len(), 2);
        let bodies: Vec<String> = all.iter().map(|n| n.body.clone()).collect();
        assert!(bodies.contains(&"p-body".to_string()));
        assert!(bodies.contains(&"e-body".to_string()));
    }

    #[test]
    fn search_post_filters_encrypted_rows() {
        let conn = fresh_conn();
        let ctx = test_ctx();
        insert_notification(&conn, &sample_notif(20, "alpha meeting", "x"), Some(&ctx)).unwrap();
        insert_notification(&conn, &sample_notif(21, "beta call", "y"), Some(&ctx)).unwrap();
        insert_notification(&conn, &sample_notif(22, "meeting notes", "ok"), Some(&ctx)).unwrap();

        let filter = NotificationFilter {
            search: Some("meeting".into()),
            ..Default::default()
        };
        let hits = query_notifications(&conn, &filter, Some(&ctx)).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(count_notifications(&conn, &filter, Some(&ctx)).unwrap(), 2);
    }

    #[test]
    fn post_filter_respects_limit_and_offset() {
        let conn = fresh_conn();
        let ctx = test_ctx();
        for i in 0..5 {
            insert_notification(
                &conn,
                &sample_notif(100 + i, &format!("match {}", i), "body"),
                Some(&ctx),
            )
            .unwrap();
        }

        let filter = NotificationFilter {
            search: Some("match".into()),
            limit: Some(2),
            offset: Some(1),
            ..Default::default()
        };
        let hits = query_notifications(&conn, &filter, Some(&ctx)).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn meta_kv_roundtrip() {
        let conn = fresh_conn();
        assert_eq!(get_meta(&conn, "dnd_enabled").unwrap(), None);

        set_meta(&conn, "dnd_enabled", "true").unwrap();
        assert_eq!(get_meta(&conn, "dnd_enabled").unwrap().as_deref(), Some("true"));

        // Upsert updates in place instead of duplicating rows.
        set_meta(&conn, "dnd_enabled", "false").unwrap();
        assert_eq!(get_meta(&conn, "dnd_enabled").unwrap().as_deref(), Some("false"));

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM meta WHERE key='dnd_enabled'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn enc_version_counts_via_sql() {
        let conn = fresh_conn();
        let ctx = test_ctx();
        insert_notification(&conn, &sample_notif(30, "p", "p"), None).unwrap();
        insert_notification(&conn, &sample_notif(31, "e", "e"), Some(&ctx)).unwrap();
        insert_notification(&conn, &sample_notif(32, "e2", "e2"), Some(&ctx)).unwrap();
        let plain: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE enc_version = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let enc: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE enc_version > 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(plain, 1);
        assert_eq!(enc, 2);
    }
}
