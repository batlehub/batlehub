use std::future::{ready, Ready};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use actix_web::{
    body::EitherBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    http::StatusCode,
    Error, HttpResponse,
};
use futures::future::LocalBoxFuture;

use batlehub_config::schema::IpBlockingConfig;
use batlehub_core::entities::{AccessAction, AccessEvent, AccessResult, CallerNet, Identity};
use batlehub_core::ports::IpBlockStore;
use batlehub_core::services::AdminService;

use super::proxy_trust::{client_ip, peer_trust_of_service, PeerTrust};

/// Extract the client IP. `X-Forwarded-For` is only trusted when `trust` says the
/// TCP peer is one of the configured reverse proxies; otherwise the peer address
/// is used directly to prevent spoofed-header bypass.
///
/// The verdict is computed once per request by the host-routing middleware and
/// shared through the request extensions, so the client IP, the forwarded host
/// and the forwarded scheme cannot disagree about which peers are trusted — see
/// [`super::proxy_trust`].
pub(crate) fn extract_client_ip(req: &ServiceRequest, trust: PeerTrust) -> String {
    client_ip(req.request(), trust)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Factory ───────────────────────────────────────────────────────────────────

pub struct IpBlockMiddlewareFactory {
    store: Arc<dyn IpBlockStore>,
    config: Arc<IpBlockingConfig>,
    audit: Option<Arc<AdminService>>,
}

impl IpBlockMiddlewareFactory {
    pub fn new(store: Arc<dyn IpBlockStore>, config: IpBlockingConfig) -> Self {
        Self {
            store,
            config: Arc::new(config),
            audit: None,
        }
    }

    /// Write a `block_ip` audit event for every automatic ban.
    ///
    /// The ban row itself is deleted once it expires and the violation
    /// counters after 30 days, so without this an automatic ban leaves no
    /// durable trace — the manual one in the console always did. The event
    /// follows the audit trail's own retention (RFC 0036 §6.3).
    pub fn with_audit(mut self, audit: Arc<AdminService>) -> Self {
        self.audit = Some(audit);
        self
    }
}

impl<S, B> Transform<S, ServiceRequest> for IpBlockMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = IpBlockMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(IpBlockMiddleware {
            service: Rc::new(service),
            store: self.store.clone(),
            config: self.config.clone(),
            audit: self.audit.clone(),
        }))
    }
}

// ── Middleware ────────────────────────────────────────────────────────────────

pub struct IpBlockMiddleware<S> {
    service: Rc<S>,
    store: Arc<dyn IpBlockStore>,
    config: Arc<IpBlockingConfig>,
    audit: Option<Arc<AdminService>>,
}

impl<S, B> Service<ServiceRequest> for IpBlockMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let service = self.service.clone();
        let store = self.store.clone();
        let config = self.config.clone();
        let audit = self.audit.clone();

        Box::pin(async move {
            let ip = extract_client_ip(&req, peer_trust_of_service(&req));
            let user_agent = req
                .headers()
                .get(actix_web::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);

            match store.blocked_until(&ip).await {
                Ok(Some(unblock_at)) => {
                    let body = serde_json::json!({
                        "error": "Forbidden",
                        "message": "your IP address has been temporarily blocked",
                    });
                    let http_res = HttpResponse::build(StatusCode::FORBIDDEN)
                        .insert_header(("X-Block-Expires", unblock_at.to_string()))
                        .content_type("application/json")
                        .body(serde_json::to_string(&body).unwrap_or_default());
                    return Ok(req.into_response(http_res).map_into_right_body());
                }
                Ok(None) => {}
                Err(e) => {
                    // Fail-open: log but allow the request through.
                    tracing::warn!(error = %e, ip = %ip, "ip-block store unavailable; failing open");
                }
            }

            let res = service.call(req).await?.map_into_left_body();

            let status = res.status().as_u16();
            if config.trigger_on_status.contains(&status) {
                match store
                    .record_violation(&ip, config.violation_window_secs)
                    .await
                {
                    Ok((count, _)) => {
                        if count >= config.violation_threshold as u64 {
                            let unblock_at = now_unix() + config.ban_duration_secs;
                            if let Err(e) = store.block_ip(&ip, unblock_at, "auto").await {
                                tracing::warn!(error = %e, ip = %ip, "failed to auto-block ip");
                            } else {
                                tracing::info!(ip = %ip, unblock_at, "auto-blocked ip after violation threshold");
                                if let Some(audit) = &audit {
                                    audit
                                        .record_event(auto_block_event(
                                            &ip, user_agent, unblock_at, status, count, &config,
                                        ))
                                        .await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, ip = %ip, "failed to record violation");
                    }
                }
            }

            Ok(res)
        })
    }
}

