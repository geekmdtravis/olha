use crate::config::NotificationRule;
use crate::notification::Notification;
use regex::Regex;
use std::collections::HashMap;

/// Action to take on a notification that matches a rule
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    /// Clear the notification (remove from popup, mark as cleared)
    Clear,
    /// Ignore the notification (don't store in DB)
    Ignore,
    /// Do nothing — the rule exists only to attach `on_action` handlers
    None,
}

/// Result of evaluating rules against a notification
#[derive(Debug, Clone)]
pub struct RuleResult {
    pub action: Option<RuleAction>,
    pub matching_rule: Option<String>,
}

impl RuleResult {
    pub fn none() -> Self {
        Self {
            action: None,
            matching_rule: None,
        }
    }
}

/// Rule engine for matching notifications against configured rules
pub struct RulesEngine {
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    name: String,
    app_name_regex: Option<Regex>,
    summary_regex: Option<Regex>,
    body_regex: Option<Regex>,
    urgency: Option<u32>,
    category_regex: Option<Regex>,
    action: RuleAction,
    /// action_key -> shell command
    on_action: HashMap<String, String>,
}

impl RulesEngine {
    /// Create a new rules engine from configuration rules
    pub fn new(rules: &[NotificationRule]) -> Result<Self, regex::Error> {
        let mut compiled_rules = Vec::new();

        for rule in rules {
            let action = match rule.action.as_str() {
                "clear" => RuleAction::Clear,
                "ignore" => RuleAction::Ignore,
                "none" => RuleAction::None,
                _ => continue, // skip unknown actions
            };

            let app_name_regex = rule.app_name.as_ref().map(|s| Regex::new(s)).transpose()?;

            let summary_regex = rule.summary.as_ref().map(|s| Regex::new(s)).transpose()?;

            let body_regex = rule.body.as_ref().map(|s| Regex::new(s)).transpose()?;

            let category_regex = rule.category.as_ref().map(|s| Regex::new(s)).transpose()?;

            let urgency = rule.urgency.as_ref().and_then(|u| match u.as_str() {
                "low" => Some(0),
                "normal" => Some(1),
                "critical" => Some(2),
                _ => None,
            });

            compiled_rules.push(CompiledRule {
                name: rule.name.clone(),
                app_name_regex,
                summary_regex,
                body_regex,
                urgency,
                category_regex,
                action,
                on_action: rule.on_action.clone().unwrap_or_default(),
            });
        }

        Ok(Self {
            rules: compiled_rules,
        })
    }

    /// Evaluate rules against a notification
    pub fn evaluate(&self, notif: &Notification) -> RuleResult {
        for rule in &self.rules {
            if self.matches_rule(notif, rule) {
                return RuleResult {
                    action: Some(rule.action),
                    matching_rule: Some(rule.name.clone()),
                };
            }
        }

        RuleResult::none()
    }

    /// Find the shell command to run for `action_key` on `notif`. Walks rules
    /// in order and returns the first match that also has an `on_action` entry
    /// for this key. Returns `(rule_name, command)`.
    pub fn action_command(
        &self,
        notif: &Notification,
        action_key: &str,
    ) -> Option<(String, String)> {
        for rule in &self.rules {
            if !self.matches_rule(notif, rule) {
                continue;
            }
            if let Some(cmd) = rule.on_action.get(action_key) {
                return Some((rule.name.clone(), cmd.clone()));
            }
        }
        None
    }

