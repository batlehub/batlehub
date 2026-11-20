use std::sync::Arc;
use std::time::Duration;

use batlehub_config::schema::{
    EmailChannelConfig, SlackChannelConfig, TeamsChannelConfig, WebhookChannelConfig,
};
use batlehub_core::entities::{NotificationEvent, NotificationEventType};
use lettre::{
    message::header::ContentType, transport::smtp::authentication::Credentials, AsyncSmtpTransport,
    AsyncTransport, Message, Tokio1Executor,
};

// ── Dispatcher trait ──────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub(super) trait ChannelDispatcher: Send + Sync {
    async fn dispatch(&self, event: &NotificationEvent) -> anyhow::Result<()>;
    /// Send a synthetic test event to verify the channel is reachable.
    async fn send_test(&self) -> anyhow::Result<()>;
}

// ── Webhook dispatcher ────────────────────────────────────────────────────────

pub(super) struct WebhookDispatcher {
    pub url: String,
    pub secret: Option<String>,
    pub client: Arc<reqwest::Client>,
    pub timeout: Duration,
}

#[async_trait::async_trait]
impl ChannelDispatcher for WebhookDispatcher {
    async fn dispatch(&self, event: &NotificationEvent) -> anyhow::Result<()> {
        let body = serde_json::to_string(event)?;
        let mut req = self
            .client
            .post(&self.url)
            .timeout(self.timeout)
            .header("Content-Type", "application/json")
            .header("X-BatleHub-Event", event.event_type.as_str());

        if let Some(secret) = &self.secret {
            let sig = hmac_sha256_hex(secret.as_bytes(), body.as_bytes());
            req = req.header("X-BatleHub-Signature-256", format!("sha256={sig}"));
        }

        // `without_url`: a Slack/Teams/webhook URL is the credential, and the
        // error is logged.
        let resp = req
            .body(body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if !resp.status().is_success() {
            anyhow::bail!("webhook returned HTTP {}", resp.status());
        }
        Ok(())
    }

    async fn send_test(&self) -> anyhow::Result<()> {
        let event = test_event();
        self.dispatch(&event).await
    }
}

// ── Slack dispatcher ──────────────────────────────────────────────────────────

pub(super) struct SlackDispatcher {
    pub url: String,
    pub client: Arc<reqwest::Client>,
    pub timeout: Duration,
}

#[async_trait::async_trait]
impl ChannelDispatcher for SlackDispatcher {
    async fn dispatch(&self, event: &NotificationEvent) -> anyhow::Result<()> {
        let text = format_event_text(event);
        let payload = serde_json::json!({
            "text": text,
            "blocks": [{
                "type": "section",
                "text": { "type": "mrkdwn", "text": text }
            }]
        });
        let resp = self
            .client
            .post(&self.url)
            .timeout(self.timeout)
            .json(&payload)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if !resp.status().is_success() {
            anyhow::bail!("slack returned HTTP {}", resp.status());
        }
        Ok(())
    }

    async fn send_test(&self) -> anyhow::Result<()> {
        self.dispatch(&test_event()).await
    }
}

// ── Teams dispatcher ──────────────────────────────────────────────────────────

pub(super) struct TeamsDispatcher {
    pub url: String,
    pub client: Arc<reqwest::Client>,
    pub timeout: Duration,
}

#[async_trait::async_trait]
impl ChannelDispatcher for TeamsDispatcher {
    async fn dispatch(&self, event: &NotificationEvent) -> anyhow::Result<()> {
        let text = format_event_text(event);
        // Teams Adaptive Card (simple MessageCard for compatibility)
        let payload = serde_json::json!({
            "@type": "MessageCard",
            "@context": "https://schema.org/extensions",
            "themeColor": "0076D7",
            "summary": text,
            "sections": [{
                "activityTitle": "BatleHub Notification",
                "activityText": text
            }]
        });
        let resp = self
            .client
            .post(&self.url)
            .timeout(self.timeout)
            .json(&payload)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if !resp.status().is_success() {
            anyhow::bail!("teams returned HTTP {}", resp.status());
        }
        Ok(())
    }

    async fn send_test(&self) -> anyhow::Result<()> {
        self.dispatch(&test_event()).await
    }
}

// ── Email dispatcher ──────────────────────────────────────────────────────────

pub(super) struct EmailDispatcher {
    pub transport: AsyncSmtpTransport<Tokio1Executor>,
    pub from: String,
    pub to: Vec<String>,
}

#[async_trait::async_trait]
impl ChannelDispatcher for EmailDispatcher {
    async fn dispatch(&self, event: &NotificationEvent) -> anyhow::Result<()> {
        let subject = format!(
            "[BatleHub] {} — {}/{}",
            event.event_type, event.registry, event.package_name
        );
        let body = format_event_text(event);

        let from_addr: lettre::message::Mailbox = self.from.parse()?;
        let mut errors: Vec<String> = Vec::new();
        for recipient in &self.to {
            let to_addr = match recipient.parse() {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(recipient, "email: invalid recipient address: {e}");
                    errors.push(format!("{recipient}: {e}"));
                    continue;
                }
            };
            let email = Message::builder()
                .from(from_addr.clone())
                .to(to_addr)
                .subject(&subject)
                .header(ContentType::TEXT_PLAIN)
                .body(body.clone())?;
            if let Err(e) = self.transport.send(email).await {
                tracing::warn!(recipient, "email: send failed: {e}");
                errors.push(format!("{recipient}: {e}"));
                // Break on the first transport failure — remaining sends would also
                // hang until timeout (e.g. SMTP server down), exhausting semaphore slots.
                break;
            }
        }
        if !errors.is_empty() {
            anyhow::bail!(
                "email delivery failed for {} recipient(s): {}",
                errors.len(),
                errors.join("; ")
            );
        }
        Ok(())
    }

