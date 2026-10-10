use std::collections::HashMap;
use std::future::{ready, Ready};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error, HttpMessage,
};
use futures::future::LocalBoxFuture;

use batlehub_core::{
    entities::{AccessAction, AccessEvent, AccessResult, Identity},
    ports::AuthProvider,
    services::AdminService,
};

use crate::extractors::{caller_net, raw_auth_from_request};

/// Actix-web middleware that attempts each configured `AuthProvider` in order.
///
/// On success the resolved `Identity` is stored in request extensions so that
/// handlers can extract it via `AuthIdentity`. Falls back to `Identity::anonymous()`.
pub struct AuthMiddlewareFactory {
    providers: Arc<Vec<Arc<dyn AuthProvider>>>,
    audit: Option<Arc<CredentialRejectionAudit>>,
}

impl AuthMiddlewareFactory {
    pub fn new(providers: Vec<Arc<dyn AuthProvider>>) -> Self {
        Self {
            providers: Arc::new(providers),
            audit: None,
        }
    }

    /// Record a `credential_rejected` audit event when a credential was
    /// presented and no provider accepted it (RFC 0036 §6.1).
    ///
    /// Takes the writer rather than building it: an actix server calls its
    /// app factory once **per worker**, so a throttle built in there is one
    /// throttle per worker, and a burst spread over eight workers wrote eight
    /// rows a minute. Build one [`CredentialRejectionAudit`] outside the
    /// factory and hand every worker a clone.
    pub fn with_audit(mut self, audit: Arc<CredentialRejectionAudit>) -> Self {
        self.audit = Some(audit);
        self
    }
}

/// The `credential_rejected` writer: the audit sink and its throttle. One per
/// process — see [`AuthMiddlewareFactory::with_audit`].
pub struct CredentialRejectionAudit {
    admin_svc: Arc<AdminService>,
    throttle: RejectionThrottle,
}

impl CredentialRejectionAudit {
    pub fn new(admin_svc: Arc<AdminService>) -> Arc<Self> {
        Arc::new(Self {
            admin_svc,
            throttle: RejectionThrottle::default(),
        })
    }
}

/// One `credential_rejected` row per source IP per [`Self::WINDOW`].
///
/// A credential-stuffing burst costs one write a minute rather than one per
/// attempt. The attempts suppressed in between are not lost: their count rides
/// on the next row written for that IP, as its `throttled_count`.
///
/// Per process, like the `last_used_at` throttle: *n* replicas write up to *n*
/// rows a minute per IP, which the burst rule's threshold allows for.
#[derive(Default)]
struct RejectionThrottle {
    /// Per source IP: when its last row was written, and how many attempts
    /// have been suppressed since.
    seen: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RejectionThrottle {
    const WINDOW: Duration = Duration::from_secs(60);
    /// Past this many tracked IPs, expired entries are swept on the next write.
    const SWEEP_AT: usize = 10_000;

    /// `Some(suppressed)` when a row is due for `ip` — `suppressed` being how
    /// many attempts were held back since its last one — or `None` when this
    /// attempt is itself held back.
    fn admit(&self, ip: &str, now: Instant) -> Option<u32> {
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((at, suppressed)) = seen.get_mut(ip) {
            if now.duration_since(*at) < Self::WINDOW {
                *suppressed += 1;
                return None;
            }
            let held = *suppressed;
            *at = now;
            *suppressed = 0;
            return Some(held);
        }
        // ponytail: an O(n) sweep when the map is large; a busy estate under a
        // distributed spray would want an LRU, which is not needed until then.
        if seen.len() >= Self::SWEEP_AT {
            seen.retain(|_, (at, _)| now.duration_since(*at) < Self::WINDOW);
        }
        seen.insert(ip.to_owned(), (now, 0));
        Some(0)
    }
}

impl<S, B> Transform<S, ServiceRequest> for AuthMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddleware {
            service: Rc::new(service),
            providers: self.providers.clone(),
            audit: self.audit.clone(),
        }))
    }
}

pub struct AuthMiddleware<S> {
    service: Rc<S>,
    providers: Arc<Vec<Arc<dyn AuthProvider>>>,
    audit: Option<Arc<CredentialRejectionAudit>>,
}