    fn matches_rule(&self, notif: &Notification, rule: &CompiledRule) -> bool {
        // All specified conditions must match
        if let Some(ref regex) = rule.app_name_regex {
            if !regex.is_match(&notif.app_name) {
                return false;
            }
        }

        if let Some(ref regex) = rule.summary_regex {
            if !regex.is_match(&notif.summary) {
                return false;
            }
        }

        if let Some(ref regex) = rule.body_regex {
            if !regex.is_match(&notif.body) {
                return false;
            }
        }

        if let Some(urgency) = rule.urgency {
            if notif.urgency.as_u32() != urgency {
                return false;
            }
        }

        if let Some(ref regex) = rule.category_regex {
            if !regex.is_match(&notif.category) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification::Urgency;

    fn make_test_notif(app_name: &str, summary: &str, body: &str) -> Notification {
        Notification {
            row_id: None,
            dbus_id: 1,
            app_name: app_name.to_string(),
            app_icon: String::new(),
            summary: summary.to_string(),
            body: body.to_string(),
            urgency: Urgency::Normal,
            category: String::new(),
            desktop_entry: String::new(),
            actions: Vec::new(),
            hints: serde_json::json!({}),
            status: crate::notification::NotificationStatus::Unread,
            expire_timeout: -1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            closed_reason: None,
        }
    }

    fn rule(name: &str, app: &str, action: &str) -> NotificationRule {
        NotificationRule {
            name: name.to_string(),
            app_name: Some(app.to_string()),
            summary: None,
            body: None,
            urgency: None,
            category: None,
            action: action.to_string(),
            on_action: None,
        }
    }

    #[test]
    fn test_rule_matching() {
        let rules = vec![rule("test", "Slack", "clear")];
        let engine = RulesEngine::new(&rules).unwrap();
        let notif = make_test_notif("Slack", "New message", "");

        let result = engine.evaluate(&notif);
        assert_eq!(result.action, Some(RuleAction::Clear));
        assert_eq!(result.matching_rule, Some("test".to_string()));
    }

    #[test]
    fn test_no_match() {
        let rules = vec![rule("test", "Slack", "clear")];
        let engine = RulesEngine::new(&rules).unwrap();
        let notif = make_test_notif("Discord", "New message", "");

        let result = engine.evaluate(&notif);
        assert_eq!(result.action, None);
    }

    #[test]
    fn action_none_variant_is_recognized() {
        let rules = vec![rule("keeper", "Slack", "none")];
        let engine = RulesEngine::new(&rules).unwrap();
        let result = engine.evaluate(&make_test_notif("Slack", "x", ""));
        assert_eq!(result.action, Some(RuleAction::None));
    }

    #[test]
    fn action_command_returns_first_matching_rule() {
        let mut r = rule("focus-signal", "Signal", "none");
        r.on_action = Some(HashMap::from([
            (
                "default".to_string(),
                "signal-desktop --activate".to_string(),
            ),
            ("reply".to_string(), "signal-reply".to_string()),
        ]));
        let engine = RulesEngine::new(&[r]).unwrap();
        let notif = make_test_notif("Signal", "New message", "");

        let cmd = engine.action_command(&notif, "default");
        assert_eq!(
            cmd,
            Some((
                "focus-signal".to_string(),
                "signal-desktop --activate".to_string()
            ))
        );
        let cmd = engine.action_command(&notif, "reply");
        assert_eq!(
            cmd,
            Some(("focus-signal".to_string(), "signal-reply".to_string()))
        );
    }

    #[test]
    fn action_command_returns_none_when_key_absent() {
        let mut r = rule("focus-signal", "Signal", "none");
        r.on_action = Some(HashMap::from([(
            "default".to_string(),
            "signal-desktop --activate".to_string(),
        )]));
        let engine = RulesEngine::new(&[r]).unwrap();
        let notif = make_test_notif("Signal", "x", "");
        assert_eq!(engine.action_command(&notif, "snooze"), None);
    }

    #[test]
    fn action_command_returns_none_when_no_rule_matches() {
        let mut r = rule("focus-signal", "Signal", "none");
        r.on_action = Some(HashMap::from([(
            "default".to_string(),
            "signal-desktop --activate".to_string(),
        )]));
        let engine = RulesEngine::new(&[r]).unwrap();
        let notif = make_test_notif("Discord", "x", "");
        assert_eq!(engine.action_command(&notif, "default"), None);
    }

    #[test]
    fn action_command_respects_rule_order() {
        let mut first = rule("first", "Signal", "none");
        first.on_action = Some(HashMap::from([(
            "default".to_string(),
            "first-cmd".to_string(),
        )]));
        let mut second = rule("second", "Signal", "none");
        second.on_action = Some(HashMap::from([(
            "default".to_string(),
            "second-cmd".to_string(),
        )]));
        let engine = RulesEngine::new(&[first, second]).unwrap();
        let notif = make_test_notif("Signal", "x", "");
        assert_eq!(
            engine.action_command(&notif, "default"),
            Some(("first".to_string(), "first-cmd".to_string()))
        );
    }
}