    async fn send_test(&self) -> anyhow::Result<()> {
        self.dispatch(&test_event()).await
    }
}

// ── Builder helpers ───────────────────────────────────────────────────────────

pub(super) fn build_webhook(
    cfg: &WebhookChannelConfig,
    client: &Arc<reqwest::Client>,
) -> WebhookDispatcher {
    WebhookDispatcher {
        url: cfg.url.clone(),
        secret: cfg.secret.clone(),
        client: Arc::clone(client),
        timeout: Duration::from_secs(cfg.timeout_secs),
    }
}

pub(super) fn build_slack(
    cfg: &SlackChannelConfig,
    client: &Arc<reqwest::Client>,
) -> SlackDispatcher {
    SlackDispatcher {
        url: cfg.url.clone(),
        client: Arc::clone(client),
        timeout: Duration::from_secs(cfg.timeout_secs),
    }
}

pub(super) fn build_teams(
    cfg: &TeamsChannelConfig,
    client: &Arc<reqwest::Client>,
) -> TeamsDispatcher {
    TeamsDispatcher {
        url: cfg.url.clone(),
        client: Arc::clone(client),
        timeout: Duration::from_secs(cfg.timeout_secs),
    }
}

pub(super) fn build_email(cfg: &EmailChannelConfig) -> anyhow::Result<EmailDispatcher> {
    // tls = true (default) → STARTTLS (port 587); tls = false → plain SMTP
    let transport = if cfg.tls {
        let builder = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)?
            .port(cfg.smtp_port);
        match (&cfg.smtp_user, &cfg.smtp_password) {
            (Some(u), Some(p)) => builder.credentials(Credentials::new(u.clone(), p.clone())),
            _ => builder,
        }
        .build()
    } else {
        let builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.smtp_host)
            .port(cfg.smtp_port);
        match (&cfg.smtp_user, &cfg.smtp_password) {
            (Some(u), Some(p)) => builder.credentials(Credentials::new(u.clone(), p.clone())),
            _ => builder,
        }
        .build()
    };
    Ok(EmailDispatcher {
        transport,
        from: cfg.from.clone(),
        to: cfg.to.clone(),
    })
}

// ── Utility ───────────────────────────────────────────────────────────────────

pub(super) fn format_event_text(event: &NotificationEvent) -> String {
    let version_part = event
        .version
        .as_deref()
        .map(|v| format!(" v{v}"))
        .unwrap_or_default();
    format!(
        "[{}] {}/{}{version_part} — actor: {}",
        event.event_type, event.registry, event.package_name, event.actor
    )
}

pub(super) fn test_event() -> NotificationEvent {
    NotificationEvent::new(
        NotificationEventType::PackagePublished,
        "test-registry",
        "test-package",
        Some("0.0.0".to_owned()),
        "batlehub-test",
    )
}