/// The audit event for an automatic ban: done by the process (`system`), about
/// the caller it banned, saying what tipped it over.
fn auto_block_event(
    ip: &str,
    user_agent: Option<String>,
    unblock_at: u64,
    status: u16,
    count: u64,
    config: &IpBlockingConfig,
) -> AccessEvent {
    let system = Identity::system();
    AccessEvent::about_identity(
        AccessAction::BlockIp,
        system.user_id,
        system.role,
        AccessResult::Allowed,
        CallerNet {
            ip: Some(ip.to_owned()),
            user_agent,
        },
        Some(format!(
            "ip={ip} until={unblock_at} reason=auto status={status} violations={count} \
             threshold={} window_secs={}",
            config.violation_threshold, config.violation_window_secs
        )),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use actix_web::{
        http::StatusCode,
        test::{self, TestRequest},
        web, App, HttpResponse,
    };
    use batlehub_adapters::rate_limit::InMemoryIpBlockStore;
    use batlehub_config::schema::IpBlockingConfig;
    use batlehub_core::ports::IpBlockStore;

    use super::*;

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn default_config() -> IpBlockingConfig {
        IpBlockingConfig {
            enabled: true,
            violation_threshold: 3,
            violation_window_secs: 300,
            ban_duration_secs: 3600,
            trigger_on_status: vec![429, 401],
            trusted_proxies: Vec::new(),
        }
    }

    // ── extract_client_ip ─────────────────────────────────────────────────────
    //
    // The peer-membership test itself now lives in `proxy_trust` (and is covered
    // by its own CIDR tests); these assert that a given verdict produces the same
    // client IP it always did.

    #[test]
    fn xff_ignored_when_no_trusted_proxies() {
        // Without trusted_proxies, XFF must be ignored — peer addr wins.
        let req = TestRequest::get()
            .peer_addr("10.0.0.99:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "203.0.113.5, 10.0.0.1"))
            .to_srv_request();
        let ip = extract_client_ip(&req, PeerTrust::LegacyPermissive);
        assert_eq!(
            ip, "10.0.0.99",
            "XFF must not be trusted without trusted_proxies"
        );
    }

    #[test]
    fn xff_used_when_peer_is_trusted_proxy() {
        // The right-most hop is the address our own proxy observed; the entries
        // to its left are whatever the client chose to send — see
        // `proxy_trust::forwarded_client_ip`.
        let req = TestRequest::get()
            .peer_addr("10.0.0.1:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "203.0.113.5, 172.16.0.1"))
            .to_srv_request();
        assert_eq!(extract_client_ip(&req, PeerTrust::Trusted), "172.16.0.1");
    }

    #[test]
    fn xff_single_entry_with_trusted_proxy() {
        let req = TestRequest::get()
            .peer_addr("10.0.0.1:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "198.51.100.7"))
            .to_srv_request();
        assert_eq!(extract_client_ip(&req, PeerTrust::Trusted), "198.51.100.7");
    }

    #[test]
    fn xff_not_used_when_peer_not_in_trusted_list() {
        let req = TestRequest::get()
            .peer_addr("192.0.2.1:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "1.1.1.1"))
            .to_srv_request();
        assert_eq!(extract_client_ip(&req, PeerTrust::Untrusted), "192.0.2.1");
    }

    #[test]
    fn ip_falls_back_to_unknown_when_no_peer() {
        let req = TestRequest::get().to_srv_request();
        let ip = extract_client_ip(&req, PeerTrust::LegacyPermissive);
        assert!(!ip.is_empty());
    }

    /// An automatic ban lands in the audit trail — done by `system`, about the
    /// banned caller — because the ban row and the counters are both pruned.
    /// Requests after the ban are refused before counting, so one ban, one event.
    #[actix_web::test]
    async fn an_automatic_ban_is_audited_once() {
        use batlehub_core::entities::EventFilter;

        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let admin = Arc::new(AdminService::new(
            batlehub_adapters::in_memory::InMemoryPackageRepository::new(),
        ));
        let app = test::init_service(
            App::new()
                .wrap(
                    IpBlockMiddlewareFactory::new(Arc::clone(&store), default_config())
                        .with_audit(Arc::clone(&admin)),
                )
                .route(
                    "/rate",
                    web::get().to(|| async { HttpResponse::TooManyRequests().finish() }),
                ),
        )
        .await;

        for _ in 0..5 {
            let req = TestRequest::get()
                .peer_addr("198.51.100.4:1234".parse().unwrap())
                .insert_header(("user-agent", "spray/1.0"))
                .uri("/rate")
                .to_request();
            test::call_service(&app, req).await;
        }

        let events = admin
            .list_events(EventFilter {
                actions: vec![AccessAction::BlockIp],
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        let e = &events[0];
        assert_eq!(e.user_id.as_deref(), Some(Identity::SYSTEM_USER_ID));
        assert_eq!(e.ip_address.as_deref(), Some("198.51.100.4"));
        assert_eq!(e.user_agent.as_deref(), Some("spray/1.0"));
        let detail = e.detail.as_deref().unwrap();
        assert!(detail.starts_with("ip=198.51.100.4 until="), "{detail}");
        assert!(
            detail.contains("reason=auto status=429 violations=3 threshold=3"),
            "{detail}"
        );
    }

    #[actix_web::test]
    async fn middleware_honours_xff_when_the_registered_policy_trusts_the_peer() {
        // End-to-end through the middleware: the ban is recorded against the
        // forwarded client IP, not the proxy's own address.
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let config = IpBlockingConfig {
            violation_threshold: 1,
            trigger_on_status: vec![429],
            ..default_config()
        };
        let trust = crate::middleware::ProxyTrust::from_config(Some(&["10.42.0.0/16".to_owned()]));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(trust))
                .wrap(IpBlockMiddlewareFactory::new(Arc::clone(&store), config))
                .route(
                    "/rate",
                    web::get().to(|| async { HttpResponse::TooManyRequests().finish() }),
                ),
        )
        .await;

        let req = TestRequest::get()
            .peer_addr("10.42.0.9:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "203.0.113.77"))
            .uri("/rate")
            .to_request();
        test::call_service(&app, req).await;

        assert!(store.blocked_until("203.0.113.77").await.unwrap().is_some());
        assert!(store.blocked_until("10.42.0.9").await.unwrap().is_none());
    }

    #[actix_web::test]
    async fn middleware_ignores_xff_from_a_peer_outside_the_trusted_range() {
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let config = IpBlockingConfig {
            violation_threshold: 1,
            trigger_on_status: vec![429],
            ..default_config()
        };
        let trust = crate::middleware::ProxyTrust::from_config(Some(&["10.42.0.0/16".to_owned()]));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(trust))
                .wrap(IpBlockMiddlewareFactory::new(Arc::clone(&store), config))
                .route(
                    "/rate",
                    web::get().to(|| async { HttpResponse::TooManyRequests().finish() }),
                ),
        )
        .await;

        let req = TestRequest::get()
            .peer_addr("198.51.100.4:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "203.0.113.77"))
            .uri("/rate")
            .to_request();
        test::call_service(&app, req).await;

        assert!(store.blocked_until("203.0.113.77").await.unwrap().is_none());
        assert!(store.blocked_until("198.51.100.4").await.unwrap().is_some());
    }

    // ── middleware integration via actix App ──────────────────────────────────

    macro_rules! make_app {
        ($store:expr, $config:expr) => {
            test::init_service(
                App::new()
                    .wrap(IpBlockMiddlewareFactory::new($store, $config))
                    .route(
                        "/ok",
                        web::get().to(|| async { HttpResponse::Ok().finish() }),
                    )
                    .route(
                        "/rate",
                        web::get().to(|| async { HttpResponse::TooManyRequests().finish() }),
                    )
                    .route(
                        "/auth",
                        web::get().to(|| async { HttpResponse::Unauthorized().finish() }),
                    ),
            )
            .await
        };
    }

    #[actix_web::test]
    async fn allowed_ip_passes_through() {
        let store = Arc::new(InMemoryIpBlockStore::new());
        let app = make_app!(store, default_config());
        let req = TestRequest::get().uri("/ok").to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn blocked_ip_gets_403() {
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        store
            .block_ip("198.51.100.1", now() + 3600, "test")
            .await
            .unwrap();
        let app = make_app!(Arc::clone(&store), default_config());
        let req = TestRequest::get()
            .peer_addr("198.51.100.1:1234".parse().unwrap())
            .uri("/ok")
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn blocked_ip_response_has_x_block_expires_header() {
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let unblock_at = now() + 3600;
        store
            .block_ip("203.0.113.1", unblock_at, "test")
            .await
            .unwrap();
        let app = make_app!(Arc::clone(&store), default_config());
        let req = TestRequest::get()
            .peer_addr("203.0.113.1:1234".parse().unwrap())
            .uri("/ok")
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let hdr = res.headers().get("x-block-expires").unwrap();
        assert_eq!(hdr.to_str().unwrap(), unblock_at.to_string());
    }

    #[actix_web::test]
    async fn auto_block_after_violation_threshold() {
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let config = IpBlockingConfig {
            violation_threshold: 2,
            trigger_on_status: vec![429],
            ..default_config()
        };
        let app = make_app!(Arc::clone(&store), config);

        // First two 429s → violations recorded but threshold not yet reached.
        for _ in 0..2 {
            let req = TestRequest::get()
                .peer_addr("10.0.0.1:1234".parse().unwrap())
                .uri("/rate")
                .to_request();
            let res = test::call_service(&app, req).await;
            assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        }

        // Now the IP should be auto-blocked.
        assert!(store.blocked_until("10.0.0.1").await.unwrap().is_some());

        // Next request returns 403, not 429.
        let req = TestRequest::get()
            .peer_addr("10.0.0.1:1234".parse().unwrap())
            .uri("/ok")
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn violation_not_recorded_for_non_trigger_status() {
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let config = IpBlockingConfig {
            violation_threshold: 1,
            trigger_on_status: vec![429],
            ..default_config()
        };
        let app = make_app!(Arc::clone(&store), config);

        // 401 is not in trigger_on_status, so no violation recorded.
        let req = TestRequest::get()
            .peer_addr("10.0.0.2:1234".parse().unwrap())
            .uri("/auth")
            .to_request();
        test::call_service(&app, req).await;

        assert!(store.blocked_until("10.0.0.2").await.unwrap().is_none());
    }

    #[actix_web::test]
    async fn different_ips_are_independent() {
        let store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let config = IpBlockingConfig {
            violation_threshold: 2,
            trigger_on_status: vec![429],
            ..default_config()
        };
        let app = make_app!(Arc::clone(&store), config);

        // Two violations from IP A reach the threshold.
        for _ in 0..2 {
            let req = TestRequest::get()
                .peer_addr("10.0.0.10:1234".parse().unwrap())
                .uri("/rate")
                .to_request();
            test::call_service(&app, req).await;
        }

        // IP B still allowed.
        let req = TestRequest::get()
            .peer_addr("10.0.0.20:1234".parse().unwrap())
            .uri("/ok")
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
}
