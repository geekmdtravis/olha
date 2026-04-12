use chrono::{DateTime, Datelike, Local, Utc};
use serde_json::Value;

/// Format a JSON array of notifications as a compact table for terminal output.
/// Used by `olha list` (default, non-JSON mode).
pub fn format_notification_table(json_str: &str) -> String {
    let notifications: Vec<Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return json_str.to_string(),
    };

    if notifications.is_empty() {
        return "No notifications.".to_string();
    }

    // Column widths
    const ID_W: usize = 5;
    const APP_W: usize = 15;
    const SUMMARY_W: usize = 40;
    const STATUS_W: usize = 8;
    const URGENCY_W: usize = 8;
    const TIME_W: usize = 16;

    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "{:<ID_W$}  {:<APP_W$}  {:<SUMMARY_W$}  {:<STATUS_W$}  {:<URGENCY_W$}  {:<TIME_W$}\n",
        "ID", "App", "Summary", "Status", "Urgency", "Created",
    ));
    output.push_str(&format!(
        "{:<ID_W$}  {:<APP_W$}  {:<SUMMARY_W$}  {:<STATUS_W$}  {:<URGENCY_W$}  {:<TIME_W$}\n",
        "─".repeat(ID_W),
        "─".repeat(APP_W),
        "─".repeat(SUMMARY_W),
        "─".repeat(STATUS_W),
        "─".repeat(URGENCY_W),
        "─".repeat(TIME_W),
    ));

    for notif in &notifications {
        let id = notif
            .get("row_id")
            .and_then(|v| v.as_i64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());

        let app = notif.get("app_name").and_then(|v| v.as_str()).unwrap_or("");

        let summary = notif.get("summary").and_then(|v| v.as_str()).unwrap_or("");

        let status = notif.get("status").and_then(|v| v.as_str()).unwrap_or("");

        let urgency = notif.get("urgency").and_then(|v| v.as_str()).unwrap_or("");

        let created = notif
            .get("created_at")
            .and_then(|v| v.as_str())
            .map(|s| format_timestamp(s))
            .unwrap_or_default();

        output.push_str(&format!(
            "{:<ID_W$}  {:<APP_W$}  {:<SUMMARY_W$}  {:<STATUS_W$}  {:<URGENCY_W$}  {:<TIME_W$}\n",
            truncate(&id, ID_W),
            truncate(app, APP_W),
            truncate(summary, SUMMARY_W),
            truncate(status, STATUS_W),
            truncate(urgency, URGENCY_W),
            truncate(&created, TIME_W),
        ));
    }

    output.push_str(&format!("\n{} notification(s)", notifications.len()));
    output
}

/// Format a single JSON notification as key-value pairs for terminal output.
/// Used by `olha show <id>` (default, non-JSON mode).
pub fn format_notification_detail(json_str: &str) -> String {
    let notif: Value = match serde_json::from_str(json_str) {
        Ok(Value::Null) => return "Notification not found.".to_string(),
        Ok(v) => v,
        Err(_) => return json_str.to_string(),
    };

    let mut output = String::new();
    const LABEL_W: usize = 10;

    if let Some(id) = notif.get("row_id").and_then(|v| v.as_i64()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "ID:", id));
    }

    if let Some(app) = notif.get("app_name").and_then(|v| v.as_str()) {
        if !app.is_empty() {
            output.push_str(&format!("{:<LABEL_W$} {}\n", "App:", app));
        }
    }

    if let Some(summary) = notif.get("summary").and_then(|v| v.as_str()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "Summary:", summary));
    }

    if let Some(body) = notif.get("body").and_then(|v| v.as_str()) {
        if !body.is_empty() {
            output.push_str(&format!("{:<LABEL_W$} {}\n", "Body:", body));
        }
    }

    if let Some(urgency) = notif.get("urgency").and_then(|v| v.as_str()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "Urgency:", urgency));
    }

    if let Some(status) = notif.get("status").and_then(|v| v.as_str()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "Status:", status));
    }

    if let Some(category) = notif.get("category").and_then(|v| v.as_str()) {
        if !category.is_empty() {
            output.push_str(&format!("{:<LABEL_W$} {}\n", "Category:", category));
        }
    }

    if let Some(desktop) = notif.get("desktop_entry").and_then(|v| v.as_str()) {
        if !desktop.is_empty() {
            output.push_str(&format!("{:<LABEL_W$} {}\n", "Desktop:", desktop));
        }
    }

    // Format actions as "key (Label), key (Label)"
    if let Some(actions) = notif.get("actions").and_then(|v| v.as_array()) {
        if !actions.is_empty() {
            let formatted: Vec<String> = actions
                .iter()
                .filter_map(|a| {
                    let id = a.get("id").and_then(|v| v.as_str())?;
                    let label = a.get("label").and_then(|v| v.as_str())?;
                    Some(format!("{} ({})", id, label))
                })
                .collect();
            if !formatted.is_empty() {
                output.push_str(&format!(
                    "{:<LABEL_W$} {}\n",
                    "Actions:",
                    formatted.join(", ")
                ));
            }
        }
    }

    if let Some(created) = notif.get("created_at").and_then(|v| v.as_str()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "Created:", created));
    }

    if let Some(updated) = notif.get("updated_at").and_then(|v| v.as_str()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "Updated:", updated));
    }

    if let Some(dbus_id) = notif.get("dbus_id").and_then(|v| v.as_u64()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "D-Bus ID:", dbus_id));
    }

    if let Some(reason) = notif.get("closed_reason").and_then(|v| v.as_str()) {
        output.push_str(&format!("{:<LABEL_W$} {}\n", "Closed:", reason));
    }

    output
}

