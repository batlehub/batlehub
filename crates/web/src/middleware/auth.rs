use std::collections::HashMap;
use std::future::{ready, Ready};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    http::{
        header::{self, HeaderValue},
        StatusCode,
    },
    Error, HttpMessage,
};
use futures::future::LocalBoxFuture;

use batlehub_core::{
    entities::{AccessAction, AccessEvent, AccessResult, Identity},
    ports::AuthProvider,
    services::AdminService,
};

use crate::error::AppError;
use crate::extractors::{caller_net, raw_auth_from_request};
use batlehub_core::entities::CallerNet;

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

    /// Write the attempts held back for every IP whose window has elapsed
    /// (every IP at all when `all`, at shutdown): without it a burst that
    /// stops is never counted, since its count waits for a next attempt.
    ///
    /// The row stands for one of the held-back attempts, so it carries the
    /// others as `throttled_count`: readers keep summing `1 + throttled_count`.
    pub async fn flush_held(&self, now: Instant, all: bool) {
        for (ip, held) in self.throttle.take_held(now, all) {
            let mut event = AccessEvent::about_identity(
                AccessAction::CredentialRejected,
                None,
                Identity::anonymous().role,
                AccessResult::Denied {
                    reason: "no authentication provider accepted the credential".to_owned(),
                },
                CallerNet {
                    ip: Some(ip),
                    user_agent: None,
                },
                Some(format!(
                    "{held} attempt(s) held back by the throttle, written after their window"
                )),
            );
            event.throttled_count = (held > 1).then_some(held - 1);
            self.admin_svc.record_event(event).await;
        }
    }

    /// Run [`Self::flush_held`] every [`Self::FLUSH_EVERY`] for as long as the
    /// writer lives, so a burst is counted at most `WINDOW + FLUSH_EVERY`
    /// after it ends. Call from inside a tokio runtime.
    pub fn spawn_flusher(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Self::FLUSH_EVERY);
            loop {
                tick.tick().await;
                let Some(audit) = weak.upgrade() else { return };
                audit.flush_held(Instant::now(), false).await;
            }
        })
    }

    const FLUSH_EVERY: Duration = Duration::from_secs(10);
}

/// One `credential_rejected` row per source IP per [`Self::WINDOW`].
///
/// A credential-stuffing burst costs one write a minute rather than one per
/// attempt. The attempts suppressed in between are not lost: their count rides
/// on the next row written for that IP, as its `throttled_count` — or, when
/// no attempt follows, on the row [`CredentialRejectionAudit::flush_held`]
/// writes once the window has elapsed.
///
/// Per process, like the `last_used_at` throttle: *n* replicas write up to *n*
/// rows a minute per IP, which the burst rule's threshold allows for.
#[derive(Default)]
pub(crate) struct RejectionThrottle {
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
    pub(crate) fn admit(&self, ip: &str, now: Instant) -> Option<u32> {
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
        // An entry still holding attempts is kept for the flusher to write.
        if seen.len() >= Self::SWEEP_AT {
            seen.retain(|_, (at, held)| *held > 0 || now.duration_since(*at) < Self::WINDOW);
        }
        seen.insert(ip.to_owned(), (now, 0));
        Some(0)
    }

    /// Forget every IP whose window has elapsed (every IP when `all`) and
    /// return those that were holding attempts back, with their count.
    pub(crate) fn take_held(&self, now: Instant, all: bool) -> Vec<(String, u32)> {
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut held = Vec::new();
        seen.retain(|ip, (at, suppressed)| {
            if !all && now.duration_since(*at) < Self::WINDOW {
                return true;
            }
            if *suppressed > 0 {
                held.push((ip.clone(), *suppressed));
            }
            false
        });
        held
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

            // Only a package-manager route may challenge: a `WWW-Authenticate:
            // Basic` on `/api` or the SPA would pop the browser's login dialog.
            let challenge = !accepted && req.path().starts_with("/proxy/");
            req.extensions_mut().insert(identity);
            let mut res = service.call(req).await?;
            if challenge {
                challenge_anonymous(res.response_mut());
            }
            if presented && !accepted {
                res.response_mut()
                    .extensions_mut()
                    .insert(CredentialRejected);
            }
            Ok(res)
        })
    }
}

/// Marks a response to a request whose credential no provider accepted. The
/// IP-ban counts a `401` only with it: the bare challenge git, NuGet and Maven
/// need before they send their credential is not a failed login.
#[derive(Clone, Copy, Debug)]
pub struct CredentialRejected;

/// Turn a refusal a credential could answer into `401` + `WWW-Authenticate:
/// Basic`, for a caller known to have presented none a provider accepted.
///
/// git, NuGet and Maven send the credential they hold only in answer to a
/// `401` carrying a challenge; a bare `403` ends them. Anything else — a rule's
/// `403`, a `404`, a success — is left as it is.
pub(crate) fn challenge_anonymous<B>(res: &mut actix_web::HttpResponse<B>) {
    if res
        .error()
        .and_then(|e| e.as_error::<AppError>())
        .is_some_and(AppError::wants_credential)
    {
        // shortcut: the JSON body was rendered for the 403 and still says
        // "Forbidden"; rewriting it means an EitherBody middleware. Clients read
        // the status and the challenge, never that field.
        *res.status_mut() = StatusCode::UNAUTHORIZED;
        res.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"BatleHub\""),
        );
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

    /// A burst that stops is still counted: the flusher takes what an elapsed
    /// window held back, and only that, exactly once.
    #[test]
    fn a_burst_that_stops_is_taken_once_its_window_elapses() {
        let throttle = RejectionThrottle::default();
        let t0 = Instant::now();
        throttle.admit("10.0.0.1", t0);
        for _ in 0..23 {
            throttle.admit("10.0.0.1", t0 + Duration::from_secs(1));
        }
        throttle.admit("10.0.0.2", t0);
        assert!(
            throttle
                .take_held(t0 + Duration::from_secs(30), false)
                .is_empty(),
            "a window still open is left alone"
        );
        assert_eq!(
            throttle.take_held(t0 + RejectionThrottle::WINDOW, false),
            vec![("10.0.0.1".to_owned(), 23)],
            "the 23 held back, and no row for an IP that held nothing"
        );
        assert!(throttle
            .take_held(t0 + RejectionThrottle::WINDOW, false)
            .is_empty());
        assert_eq!(
            throttle.admit("10.0.0.1", t0 + RejectionThrottle::WINDOW),
            Some(0),
            "the next attempt opens a fresh window"
        );
        throttle.admit("10.0.0.1", t0 + RejectionThrottle::WINDOW);
        assert_eq!(
            throttle.take_held(t0 + RejectionThrottle::WINDOW, true),
            vec![("10.0.0.1".to_owned(), 1)],
            "at shutdown, an open window is taken too"
        );
    }

    /// Past `SWEEP_AT`, the sweep drops elapsed entries but keeps one still
    /// holding attempts for the flusher.
    #[test]
    fn the_sweep_keeps_held_attempts_for_the_flusher() {
        let throttle = RejectionThrottle::default();
        let t0 = Instant::now();
        throttle.admit("held", t0);
        throttle.admit("held", t0);
        for i in 1..RejectionThrottle::SWEEP_AT {
            throttle.admit(&format!("ip{i}"), t0);
        }
        throttle.admit("late", t0 + RejectionThrottle::WINDOW);
        assert_eq!(
            throttle.take_held(t0 + RejectionThrottle::WINDOW, false),
            vec![("held".to_owned(), 1)]
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
