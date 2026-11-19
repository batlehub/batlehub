//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use bytes::Bytes;
use serde_json::Value;

use batlehub_adapters::notification::InMemoryNotificationStore;
use batlehub_config::schema::{
    InboundWebhookConfig, NotificationChannelConfig, NotificationsConfig, WebhookChannelConfig,
};
use batlehub_core::ports::NotificationPort;
use batlehub_web::services::NotificationService;
use uuid::Uuid;

// ── Inbound webhooks ─────────────────────────────────────────────────────────

fn compute_hmac_sha256_hex(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[actix_web::test]
async fn inbound_webhook_no_config_returns_400() {
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, None).await;

    let req = TestRequest::post()
        .uri("/api/v1/webhooks/inbound/ci")
        .set_json(serde_json::json!({"foo": "bar"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn inbound_webhook_unknown_name_returns_400() {
    let notifications_config = NotificationsConfig {
        enabled: true,
        channels: vec![],
        inbound: vec![InboundWebhookConfig {
            name: "ci".to_owned(),
            secret: None,
        }],
    };
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, Some(notifications_config)).await;

    let req = TestRequest::post()
        .uri("/api/v1/webhooks/inbound/unknown")
        .set_json(serde_json::json!({"foo": "bar"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn inbound_webhook_no_secret_accepts_and_records_event() {
    let notifications_config = NotificationsConfig {
        enabled: true,
        channels: vec![],
        inbound: vec![InboundWebhookConfig {
            name: "ci".to_owned(),
            secret: None,
        }],
    };
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, Some(notifications_config)).await;

    let req = TestRequest::post()
        .uri("/api/v1/webhooks/inbound/ci")
        .set_json(serde_json::json!({"foo": "bar"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let req = TestRequest::get()
        .uri("/api/v1/admin/notifications/inbound")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let events = body["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["webhook_name"], "ci");
    assert_eq!(events[0]["signature_valid"], Value::Null);
}

#[actix_web::test]
async fn inbound_webhook_missing_hmac_header_returns_403() {
    let notifications_config = NotificationsConfig {
        enabled: true,
        channels: vec![],
        inbound: vec![InboundWebhookConfig {
            name: "ci".to_owned(),
            secret: Some("s3cret".to_owned()),
        }],
    };
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, Some(notifications_config)).await;

    let req = TestRequest::post()
        .uri("/api/v1/webhooks/inbound/ci")
        .set_json(serde_json::json!({"foo": "bar"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

/// POST `body` to the `ci` inbound webhook, signed over `"{ts}.{body}"` with
/// `ts` sent as `X-BatleHub-Timestamp` (or no timestamp header when `None`).
async fn post_signed_inbound(secret: &str, ts: Option<i64>, body: &[u8]) -> u16 {
    let notifications_config = NotificationsConfig {
        enabled: true,
        channels: vec![],
        inbound: vec![InboundWebhookConfig {
            name: "ci".to_owned(),
            secret: Some(secret.to_owned()),
        }],
    };
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, Some(notifications_config)).await;

    let ts_text = ts.map(|t| t.to_string()).unwrap_or_default();
    let mut signed = format!("{ts_text}.").into_bytes();
    signed.extend_from_slice(body);
    let signature = format!("sha256={}", compute_hmac_sha256_hex(secret, &signed));

    let mut req = TestRequest::post()
        .uri("/api/v1/webhooks/inbound/ci")
        .insert_header(("X-Hub-Signature-256", signature))
        .insert_header(("content-type", "application/json"));
    if ts.is_some() {
        req = req.insert_header(("X-BatleHub-Timestamp", ts_text));
    }
    call_service(&app, req.set_payload(body.to_vec()).to_request())
        .await
        .status()
        .as_u16()
}

#[actix_web::test]
async fn inbound_webhook_valid_hmac_returns_200() {
    let body = serde_json::to_vec(&serde_json::json!({"foo": "bar"})).unwrap();
    let now = chrono::Utc::now().timestamp();
    assert_eq!(post_signed_inbound("s3cret", Some(now), &body).await, 200);
}

#[actix_web::test]
async fn inbound_webhook_replayed_with_an_old_timestamp_is_refused() {
    // A correctly signed request captured an hour ago: the signature still
    // matches its own timestamp, but the timestamp is stale.
    let body = serde_json::to_vec(&serde_json::json!({"foo": "bar"})).unwrap();
    let an_hour_ago = chrono::Utc::now().timestamp() - 3600;
    assert_eq!(
        post_signed_inbound("s3cret", Some(an_hour_ago), &body).await,
        403
    );
}

#[actix_web::test]
async fn inbound_webhook_without_a_timestamp_is_refused() {
    // The old scheme: HMAC over `".{body}"` with no timestamp header.
    let body = serde_json::to_vec(&serde_json::json!({"foo": "bar"})).unwrap();
    assert_eq!(post_signed_inbound("s3cret", None, &body).await, 403);
}

#[actix_web::test]
async fn inbound_webhook_invalid_json_returns_400() {
    let notifications_config = NotificationsConfig {
        enabled: true,
        channels: vec![],
        inbound: vec![InboundWebhookConfig {
            name: "ci".to_owned(),
            secret: None,
        }],
    };
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, Some(notifications_config)).await;

    let req = TestRequest::post()
        .uri("/api/v1/webhooks/inbound/ci")
        .insert_header(("content-type", "application/octet-stream"))
        .set_payload(Bytes::from_static(b"not json"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn list_inbound_events_requires_admin() {
    let notifications_config = NotificationsConfig {
        enabled: true,
        channels: vec![],
        inbound: vec![],
    };
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, Some(notifications_config)).await;

    let req = TestRequest::get()
        .uri("/api/v1/admin/notifications/inbound")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    let req = TestRequest::get()
        .uri("/api/v1/admin/notifications/inbound")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── Notification subscriptions admin CRUD ────────────────────────────────────

fn make_notification_service_with_webhook(url: &str) -> Arc<NotificationService> {
    let config = NotificationsConfig {
        enabled: true,
        channels: vec![NotificationChannelConfig::Webhook(WebhookChannelConfig {
            name: "wh".to_owned(),
            url: url.to_owned(),
            secret: None,
            timeout_secs: 5,
        })],
        inbound: vec![],
    };
    Arc::new(NotificationService::new(
        Arc::new(InMemoryNotificationStore::new()),
        &config,
    ))
}

#[actix_web::test]
async fn notification_admin_endpoints_503_when_not_configured() {
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    let app = make_app_with_notifications(None, store, None).await;
    let id = Uuid::new_v4();
    let create_body = serde_json::json!({
        "registry": null,
        "package_name": null,
        "event_types": ["package_published"],
        "channel_name": "wh",
    });
    let update_body = serde_json::json!({
        "registry": null,
        "package_name": null,
        "event_types": ["package_published"],
        "channel_name": "wh",
        "enabled": true,
    });

    let cases: Vec<(actix_http::Method, String, Option<Value>)> = vec![
        (
            actix_http::Method::GET,
            "/api/v1/admin/notifications/channels".to_owned(),
            None,
        ),
        (
            actix_http::Method::GET,
            "/api/v1/admin/notifications/subscriptions".to_owned(),
            None,
        ),
        (
            actix_http::Method::POST,
            "/api/v1/admin/notifications/subscriptions".to_owned(),
            Some(create_body),
        ),
        (
            actix_http::Method::GET,
            format!("/api/v1/admin/notifications/subscriptions/{id}"),
            None,
        ),
        (
            actix_http::Method::PUT,
            format!("/api/v1/admin/notifications/subscriptions/{id}"),
            Some(update_body),
        ),
        (
            actix_http::Method::DELETE,
            format!("/api/v1/admin/notifications/subscriptions/{id}"),
            None,
        ),
        (
            actix_http::Method::POST,
            format!("/api/v1/admin/notifications/subscriptions/{id}/test"),
            None,
        ),
    ];

    for (method, uri, body) in cases {
        let mut req = TestRequest::with_uri(&uri)
            .method(method.clone())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)));
        if let Some(b) = &body {
            req = req.set_json(b);
        }
        let resp = call_service(&app, req.to_request()).await;
        assert_eq!(resp.status(), 503, "{method} {uri}");
    }
}

#[actix_web::test]
async fn list_notification_channels_returns_configured_channels() {
    let svc = make_notification_service_with_webhook("http://example.invalid/hook");
    let store = Arc::clone(svc.store());
    let app = make_app_with_notifications(Some(svc), store, None).await;

    let req = TestRequest::get()
        .uri("/api/v1/admin/notifications/channels")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let channels = body.as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["name"], "wh");
}

#[actix_web::test]
async fn create_subscription_validation_errors() {
    let svc = make_notification_service_with_webhook("http://example.invalid/hook");
    let store = Arc::clone(svc.store());
    let app = make_app_with_notifications(Some(svc), store, None).await;

    // Empty event_types -> 400.
    let req = TestRequest::post()
        .uri("/api/v1/admin/notifications/subscriptions")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": null,
            "package_name": null,
            "event_types": [],
            "channel_name": "wh",
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);

    // Unknown channel_name -> 400.
    let req = TestRequest::post()
        .uri("/api/v1/admin/notifications/subscriptions")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": null,
            "package_name": null,
            "event_types": ["package_published"],
            "channel_name": "does-not-exist",
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn subscription_crud_round_trip() {
    let svc = make_notification_service_with_webhook("http://example.invalid/hook");
    let store = Arc::clone(svc.store());
    let app = make_app_with_notifications(Some(svc), store, None).await;

    // Create.
    let req = TestRequest::post()
        .uri("/api/v1/admin/notifications/subscriptions")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm-proxy",
            "package_name": null,
            "event_types": ["package_published"],
            "channel_name": "wh",
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let created: Value = read_body_json(resp).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let created_at = created["created_at"].clone();
    assert_eq!(created["created_by"], "admin");

    // List.
    let req = TestRequest::get()
        .uri("/api/v1/admin/notifications/subscriptions")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let listed: Value = read_body_json(resp).await;
    assert_eq!(listed.as_array().expect("array").len(), 1);

    // Get.
    let req = TestRequest::get()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    // Update — preserves id/created_by/created_at via `..existing`.
    let req = TestRequest::put()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm-proxy",
            "package_name": "left-pad",
            "event_types": ["package_published", "package_yanked"],
            "channel_name": "wh",
            "enabled": false,
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let updated: Value = read_body_json(resp).await;
    assert_eq!(updated["id"], id);
    assert_eq!(updated["created_by"], "admin");
    assert_eq!(updated["created_at"], created_at);
    assert_eq!(updated["package_name"], "left-pad");
    assert_eq!(updated["enabled"], false);

    // Delete.
    let req = TestRequest::delete()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 204);

    // Get after delete -> 404.
    let req = TestRequest::get()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn subscription_unknown_id_returns_404() {
    let svc = make_notification_service_with_webhook("http://example.invalid/hook");
    let store = Arc::clone(svc.store());
    let app = make_app_with_notifications(Some(svc), store, None).await;
    let id = Uuid::new_v4();

    let req = TestRequest::get()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);

    let req = TestRequest::put()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": null,
            "package_name": null,
            "event_types": ["package_published"],
            "channel_name": "wh",
            "enabled": true,
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);

    let req = TestRequest::delete()
        .uri(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);

    let req = TestRequest::post()
        .uri(&format!(
            "/api/v1/admin/notifications/subscriptions/{id}/test"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn test_subscription_dispatch_failure_returns_400() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("POST", "/hook")
        .with_status(500)
        .create_async()
        .await;
    let svc = make_notification_service_with_webhook(&format!("{}/hook", server.url()));
    let store = Arc::clone(svc.store());
    let app = make_app_with_notifications(Some(svc), store, None).await;

    // Create a subscription pointing at the failing webhook.
    let req = TestRequest::post()
        .uri("/api/v1/admin/notifications/subscriptions")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": null,
            "package_name": null,
            "event_types": ["package_published"],
            "channel_name": "wh",
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let created: Value = read_body_json(resp).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let req = TestRequest::post()
        .uri(&format!(
            "/api/v1/admin/notifications/subscriptions/{id}/test"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}
