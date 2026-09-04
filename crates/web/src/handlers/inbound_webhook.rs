use std::sync::Arc;

use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use batlehub_config::schema::NotificationsConfig;
use batlehub_core::entities::{
    FlagEffect, FlagKind, FlagPush, InboundWebhookEvent, PackageId, ScanTrigger, Severity,
};
use batlehub_core::services::{FlagService, FlagSourceLimits};
use bytes::BytesMut;
use chrono::Utc;
use futures::StreamExt;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError, extractors::AuthIdentity, handlers::schemas::OkResponse,
    services::verify_inbound_hmac,
};
use batlehub_core::ports::NotificationPort;

// ── Inbound webhook receiver ──────────────────────────────────────────────────

/// Receive an event from an external system.
///
/// If a `secret` is configured for this webhook, the request must include a
/// `X-Hub-Signature-256: sha256=<hmac-hex>` header computed over the raw body.
#[utoipa::path(
    post,
    path = "/api/v1/webhooks/inbound/{name}",
    tag = "notifications",
    params(("name" = String, Path, description = "Inbound webhook name")),
    responses(
        (status = 200, description = "Event received", body = OkResponse),
        (status = 400, description = "Unknown webhook name"),
        (status = 401, description = "HMAC signature mismatch"),
    ),
)]
#[post("/api/v1/webhooks/inbound/{name}")]
pub async fn receive_inbound_webhook(
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
    notification_store: web::Data<Arc<dyn NotificationPort>>,
    notifications_config: web::Data<Option<NotificationsConfig>>,
    flags: Option<web::Data<Arc<FlagService>>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let name = path.into_inner();

    // Validate name and enabled flag BEFORE reading the body so that large
    // payloads to unknown/disabled webhook names are rejected cheaply.
    let inbound_cfg = notifications_config
        .as_ref()
        .as_ref()
        .filter(|nc| nc.enabled)
        .and_then(|nc| nc.inbound.iter().find(|i| i.name == name));

    let inbound_cfg = match inbound_cfg {
        Some(cfg) => cfg,
        None => {
            // Be vague about whether it exists to avoid enumeration.
            return Err(AppError::bad_request(format!(
                "unknown inbound webhook: {name}"
            )));
        }
    };

    // Inbound webhook payloads are small JSON events; bound the raw `Payload`
    // accumulation so an unauthenticated caller cannot stream an unbounded body
    // into memory (OOM) before the HMAC check even runs.
    const MAX_WEBHOOK_BYTES: u64 = 5 * 1024 * 1024;
    let mut raw = BytesMut::new();
    let mut total: u64 = 0;
    while let Some(chunk) = payload.next().await {
        let chunk = chunk.map_err(|e| AppError::bad_request(e.to_string()))?;
        total += chunk.len() as u64;
        if total > MAX_WEBHOOK_BYTES {
            return Err(AppError::bad_request(format!(
                "webhook payload exceeds the {MAX_WEBHOOK_BYTES}-byte limit"
            )));
        }
        raw.extend_from_slice(&chunk);
    }
    let body = raw.freeze();

    // Empty secrets provide no security (attacker can compute HMAC with empty key),
    // so treat them the same as no secret configured.
    let signature_valid = match inbound_cfg.secret.as_deref().filter(|s| !s.is_empty()) {
        Some(secret) => {
            let header_val = req
                .headers()
                .get("X-Hub-Signature-256")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !verify_inbound_hmac(secret, &body, header_val) {
                return Err(AppError::forbidden("HMAC signature mismatch"));
            }
            Some(true)
        }
        None => None,
    };

    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("request body is not valid JSON: {e}")))?;

    // `connection_info().realip_remote_addr()` believes `X-Forwarded-For` from
    // any peer, which would let an unauthenticated caller write whatever source
    // address it liked into the audit record — the one field an operator reads to
    // find out who sent an unsigned event. Route through the same proxy-trust
    // verdict as every other IP-consuming code path so they cannot disagree.
    let source_ip = Some(crate::middleware::proxy_trust::client_ip(
        &req,
        crate::middleware::proxy_trust::peer_trust(&req),
    ));

    let event = InboundWebhookEvent {
        id: Uuid::new_v4(),
        webhook_name: name.clone(),
        payload: payload.clone(),
        source_ip,
        received_at: Utc::now(),
        signature_valid,
    };

    notification_store.record_inbound_event(event).await?;

    // RFC 0018 §4.3's two security events, on a signed webhook only. Unsigned
    // ones are recorded above and do nothing: validation already refuses an
    // unsigned inbound webhook once a registry has `[security]`, and this is
    // the second line — a `security.verdict` that could deny a package must
    // come from someone holding the secret.
    if signature_valid == Some(true) {
        handle_security_event(&name, &payload, flags.as_ref().map(|d| d.get_ref()), &hot).await;
    }

    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// `security.verdict` and `security.rescan` (RFC 0018 §4.3, recast by RFC
