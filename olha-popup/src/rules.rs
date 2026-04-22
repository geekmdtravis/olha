use regex::Regex;

use crate::config::PopupRule;
use crate::model::{Notification, Urgency};

/// Outcome of evaluating popup rules against an incoming notification.
#[derive(Debug, Clone, Default)]
pub struct RuleDecision {
    pub suppress: bool,
    pub override_urgency: Option<Urgency>,
    pub override_timeout_secs: Option<u32>,
    pub matched: Option<String>,
}

struct Compiled {
    name: String,
    app_name: Option<Regex>,
    summary: Option<Regex>,
    body: Option<Regex>,
    urgency: Option<Urgency>,
    suppress: bool,
    override_urgency: Option<Urgency>,
    override_timeout_secs: Option<u32>,
}

pub struct PopupRules {
    rules: Vec<Compiled>,
}

impl PopupRules {
    pub fn new(rules: &[PopupRule]) -> Self {
        let mut out = Vec::with_capacity(rules.len());
        for r in rules {
            match compile(r) {
                Ok(c) => out.push(c),
                Err(e) => {
                    tracing::warn!(
                        "skipping popup rule {:?}: {e}",
                        if r.name.is_empty() { "(unnamed)" } else { &r.name }
                    );
                }
            }
        }
        Self { rules: out }
    }

    pub fn evaluate(&self, notif: &Notification) -> RuleDecision {
        for rule in &self.rules {
            if !matches(notif, rule) {
                continue;
            }
            return RuleDecision {
                suppress: rule.suppress,
                override_urgency: rule.override_urgency,
                override_timeout_secs: rule.override_timeout_secs,
                matched: Some(rule.name.clone()),
            };
        }
        RuleDecision::default()
    }
}

fn compile(r: &PopupRule) -> Result<Compiled, regex::Error> {
    Ok(Compiled {
        name: if r.name.is_empty() {
            "(unnamed)".to_string()
        } else {
            r.name.clone()
        },
        app_name: r.app_name.as_deref().map(Regex::new).transpose()?,
        summary: r.summary.as_deref().map(Regex::new).transpose()?,
        body: r.body.as_deref().map(Regex::new).transpose()?,
        urgency: r.urgency,
        suppress: r.suppress,
        override_urgency: r.override_urgency,
        override_timeout_secs: r.override_timeout_secs,
    })
}

