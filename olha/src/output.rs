use chrono::{DateTime, Datelike, Local, Utc};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
use owo_colors::{OwoColorize, Stream};
use serde_json::Value;

/// Format a JSON array of notifications as a table for terminal output.
/// Used by `olha list` (default, non-JSON mode).
pub fn format_notification_table(json_str: &str) -> String {
    let notifications: Vec<Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return json_str.to_string(),
    };

    if notifications.is_empty() {
        return "No notifications.\n".to_string();
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            header_cell("ID"),
            header_cell("App"),
            header_cell("Summary"),
            header_cell("Status"),
            header_cell("Urgency"),
            header_cell("Created"),
        ]);

    for notif in &notifications {
        let id = notif
            .get("row_id")
            .and_then(|v| v.as_i64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());

        let app = notif.get("app_name").and_then(|v| v.as_str()).unwrap_or("");
        let summary = notif.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let status = notif.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let urgency = notif
            .get("urgency")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        let created = notif
            .get("created_at")
            .and_then(|v| v.as_str())
            .map(format_timestamp)
            .unwrap_or_default();

        table.add_row(vec![
            Cell::new(id).set_alignment(CellAlignment::Right),
            Cell::new(app),
            Cell::new(summary),
            status_cell(status),
            urgency_cell(urgency),
            Cell::new(created),
        ]);
    }

    let count = notifications.len();
    format!("{}\n{} notification(s)\n", table, count)
}

fn header_cell(text: &str) -> Cell {
    Cell::new(text).add_attribute(Attribute::Bold)
}

fn status_cell(status: &str) -> Cell {
    let cell = Cell::new(status);
    match status {
        "unread" => cell.fg(Color::Yellow).add_attribute(Attribute::Bold),
        "read" => cell.add_attribute(Attribute::Dim),
        "cleared" => cell.add_attribute(Attribute::Dim),
        _ => cell,
    }
}

fn urgency_cell(urgency: &str) -> Cell {
    let cell = Cell::new(urgency);
    match urgency {
        "critical" => cell.fg(Color::Red).add_attribute(Attribute::Bold),
        "low" => cell.add_attribute(Attribute::Dim),
        _ => cell,
    }
}

/// Format a single JSON notification as key-value pairs for terminal output.
/// Used by `olha show <id>` (default, non-JSON mode).
pub fn format_notification_detail(json_str: &str) -> String {
    let notif: Value = match serde_json::from_str(json_str) {
        Ok(Value::Null) => return "Notification not found.\n".to_string(),
        Ok(v) => v,
        Err(_) => return json_str.to_string(),
    };

    let mut out = String::new();

    if let Some(id) = notif.get("row_id").and_then(|v| v.as_i64()) {
        push_field(&mut out, "ID", &id.to_string());
    }
    if let Some(app) = notif.get("app_name").and_then(|v| v.as_str()) {
        if !app.is_empty() {
            push_field(&mut out, "App", app);
        }
    }
    if let Some(summary) = notif.get("summary").and_then(|v| v.as_str()) {
        push_field(&mut out, "Summary", summary);
    }
    if let Some(body) = notif.get("body").and_then(|v| v.as_str()) {
        if !body.is_empty() {
            push_field(&mut out, "Body", body);
        }
    }
    if let Some(urgency) = notif.get("urgency").and_then(|v| v.as_str()) {
        push_field(&mut out, "Urgency", &colorize_urgency(urgency));
    }
    if let Some(status) = notif.get("status").and_then(|v| v.as_str()) {
        push_field(&mut out, "Status", &colorize_status(status));
    }
    if let Some(category) = notif.get("category").and_then(|v| v.as_str()) {
        if !category.is_empty() {
            push_field(&mut out, "Category", category);
        }
    }
    if let Some(desktop) = notif.get("desktop_entry").and_then(|v| v.as_str()) {
        if !desktop.is_empty() {
            push_field(&mut out, "Desktop", desktop);
        }
    }

    if let Some(actions) = notif.get("actions").and_then(|v| v.as_array()) {
        let formatted: Vec<(String, String)> = actions
            .iter()
            .filter_map(|a| {
                let id = a.get("id").and_then(|v| v.as_str())?;
                let label = a.get("label").and_then(|v| v.as_str())?;
                Some((id.to_string(), label.to_string()))
            })
            .collect();
        if !formatted.is_empty() {
            push_label(&mut out, "Actions");
            out.push('\n');
            for (id, label) in &formatted {
                out.push_str("  • ");
                out.push_str(id);
                out.push_str(" — ");
                out.push_str(label);
                out.push('\n');
            }
        }
    }

    if let Some(created) = notif.get("created_at").and_then(|v| v.as_str()) {
        push_field(&mut out, "Created", created);
    }
    if let Some(updated) = notif.get("updated_at").and_then(|v| v.as_str()) {
        push_field(&mut out, "Updated", updated);
    }
    if let Some(dbus_id) = notif.get("dbus_id").and_then(|v| v.as_u64()) {
        push_field(&mut out, "D-Bus ID", &dbus_id.to_string());
    }
    if let Some(reason) = notif.get("closed_reason").and_then(|v| v.as_str()) {
        push_field(&mut out, "Closed", reason);
    }

    out
}

