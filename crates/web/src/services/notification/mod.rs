use std::{collections::HashMap, sync::Arc};

use batlehub_config::schema::{NotificationChannelConfig, NotificationsConfig};
use batlehub_core::ports::NotificationPort;

mod channels;
mod dispatch;

use channels::ChannelDispatcher;

pub(super) const MAX_CONCURRENT_DISPATCHES: usize = 64;

// ── NotificationService ───────────────────────────────────────────────────────

/// Evaluates subscriptions and dispatches outbound notifications.
///
/// Channels are built once from `NotificationsConfig` at startup.
/// A shared `reqwest::Client` is used for all HTTP-based channels.
pub struct NotificationService {
    store: Arc<dyn NotificationPort>,
    channels: HashMap<String, Box<dyn ChannelDispatcher>>,
    /// Tracks in-flight background dispatch tasks for graceful shutdown.
    pending: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Caps the number of concurrently-running dispatch tasks to prevent burst storms.
    dispatch_semaphore: Arc<tokio::sync::Semaphore>,
}

impl NotificationService {
    pub fn new(store: Arc<dyn NotificationPort>, config: &NotificationsConfig) -> Self {
        let client = Arc::new(
            reqwest::Client::builder()
                .user_agent("batlehub-notifications/0.1")
                .build()
                .expect("notification HTTP client"),
        );

        let mut ch: HashMap<String, Box<dyn ChannelDispatcher>> = HashMap::new();
        for cfg in &config.channels {
            let name = cfg.name().to_owned();
            let dispatcher: Box<dyn ChannelDispatcher> = match cfg {
                NotificationChannelConfig::Webhook(c) => {
                    Box::new(channels::build_webhook(c, &client))
                }
                NotificationChannelConfig::Slack(c) => Box::new(channels::build_slack(c, &client)),
                NotificationChannelConfig::Teams(c) => Box::new(channels::build_teams(c, &client)),
                NotificationChannelConfig::Email(c) => match channels::build_email(c) {
                    Ok(d) => Box::new(d),
                    Err(e) => {
                        tracing::error!(channel = %name, "notification: failed to build email dispatcher: {e}");
                        continue;
                    }
                },
            };
            ch.insert(name, dispatcher);
        }

        Self {
            store,
            channels: ch,
            pending: std::sync::Mutex::new(Vec::new()),
            dispatch_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DISPATCHES)),
        }
    }
}

// ── The domain's view of dispatch ────────────────────────────────────────────

/// [`NotificationSink`](batlehub_core::ports::NotificationSink) over a
/// [`NotificationService`]: what a core service — the upstream audit (RFC
/// 0014 §4.5) — is handed so it can report without knowing about channels.
/// Fire-and-forget through `dispatch_event_background`, as the port promises.
pub struct NotificationSinkAdapter(pub Arc<NotificationService>);

impl batlehub_core::ports::NotificationSink for NotificationSinkAdapter {
    fn emit(&self, event: batlehub_core::entities::NotificationEvent) {
        self.0.dispatch_event_background(event);
    }
}

// ── Inbound webhook HMAC verification ────────────────────────────────────────

/// Verify a `X-Hub-Signature-256: sha256=<hex>` header against the request body.
pub fn verify_inbound_hmac(secret: &str, body: &[u8], header_value: &str) -> bool {
    let expected = format!(
        "sha256={}",
        channels::hmac_sha256_hex(secret.as_bytes(), body)
    );
    // Constant-time compare via hmac verify_slice equivalent (manual XOR).
    if expected.len() != header_value.len() {
        return false;
    }
    expected
        .bytes()
        .zip(header_value.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