impl<S, B> Service<ServiceRequest> for AuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let service = self.service.clone();
        let providers = self.providers.clone();
        let audit = self.audit.clone();

        Box::pin(async move {
            let raw = raw_auth_from_request(req.request());
            let mut identity = Identity::anonymous();
            let mut accepted = false;

            'providers: for provider in providers.iter() {
                match provider.authenticate(&raw).await {
                    Ok(Some(id)) => {
                        tracing::debug!(
                            provider = provider.name(),
                            user_id = ?id.user_id,
                            role = %id.role,
                            "authenticated"
                        );
                        identity = id;
                        accepted = true;
                        break 'providers;
                    }
                    Ok(None) => {} // provider did not recognise the credentials
                    Err(e) => {
                        // Only genuine provider-level failures land here (e.g. malformed
                        // JWT, unknown signing key) — a wrong static token or an expired
                        // JWT returns `Ok(None)` per the `AuthProvider` contract, so this
                        // counter deliberately does not cover "all failed logins".
                        metrics::counter!("batlehub_auth_failures_total", "provider" => provider.name().to_string()).increment(1);
                        tracing::warn!(provider = provider.name(), error = %e, "auth provider error");
                    }
                }
            }

            // The request goes on as anonymous either way — the refusal the
            // client sees is the registry's own for an anonymous read — so this
            // is the one place that knows a credential was presented *and*
            // refused, and the one place that can say so.
            let presented = raw
                .headers
                .get("authorization")
                .is_some_and(|v| !v.trim().is_empty());
            if let (Some(audit), true, false) = (audit.as_ref(), presented, accepted) {
                let net = caller_net(req.request());
                let ip = net.ip.clone().unwrap_or_default();
                if let Some(suppressed) = audit.throttle.admit(&ip, Instant::now()) {
                    let mut event = AccessEvent::about_identity(
                        AccessAction::CredentialRejected,
                        None,
                        identity.role.clone(),
                        AccessResult::Denied {
                            reason: "no authentication provider accepted the credential".to_owned(),
                        },
                        net,
                        None,
                    );
                    event.throttled_count = (suppressed > 0).then_some(suppressed);
                    audit.admin_svc.record_event(event).await;
                }
            }

            req.extensions_mut().insert(identity);
            service.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_throttle_writes_once_a_window_and_carries_what_it_held_back() {
        let throttle = RejectionThrottle::default();
        let t0 = Instant::now();
        assert_eq!(
            throttle.admit("10.0.0.1", t0),
            Some(0),
            "the first attempt is written"
        );
        for _ in 0..20 {
            assert_eq!(
                throttle.admit("10.0.0.1", t0 + Duration::from_secs(1)),
                None
            );
        }
        assert_eq!(
            throttle.admit("10.0.0.2", t0 + Duration::from_secs(1)),
            Some(0),
            "another IP has its own window"
        );
        assert_eq!(
            throttle.admit("10.0.0.1", t0 + RejectionThrottle::WINDOW),
            Some(20),
            "the next row carries the twenty held back"
        );
    }
    use actix_web::{
        test::{self, TestRequest},
        web, App, HttpRequest, HttpResponse,
    };
    use async_trait::async_trait;
    use batlehub_core::{
        entities::{Identity, Role},
        error::CoreError,
        ports::{AuthProvider, RawAuthRequest},
    };

    struct AlwaysAuth(String);

    #[async_trait]
    impl AuthProvider for AlwaysAuth {
        fn name(&self) -> &str {
            "always"
        }
        async fn authenticate(&self, _: &RawAuthRequest) -> Result<Option<Identity>, CoreError> {
            Ok(Some(Identity {
                user_id: Some(self.0.clone()),
                role: Role::User,
                auth_provider: Some("always".into()),
                groups: vec![],
            }))
        }
    }

    struct NeverAuth;

    #[async_trait]
    impl AuthProvider for NeverAuth {
        fn name(&self) -> &str {
            "never"
        }
        async fn authenticate(&self, _: &RawAuthRequest) -> Result<Option<Identity>, CoreError> {
            Ok(None)
        }
    }

    async fn who_am_i(req: HttpRequest) -> HttpResponse {
        let user = req
            .extensions()
            .get::<Identity>()
            .and_then(|i| i.user_id.clone())
            .unwrap_or_else(|| "anonymous".into());
        HttpResponse::Ok().body(user)
    }

    #[actix_web::test]
    async fn no_providers_yields_anonymous() {
        let app = test::init_service(
            App::new()
                .wrap(AuthMiddlewareFactory::new(vec![]))
                .route("/", web::get().to(who_am_i)),
        )
        .await;
        let req = TestRequest::get().uri("/").to_request();
        let resp = test::call_service(&app, req).await;
        let body = test::read_body(resp).await;
        assert_eq!(body, "anonymous");
    }

    #[actix_web::test]
    async fn first_matching_provider_wins() {
        let providers: Vec<Arc<dyn AuthProvider>> = vec![
            Arc::new(AlwaysAuth("alice".into())),
            Arc::new(AlwaysAuth("bob".into())),
        ];
        let app = test::init_service(
            App::new()
                .wrap(AuthMiddlewareFactory::new(providers))
                .route("/", web::get().to(who_am_i)),
        )
        .await;
        let req = TestRequest::get().uri("/").to_request();
        let body = test::read_body(test::call_service(&app, req).await).await;
        assert_eq!(body, "alice");
    }

    #[actix_web::test]
    async fn falls_back_to_second_provider() {
        let providers: Vec<Arc<dyn AuthProvider>> =
            vec![Arc::new(NeverAuth), Arc::new(AlwaysAuth("carol".into()))];
        let app = test::init_service(
            App::new()
                .wrap(AuthMiddlewareFactory::new(providers))
                .route("/", web::get().to(who_am_i)),
        )
        .await;
        let req = TestRequest::get().uri("/").to_request();
        let body = test::read_body(test::call_service(&app, req).await).await;
        assert_eq!(body, "carol");
    }

    #[actix_web::test]
    async fn all_providers_fail_yields_anonymous() {
        let providers: Vec<Arc<dyn AuthProvider>> = vec![Arc::new(NeverAuth)];
        let app = test::init_service(
            App::new()
                .wrap(AuthMiddlewareFactory::new(providers))
                .route("/", web::get().to(who_am_i)),
        )
        .await;
        let req = TestRequest::get().uri("/").to_request();
        let body = test::read_body(test::call_service(&app, req).await).await;
        assert_eq!(body, "anonymous");
    }
}