fn matches(notif: &Notification, rule: &Compiled) -> bool {
    if let Some(re) = &rule.app_name {
        if !re.is_match(&notif.app_name) {
            return false;
        }
    }
    if let Some(re) = &rule.summary {
        if !re.is_match(&notif.summary) {
            return false;
        }
    }
    if let Some(re) = &rule.body {
        if !re.is_match(&notif.body) {
            return false;
        }
    }
    if let Some(u) = rule.urgency {
        if notif.urgency != u {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn notif(app: &str, summary: &str, body: &str, urgency: Urgency) -> Notification {
        Notification {
            row_id: None,
            dbus_id: 0,
            app_name: app.into(),
            summary: summary.into(),
            body: body.into(),
            urgency,
            actions: Vec::new(),
        }
    }

    /// Parse `[[rule]]` blocks into `Vec<PopupRule>` exactly the way
    /// `AppConfig` does when it reads `~/.config/olha/config.toml`. Tests
    /// written against this helper catch TOML-level escape-sequence
    /// surprises, not just direct Rust string construction.
    fn rules_from_toml(src: &str) -> Vec<PopupRule> {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            rule: Vec<PopupRule>,
        }
        let w: Wrap = toml::from_str(src).expect("test TOML must parse");
        w.rule
    }

    fn build(src: &str) -> PopupRules {
        PopupRules::new(&rules_from_toml(src))
    }

    // ---- matching semantics ----

    #[test]
    fn is_match_is_unanchored_substring() {
        // "Slack" matches anywhere in the app_name string — use ^ / $ to
        // anchor if you only want an exact match.
        let rules = build(
            r#"
            [[rule]]
            name = "slack-unanchored"
            app_name = "Slack"
            suppress = true
            "#,
        );
        assert!(rules.evaluate(&notif("Slack", "", "", Urgency::Normal)).suppress);
        assert!(rules.evaluate(&notif("Slack Desktop", "", "", Urgency::Normal)).suppress);
        assert!(rules.evaluate(&notif("slackware", "hi", "", Urgency::Normal)).suppress == false);
    }

    #[test]
    fn anchors_force_exact_match() {
        let rules = build(
            r#"
            [[rule]]
            name = "teams-exact"
            app_name = "^Microsoft Teams$"
            override_urgency = "normal"
            "#,
        );
        let hit = rules.evaluate(&notif("Microsoft Teams", "", "", Urgency::Critical));
        assert_eq!(hit.override_urgency, Some(Urgency::Normal));
        let miss = rules.evaluate(&notif("Microsoft Teams Classic", "", "", Urgency::Critical));
        assert_eq!(miss.override_urgency, None);
    }

    #[test]
    fn dot_is_any_char_unless_escaped_with_literal_string() {
        // Literal single-quoted TOML string ⇒ backslash is passed through
        // to the regex engine as-is. This is the recommended form.
        let literal = build(
            r#"
            [[rule]]
            name = "literal-dot"
            summary = '^v1\.0$'
            suppress = true
            "#,
        );
        assert!(literal.evaluate(&notif("", "v1.0", "", Urgency::Normal)).suppress);
        assert!(!literal.evaluate(&notif("", "v1x0", "", Urgency::Normal)).suppress);

        // Same pattern written as a TOML basic (double-quoted) string needs
        // the backslash doubled — TOML eats one layer, regex eats the
        // other. Easy to get wrong; prefer single quotes.
        let basic = build(
            r#"
            [[rule]]
            name = "basic-dot"
            summary = "^v1\\.0$"
            suppress = true
            "#,
        );
        assert!(basic.evaluate(&notif("", "v1.0", "", Urgency::Normal)).suppress);
        assert!(!basic.evaluate(&notif("", "v1x0", "", Urgency::Normal)).suppress);
    }

    #[test]
    fn case_sensitive_by_default_inline_flag_makes_it_insensitive() {
        let sensitive = build(
            r#"
            [[rule]]
            name = "case"
            app_name = "slack"
            suppress = true
            "#,
        );
        assert!(!sensitive.evaluate(&notif("Slack", "", "", Urgency::Normal)).suppress);

        let insensitive = build(
            r#"
            [[rule]]
            name = "case-i"
            app_name = "(?i)slack"
            suppress = true
            "#,
        );
        assert!(insensitive.evaluate(&notif("Slack", "", "", Urgency::Normal)).suppress);
    }

    #[test]
    fn multiple_fields_are_anded() {
        let rules = build(
            r#"
            [[rule]]
            name = "and"
            app_name = "^Slack$"
            summary = "^Thread:"
            suppress = true
            "#,
        );
        // Both match.
        assert!(rules.evaluate(&notif("Slack", "Thread: foo", "", Urgency::Normal)).suppress);
        // Only app_name matches.
        assert!(!rules.evaluate(&notif("Slack", "DM", "", Urgency::Normal)).suppress);
        // Only summary matches.
        assert!(!rules.evaluate(&notif("Discord", "Thread: foo", "", Urgency::Normal)).suppress);
    }

    #[test]
    fn urgency_field_is_exact_not_regex() {
        let rules = build(
            r#"
            [[rule]]
            name = "critical-only"
            urgency = "critical"
            override_timeout_secs = 5
            "#,
        );
        assert_eq!(
            rules.evaluate(&notif("X", "", "", Urgency::Critical)).override_timeout_secs,
            Some(5),
        );
        assert_eq!(
            rules.evaluate(&notif("X", "", "", Urgency::Normal)).override_timeout_secs,
            None,
        );
    }

    #[test]
    fn first_matching_rule_wins() {
        let rules = build(
            r#"
            [[rule]]
            name = "first"
            app_name = "Teams"
            override_urgency = "normal"

            [[rule]]
            name = "second"
            app_name = "Teams"
            suppress = true
            "#,
        );
        let d = rules.evaluate(&notif("Microsoft Teams", "", "", Urgency::Critical));
        assert_eq!(d.matched.as_deref(), Some("first"));
        assert_eq!(d.override_urgency, Some(Urgency::Normal));
        assert!(!d.suppress);
    }

    #[test]
    fn no_match_returns_default_decision() {
        let rules = build(
            r#"
            [[rule]]
            name = "only-teams"
            app_name = "^Microsoft Teams$"
            suppress = true
            "#,
        );
        let d = rules.evaluate(&notif("Firefox", "", "", Urgency::Normal));
        assert!(!d.suppress);
        assert_eq!(d.override_urgency, None);
        assert_eq!(d.override_timeout_secs, None);
        assert_eq!(d.matched, None);
    }

    #[test]
    fn rule_with_no_match_fields_matches_everything() {
        // A rule that only declares an action acts as a catch-all and fires
        // against the first notification it sees — worth verifying so
        // users aren't surprised if they forget to add match fields.
        let rules = build(
            r#"
            [[rule]]
            name = "catch-all"
            suppress = true
            "#,
        );
        assert!(rules.evaluate(&notif("Anything", "At all", "", Urgency::Low)).suppress);
    }

    // ---- action propagation ----

    #[test]
    fn override_urgency_and_timeout_both_propagate() {
        let rules = build(
            r#"
            [[rule]]
            name = "demote-and-timeout"
            app_name = "^Teams$"
            override_urgency = "normal"
            override_timeout_secs = 8
            "#,
        );
        let d = rules.evaluate(&notif("Teams", "", "", Urgency::Critical));
        assert_eq!(d.override_urgency, Some(Urgency::Normal));
        assert_eq!(d.override_timeout_secs, Some(8));
        assert!(!d.suppress);
    }

    #[test]
    fn suppress_propagates() {
        let rules = build(
            r#"
            [[rule]]
            name = "hide"
            app_name = "^Spotify$"
            suppress = true
            "#,
        );
        assert!(rules.evaluate(&notif("Spotify", "", "", Urgency::Low)).suppress);
    }

    // ---- compile errors ----

    #[test]
    fn invalid_regex_is_skipped_engine_keeps_working() {
        // Rule 0 has an unclosed group — it must be dropped without
        // crashing, and rule 1 must still fire.
        let rules = build(
            r#"
            [[rule]]
            name = "broken"
            app_name = "("
            suppress = true

            [[rule]]
            name = "good"
            app_name = "^Firefox$"
            override_urgency = "low"
            "#,
        );
        let d = rules.evaluate(&notif("Firefox", "", "", Urgency::Normal));
        assert_eq!(d.matched.as_deref(), Some("good"));
        assert_eq!(d.override_urgency, Some(Urgency::Low));
    }
}