/// Truncate a string to `max` characters, appending an ellipsis if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 1 {
        ".".to_string()
    } else {
        let mut truncated = String::with_capacity(max);
        for (i, ch) in s.chars().enumerate() {
            if i >= max - 1 {
                truncated.push('…');
                break;
            }
            truncated.push(ch);
        }
        truncated
    }
}

/// Format an ISO 8601 timestamp into a short, human-readable local time.
/// Shows "HH:MM" for today, "Mon HH:MM" for this week, "Jan 15 HH:MM" otherwise.
fn format_timestamp(iso: &str) -> String {
    let parsed: DateTime<Utc> = match iso.parse() {
        Ok(dt) => dt,
        Err(_) => return iso.to_string(),
    };

    let local: DateTime<Local> = parsed.into();
    let now = Local::now();

    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else if (now - local).num_days() < 7 {
        local.format("%a %H:%M").to_string()
    } else if local.date_naive().year() == now.date_naive().year() {
        local.format("%b %d %H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("ab", 2), "ab");
        assert_eq!(truncate("abc", 2), "a…");
    }

    #[test]
    fn test_table_empty() {
        let output = format_notification_table("[]");
        assert_eq!(output, "No notifications.");
    }

    #[test]
    fn test_table_with_notifications() {
        let json = r#"[
            {
                "row_id": 1,
                "app_name": "Firefox",
                "summary": "Download complete",
                "status": "unread",
                "urgency": "normal",
                "created_at": "2024-01-15T10:30:45Z"
            }
        ]"#;
        let output = format_notification_table(json);
        assert!(output.contains("Firefox"));
        assert!(output.contains("Download complete"));
        assert!(output.contains("unread"));
        assert!(output.contains("1 notification(s)"));
    }

    #[test]
    fn test_detail_not_found() {
        let output = format_notification_detail("null");
        assert_eq!(output, "Notification not found.");
    }

    #[test]
    fn test_detail_with_notification() {
        let json = r#"{
            "row_id": 1,
            "dbus_id": 5,
            "app_name": "Firefox",
            "summary": "Download complete",
            "body": "document.pdf",
            "status": "unread",
            "urgency": "normal",
            "category": "transfer.complete",
            "actions": [{"id": "open", "label": "Open"}],
            "created_at": "2024-01-15T10:30:45Z",
            "updated_at": "2024-01-15T10:30:45Z"
        }"#;
        let output = format_notification_detail(json);
        assert!(output.contains("ID:"));
        assert!(output.contains("Firefox"));
        assert!(output.contains("Download complete"));
        assert!(output.contains("document.pdf"));
        assert!(output.contains("transfer.complete"));
        assert!(output.contains("open (Open)"));
    }

    #[test]
    fn test_detail_with_actions() {
        let json = r#"{
            "row_id": 1,
            "summary": "Test",
            "actions": [
                {"id": "reply", "label": "Reply"},
                {"id": "mark-read", "label": "Mark as Read"}
            ]
        }"#;
        let output = format_notification_detail(json);
        assert!(output.contains("reply (Reply), mark-read (Mark as Read)"));
    }
}