pub(super) fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(version: Option<&str>) -> NotificationEvent {
        NotificationEvent::new(
            NotificationEventType::PackagePublished,
            "npm-proxy",
            "left-pad",
            version.map(str::to_owned),
            "alice",
        )
    }

    #[test]
    fn format_event_text_includes_version_when_present() {
        let event = sample_event(Some("1.2.3"));
        let text = format_event_text(&event);
        assert!(text.contains("v1.2.3"));
        assert!(text.contains("npm-proxy/left-pad"));
        assert!(text.contains("actor: alice"));
    }

    #[test]
    fn format_event_text_omits_version_when_absent() {
        let event = sample_event(None);
        let text = format_event_text(&event);
        assert!(!text.contains(" v"));
        assert!(text.contains("npm-proxy/left-pad"));
    }

    #[test]
    fn hmac_sha256_hex_matches_known_vector() {
        let mac = hmac_sha256_hex(b"key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            mac,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[tokio::test]
    async fn webhook_dispatch_success_includes_signature_header() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/hook")
            .match_header("x-batlehub-event", "package_published")
            .match_header(
                "x-batlehub-signature-256",
                mockito::Matcher::Regex("^sha256=[0-9a-f]{64}$".to_owned()),
            )
            .with_status(200)
            .create_async()
            .await;

        let dispatcher = WebhookDispatcher {
            url: format!("{}/hook", server.url()),
            secret: Some("s3cret".to_owned()),
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };

        dispatcher
            .dispatch(&sample_event(Some("1.0.0")))
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn webhook_dispatch_5xx_returns_err() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/hook")
            .with_status(500)
            .create_async()
            .await;

        let dispatcher = WebhookDispatcher {
            url: format!("{}/hook", server.url()),
            secret: None,
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };

        let err = dispatcher.dispatch(&sample_event(None)).await.unwrap_err();
        assert!(err.to_string().contains("500"));
    }

    #[tokio::test]
    async fn slack_dispatch_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/slack")
            .with_status(200)
            .create_async()
            .await;

        let dispatcher = SlackDispatcher {
            url: format!("{}/slack", server.url()),
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };
        dispatcher.send_test().await.unwrap();
        mock.assert_async().await;
    }

    /// The webhook URL is the credential; a transport error is logged.
    #[tokio::test]
    async fn a_transport_error_does_not_carry_the_webhook_url() {
        let dispatcher = SlackDispatcher {
            url: "http://127.0.0.1:1/services/T0/B0/hook-secret".to_owned(),
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };
        let err = dispatcher.dispatch(&sample_event(None)).await.unwrap_err();
        assert!(!format!("{err:#}").contains("hook-secret"), "{err:#}");
    }

    #[tokio::test]
    async fn slack_dispatch_5xx_returns_err() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/slack")
            .with_status(503)
            .create_async()
            .await;

        let dispatcher = SlackDispatcher {
            url: format!("{}/slack", server.url()),
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };
        let err = dispatcher.dispatch(&sample_event(None)).await.unwrap_err();
        assert!(err.to_string().contains("503"));
    }

    #[tokio::test]
    async fn teams_dispatch_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/teams")
            .with_status(200)
            .create_async()
            .await;

        let dispatcher = TeamsDispatcher {
            url: format!("{}/teams", server.url()),
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };
        dispatcher.send_test().await.unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn teams_dispatch_5xx_returns_err() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/teams")
            .with_status(502)
            .create_async()
            .await;

        let dispatcher = TeamsDispatcher {
            url: format!("{}/teams", server.url()),
            client: Arc::new(reqwest::Client::new()),
            timeout: Duration::from_secs(5),
        };
        let err = dispatcher.dispatch(&sample_event(None)).await.unwrap_err();
        assert!(err.to_string().contains("502"));
    }

    #[test]
    fn build_webhook_copies_config_fields() {
        let cfg = WebhookChannelConfig {
            name: "wh".to_owned(),
            url: "https://example.com/hook".to_owned(),
            secret: Some("sec".to_owned()),
            timeout_secs: 30,
        };
        let client = Arc::new(reqwest::Client::new());
        let d = build_webhook(&cfg, &client);
        assert_eq!(d.url, "https://example.com/hook");
        assert_eq!(d.secret.as_deref(), Some("sec"));
        assert_eq!(d.timeout, Duration::from_secs(30));
    }

    #[test]
    fn build_slack_and_teams_copy_config_fields() {
        let client = Arc::new(reqwest::Client::new());
        let slack_cfg = SlackChannelConfig {
            name: "slack".to_owned(),
            url: "https://hooks.slack.com/x".to_owned(),
            timeout_secs: 15,
        };
        let slack = build_slack(&slack_cfg, &client);
        assert_eq!(slack.url, "https://hooks.slack.com/x");
        assert_eq!(slack.timeout, Duration::from_secs(15));

        let teams_cfg = TeamsChannelConfig {
            name: "teams".to_owned(),
            url: "https://outlook.office.com/x".to_owned(),
            timeout_secs: 20,
        };
        let teams = build_teams(&teams_cfg, &client);
        assert_eq!(teams.url, "https://outlook.office.com/x");
        assert_eq!(teams.timeout, Duration::from_secs(20));
    }

    fn email_cfg(tls: bool, creds: bool) -> EmailChannelConfig {
        EmailChannelConfig {
            name: "email".to_owned(),
            smtp_host: "localhost".to_owned(),
            smtp_port: 2525,
            smtp_user: creds.then(|| "user".to_owned()),
            smtp_password: creds.then(|| "pass".to_owned()),
            from: "noreply@example.com".to_owned(),
            to: vec!["dev@example.com".to_owned()],
            tls,
            timeout_secs: 1,
        }
    }

    #[test]
    fn build_email_with_tls_disabled_succeeds() {
        let dispatcher = build_email(&email_cfg(false, false)).unwrap();
        assert_eq!(dispatcher.from, "noreply@example.com");
        assert_eq!(dispatcher.to, vec!["dev@example.com".to_owned()]);
    }

    #[test]
    fn build_email_with_tls_and_credentials_succeeds() {
        let dispatcher = build_email(&email_cfg(true, true)).unwrap();
        assert_eq!(dispatcher.from, "noreply@example.com");
    }

    #[tokio::test]
    async fn email_dispatch_with_invalid_recipient_reports_error() {
        let mut cfg = email_cfg(false, false);
        cfg.to = vec!["not-an-email".to_owned()];
        let dispatcher = build_email(&cfg).unwrap();
        let err = dispatcher.dispatch(&sample_event(None)).await.unwrap_err();
        assert!(err.to_string().contains("not-an-email"));
    }
}