const LABEL_WIDTH: usize = 10;

fn push_label(out: &mut String, label: &str) {
    let with_colon = format!("{}:", label);
    let styled = with_colon
        .if_supports_color(Stream::Stdout, |t| t.bold())
        .to_string();
    let pad = LABEL_WIDTH.saturating_sub(with_colon.chars().count());
    out.push_str(&styled);
    for _ in 0..pad {
        out.push(' ');
    }
}

fn push_field(out: &mut String, label: &str, value: &str) {
    push_label(out, label);
    out.push(' ');
    out.push_str(value);
    out.push('\n');
}

fn colorize_status(status: &str) -> String {
    match status {
        "unread" => status
            .if_supports_color(Stream::Stdout, |t| t.yellow().bold().to_string())
            .to_string(),
        "read" | "cleared" => status
            .if_supports_color(Stream::Stdout, |t| t.dimmed().to_string())
            .to_string(),
        _ => status.to_string(),
    }
}

fn colorize_urgency(urgency: &str) -> String {
    match urgency {
        "critical" => urgency
            .if_supports_color(Stream::Stdout, |t| t.red().bold().to_string())
            .to_string(),
        "low" => urgency
            .if_supports_color(Stream::Stdout, |t| t.dimmed().to_string())
            .to_string(),
        _ => urgency.to_string(),
    }
}

/// Truncate a string to `max` characters, appending an ellipsis if truncated.
/// Kept for potential future callers; list rendering now uses comfy-table's wrapping.
#[allow(dead_code)]
fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else if max <= 1 {
        "…".to_string()
    } else {
        let mut truncated: String = s.chars().take(max - 1).collect();
        truncated.push('…');
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
    } else {
        let delta = now - local;
        if delta.num_days() >= 0 && delta.num_days() < 7 {
            local.format("%a %H:%M").to_string()
        } else if local.date_naive().year() == now.date_naive().year() {
            local.format("%b %d %H:%M").to_string()
        } else {
            local.format("%Y-%m-%d %H:%M").to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("ab", 2), "ab");
        assert_eq!(truncate("abc", 2), "a…");
    }

    #[test]
    fn test_truncate_multibyte() {
        // "héllo" is 5 chars but 6 bytes (é is 2 bytes in UTF-8).
        // Must not count bytes.
        assert_eq!(truncate("héllo", 5), "héllo");
        assert_eq!(truncate("héllo", 4), "hél…");
        // "éé" is 2 chars; truncating to 1 forces ellipsis.
        assert_eq!(truncate("éé", 1), "…");
        // CJK chars are 3 bytes each in UTF-8 but one char.
        assert_eq!(truncate("日本語", 3), "日本語");
        assert_eq!(truncate("日本語テスト", 4), "日本語…");
    }

    #[test]
    fn test_table_empty() {
        let output = format_notification_table("[]");
        assert_eq!(output, "No notifications.\n");
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
        // Should use box-drawing chars from the preset.
        assert!(output.contains('─') || output.contains('│'));
    }

    #[test]
    fn test_detail_not_found() {
        let output = format_notification_detail("null");
        assert_eq!(output, "Notification not found.\n");
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
        assert!(output.contains("open"));
        assert!(output.contains("Open"));
    }

    #[test]
    fn test_detail_with_multiple_actions() {
        let json = r#"{
            "row_id": 1,
            "summary": "Test",
            "actions": [
                {"id": "reply", "label": "Reply"},
                {"id": "mark-read", "label": "Mark as Read"}
            ]
        }"#;
        let output = format_notification_detail(json);
        // Both actions should appear; new layout is one per line.
        assert!(output.contains("reply"));
        assert!(output.contains("Reply"));
        assert!(output.contains("mark-read"));
        assert!(output.contains("Mark as Read"));
        // Bulleted rendering.
        assert!(output.contains("•"));
    }
}