/// 0002 §13 decision 2): the verdict event is an alias for a `hard_block`
/// flag pushed under the webhook's name, so it lands in `package_flags` and
/// the exposure report sees it; the rescan event enqueues its coordinates
/// at `Webhook` priority.
async fn handle_security_event(
    webhook: &str,
    payload: &serde_json::Value,
    flags: Option<&Arc<FlagService>>,
    hot: &batlehub_core::services::hot_config::HotConfigLock,
) {
    let kind = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match kind {
        "security.verdict" => {
            let Some(flags) = flags else { return };
            let Some((registry, name, version)) = payload
                .get("coordinate")
                .and_then(|c| c.as_str())
                .and_then(split_coordinate)
            else {
                tracing::warn!(
                    webhook,
                    "security.verdict without a `registry:name@version` coordinate"
                );
                return;
            };
            let summary = payload
                .get("summary")
                .and_then(|s| s.as_str())
                .unwrap_or("SOC verdict")
                .to_owned();
            let external_id = payload
                .get("external_id")
                .and_then(|s| s.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{registry}:{name}@{version}"));
            let push = FlagPush {
                external_id,
                registry,
                package_name: name,
                version: Some(version),
                version_range: None,
                kind: payload
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .map(FlagKind::parse)
                    .unwrap_or(FlagKind::Malware),
                effect: FlagEffect::HardBlock,
                severity: payload
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .and_then(Severity::parse),
                summary,
                url: payload
                    .get("url")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned),
                expires_at: payload
                    .get("expires_at")
                    .and_then(|s| s.as_str())
                    .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok()),
            };
            let source = FlagSourceLimits {
                name: webhook.to_owned(),
                max_effect: FlagEffect::HardBlock,
                registries: Vec::new(),
                max_flags_per_minute: 0,
            };
            match flags.push(&source, vec![push], Utc::now()).await {
                Ok(out) => {
                    tracing::info!(webhook, accepted = out.accepted, rejected = out.rejected,
                    outcome = ?out.items.first(), "security.verdict recorded as a flag")
                }
                Err(e) => tracing::warn!(webhook, error = %e, "security.verdict not recorded"),
            }
        }
        "security.rescan" => {
            let queue = { hot.read().await.scan_queue.clone() };
            let Some(queue) = queue else { return };
            for coordinate in payload
                .get("coordinates")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str())
            {
                let Some((registry, name, version)) = split_coordinate(coordinate) else {
                    continue;
                };
                let pkg = PackageId::new(&registry, &name, &version);
                if let Err(e) = queue.enqueue(&pkg, None, ScanTrigger::Webhook).await {
                    tracing::warn!(webhook, package = %pkg, error = %e, "security.rescan not queued");
                }
            }
        }
        _ => {}
    }
}

/// `registry:name@version` → its three parts. The name may itself carry
/// `@` (an npm scope) and `:` (a Maven coordinate), so the registry is the
/// text before the first `:` and the version the text after the last `@`.
fn split_coordinate(s: &str) -> Option<(String, String, String)> {
    let (registry, rest) = s.split_once(':')?;
    let (name, version) = rest.rsplit_once('@')?;
    if registry.is_empty() || name.is_empty() || version.is_empty() {
        return None;
    }
    Some((registry.to_owned(), name.to_owned(), version.to_owned()))
}

// ── Admin: list inbound events ────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct InboundEventsResponse {
    pub events: Vec<batlehub_core::entities::InboundWebhookEvent>,
}

/// List recent inbound webhook events (admin only).
#[utoipa::path(
    get,
    path = "/api/v1/admin/notifications/inbound",
    tag = "back-office",
    responses(
        (status = 200, description = "Inbound events", body = InboundEventsResponse),
        (status = 403, description = "`system:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/notifications/inbound")]
pub async fn list_inbound_events(
    identity: AuthIdentity,
    notification_store: web::Data<Arc<dyn NotificationPort>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::SystemRead,
        None,
        &hot,
    )
    .await?;
    let events = notification_store.list_inbound_events(100).await?;
    Ok(web::Json(InboundEventsResponse { events }))
}

#[cfg(test)]
mod tests {
    use super::split_coordinate;

    #[test]
    fn coordinates_split_on_the_first_colon_and_the_last_at() {
        assert_eq!(
            split_coordinate("npm:left-pad@1.3.1"),
            Some(("npm".into(), "left-pad".into(), "1.3.1".into()))
        );
        assert_eq!(
            split_coordinate("npm:@scope/pkg@2.0.0"),
            Some(("npm".into(), "@scope/pkg".into(), "2.0.0".into()))
        );
        assert_eq!(
            split_coordinate("maven:org.apache:commons@1.0"),
            Some(("maven".into(), "org.apache:commons".into(), "1.0".into()))
        );
        assert_eq!(split_coordinate("left-pad@1.3.1"), None);
        assert_eq!(split_coordinate("npm:left-pad"), None);
    }
}
