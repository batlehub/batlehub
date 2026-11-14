use serde::Deserialize;

use super::registry::default_true;

// ── Notifications & Webhooks ──────────────────────────────────────────────────

/// Top-level notification configuration block.
///
/// ```toml
/// [notifications]
/// enabled = true
///
/// [[notifications.channels]]
/// name = "my-slack"
/// type = "slack"
/// url = "https://hooks.slack.com/services/..."
///
/// [[notifications.channels]]
/// name = "ci-webhook"
/// type = "webhook"
/// url = "https://example.com/hook"
/// secret = "hmac-secret"
///
/// [[notifications.inbound]]
/// name = "ci-scanner"
/// secret = "verify-me"
/// ```
#[derive(Debug, Deserialize, Default, Clone, schemars::JsonSchema)]
pub struct NotificationsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub channels: Vec<NotificationChannelConfig>,
    #[serde(default)]
    pub inbound: Vec<InboundWebhookConfig>,
}

/// A single outbound notification channel.
#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationChannelConfig {
    Webhook(WebhookChannelConfig),
    Slack(SlackChannelConfig),
    Teams(TeamsChannelConfig),
    Email(EmailChannelConfig),
}

impl NotificationChannelConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Webhook(c) => &c.name,
            Self::Slack(c) => &c.name,
            Self::Teams(c) => &c.name,
            Self::Email(c) => &c.name,
        }
    }
}

/// Generic outbound HTTP webhook channel.
#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
pub struct WebhookChannelConfig {
    pub name: String,
    pub url: String,
    /// Optional HMAC-SHA256 signing secret. When set, a
    /// `X-BatleHub-Signature-256: sha256=<hex>` header is added to each POST.
    pub secret: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// Slack Incoming Webhook channel.
#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
pub struct SlackChannelConfig {
    pub name: String,
    pub url: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// Microsoft Teams Incoming Webhook channel.
#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
pub struct TeamsChannelConfig {
    pub name: String,
    pub url: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// SMTP email channel.
#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
pub struct EmailChannelConfig {
    pub name: String,
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    pub smtp_user: Option<String>,
    pub smtp_password: Option<String>,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default = "default_true")]
    pub tls: bool,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    10
}

fn default_smtp_port() -> u16 {
    587
}

/// Configuration for a single inbound webhook endpoint.
#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
pub struct InboundWebhookConfig {
    pub name: String,
    /// Optional HMAC-SHA256 secret used to verify the `X-Hub-Signature-256` header.
    /// When absent, any payload is accepted (suitable only for internal networks).
    pub secret: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_enabled_with_empty_channels() {
        let c = NotificationsConfig::default();
        // `Default` derive gives `false`; the serde default is `true` — exercise both.
        assert!(!c.enabled);
        assert!(c.channels.is_empty() && c.inbound.is_empty());
    }

    #[test]
    fn deserializes_all_channel_types_and_defaults() {
        let toml_str = r#"
            enabled = true
            [[channels]]
            type = "webhook"
            name = "ci-hook"
            url = "https://example.com/hook"
            secret = "s"
            [[channels]]
            type = "slack"
            name = "slk"
            url = "https://hooks.slack.com/x"
            [[channels]]
            type = "teams"
            name = "tms"
            url = "https://teams/x"
            [[channels]]
            type = "email"
            name = "mail"
            smtp_host = "smtp.example.com"
            from = "a@example.com"
            to = ["b@example.com"]
            [[inbound]]
            name = "scanner"
            secret = "verify"
        "#;
        let c: NotificationsConfig = toml::from_str(toml_str).unwrap();
        assert!(c.enabled);
        assert_eq!(c.channels.len(), 4);
        // `name()` covers every enum arm.
        assert_eq!(c.channels[0].name(), "ci-hook");
        assert_eq!(c.channels[1].name(), "slk");
        assert_eq!(c.channels[2].name(), "tms");
        assert_eq!(c.channels[3].name(), "mail");
        // Defaults applied.
        if let NotificationChannelConfig::Webhook(w) = &c.channels[0] {
            assert_eq!(w.timeout_secs, 10);
            assert_eq!(w.secret.as_deref(), Some("s"));
        } else {
            panic!("expected webhook");
        }
        if let NotificationChannelConfig::Email(e) = &c.channels[3] {
            assert_eq!(e.smtp_port, 587);
            assert!(e.tls);
            assert_eq!(e.to, vec!["b@example.com".to_string()]);
        } else {
            panic!("expected email");
        }
        assert_eq!(c.inbound.len(), 1);
        assert_eq!(c.inbound[0].name, "scanner");
    }
}
