//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body_json, TestRequest};
use actix_web::App;
use serde_json::Value;
use utoipa_actix_web::AppExt;

use batlehub_adapters::auth::StaticTokenAuthProvider;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::rate_limit::InMemoryRateLimitStore;
use batlehub_config::schema::{GroupRateLimitConfig, RateLimitConfig, RateLimitEnforcement};
use batlehub_core::{
    entities::Role,
    ports::{
        AuthProvider, CacheStore, PackageRepository, RegistryClient, StorageBackend,
        UserTokenRepository,
    },
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::{
    AuthMiddlewareFactory, RateLimitMiddlewareFactory, RateLimitService, RegistryModeMap,
};

// ── Rate-limited app factory ──────────────────────────────────────────────────

const GROUP_TOKEN_1: &str = "group-token-1";
const GROUP_TOKEN_2: &str = "group-token-2";
const GROUP_NAME: &str = "ci-bots";

fn test_auth_providers_with_groups() -> Vec<Arc<dyn AuthProvider>> {
    vec![Arc::new(
        StaticTokenAuthProvider::new([
            (
                ADMIN_TOKEN.to_owned(),
                Some("admin".to_owned()),
                Role::Admin,
            ),
            (USER_TOKEN.to_owned(), Some("user-1".to_owned()), Role::User),
        ])
        .with_group_entries([
            (
                GROUP_TOKEN_1.to_owned(),
                Some("group-user-1".to_owned()),
                Role::User,
                vec![GROUP_NAME.to_owned()],
            ),
            (
                GROUP_TOKEN_2.to_owned(),
                Some("group-user-2".to_owned()),
                Role::User,
                vec![GROUP_NAME.to_owned()],
            ),
        ]),
    )]
}

/// Build a fully-wired test app with both auth and rate-limiting middleware.
///
/// Middleware execution order (last registered = outermost = first to run):
///   auth (outermost) → rate_limit → handlers
/// This ensures Identity is set by auth before rate limiting reads it.
async fn make_rate_limited_app(
    rl_svc: Arc<RateLimitService>,
    auth_providers: Vec<Arc<dyn AuthProvider>>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<
        actix_web::body::EitherBody<actix_web::body::BoxBody>,
    >,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "npm".to_owned(),
        FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
    )]
    .into();

    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [("npm".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0))].into();

    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);

    let access_config = access_config_for(&["npm"]);
    let registry_map = registry_map_for(&[("npm", "npm")]);

    let (app, _) = App::new()
        // RFC 0015 §4.2 — this app registers only the handlers under test, so the
        // hot lock the control-verb check reads has to be registered with them.
        .app_data(actix_web::web::Data::new(
            batlehub_core::services::hot_config::new_hot_lock(
                batlehub_core::services::hot_config::HotConfig {
                    instance: Some(std::sync::Arc::new(
                        batlehub_core::services::authz::translate::instance_node(None),
                    )),
                    ..Default::default()
                },
            ),
        ))
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            admin_svc,
            token_repo,
            access_config,
            registry_map,
            ConfigureAppDefaults::default(),
        ))
        .split_for_parts();
    let app = app
        .app_data(actix_web::web::Data::new(
            batlehub_web::CargoIndexMap::default(),
        ))
        .app_data(actix_web::web::Data::new(local_svc))
        .app_data(actix_web::web::Data::new(RegistryModeMap::default()));

    // Auth (outer) must run before rate limiting (inner) so Identity is set.
    init_service(
        app.wrap(RateLimitMiddlewareFactory::new(rl_svc))
            .wrap(AuthMiddlewareFactory::new(auth_providers)),
    )
    .await
}

fn block_rl_svc(registry: &str, requests_per_window: u32) -> Arc<RateLimitService> {
    let store = Arc::new(InMemoryRateLimitStore::new());
    let cfg = RateLimitConfig {
        requests_per_window,
        window_secs: 60,
        enforcement: RateLimitEnforcement::Block,
        groups: vec![],
    };
    let mut m = HashMap::new();
    m.insert(registry.to_owned(), cfg);
    Arc::new(RateLimitService::new(&m, store))
}

fn warn_rl_svc(registry: &str, requests_per_window: u32) -> Arc<RateLimitService> {
    let store = Arc::new(InMemoryRateLimitStore::new());
    let cfg = RateLimitConfig {
        requests_per_window,
        window_secs: 60,
        enforcement: RateLimitEnforcement::Warn,
        groups: vec![],
    };
    let mut m = HashMap::new();
    m.insert(registry.to_owned(), cfg);
    Arc::new(RateLimitService::new(&m, store))
}

fn group_rl_svc(
    registry: &str,
    user_limit: u32,
    group: &str,
    group_limit: u32,
) -> Arc<RateLimitService> {
    let store = Arc::new(InMemoryRateLimitStore::new());
    let cfg = RateLimitConfig {
        requests_per_window: user_limit,
        window_secs: 60,
        enforcement: RateLimitEnforcement::Block,
        groups: vec![GroupRateLimitConfig {
            name: group.to_owned(),
            requests_per_window: group_limit,
            window_secs: 60,
            enforcement: None,
        }],
    };
    let mut m = HashMap::new();
    m.insert(registry.to_owned(), cfg);
    Arc::new(RateLimitService::new(&m, store))
}

// ── Rate limiting integration tests ──────────────────────────────────────────

#[actix_web::test]
async fn non_proxy_route_is_never_rate_limited() {
    // /api/v1/me is not under /proxy/... so the rate limit middleware must pass it through
    // even when the limit is 0 (which would block every proxy request).
    let rl_svc = block_rl_svc("npm", 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    // Exhaust the npm limit.
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    call_service(&app, req).await;

    // Non-proxy route must still be 200 (anonymous = no auth needed for /me).
    let req = TestRequest::get().uri("/api/v1/me").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "/api/v1/me must never be rate limited");
    assert!(
        resp.headers().get("x-ratelimit-limit").is_none(),
        "non-proxy routes must not carry X-RateLimit-Limit"
    );
}

#[actix_web::test]
async fn requests_below_limit_succeed_with_ratelimit_header() {
    let rl_svc = block_rl_svc("npm", 5);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    for _ in 0..5 {
        let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "requests under the limit must succeed");
        assert!(
            resp.headers().get("x-ratelimit-limit").is_some(),
            "allowed responses must carry X-RateLimit-Limit"
        );
    }
}

#[actix_web::test]
async fn request_over_limit_returns_429() {
    let rl_svc = block_rl_svc("npm", 3);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    for _ in 0..3 {
        let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 429, "4th request must be rate limited");
}

#[actix_web::test]
async fn a_percent_encoded_registry_name_shares_the_same_bucket() {
    // actix requotes the path before routing, so `/proxy/np%6d/…` reaches the
    // `npm` handler. Keying the bucket on the raw URI would make that a free
    // opt-out from the registry's rate limit.
    let rl_svc = block_rl_svc("npm", 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::get().uri("/proxy/np%6d/lodash").to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        429,
        "percent-encoding the registry name must not bypass its rate limit"
    );
}

#[actix_web::test]
async fn block_mode_response_carries_required_headers() {
    let rl_svc = block_rl_svc("npm", 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    // First request succeeds; second is blocked.
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    call_service(&app, req).await;

    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 429);

    let retry_after = resp
        .headers()
        .get("retry-after")
        .expect("429 must carry Retry-After")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(retry_after >= 1, "Retry-After must be at least 1 second");

    let reset_ts = resp
        .headers()
        .get("x-ratelimit-reset")
        .expect("429 must carry X-RateLimit-Reset")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(reset_ts > now, "X-RateLimit-Reset must be in the future");

    let limit = resp
        .headers()
        .get("x-ratelimit-limit")
        .expect("429 must carry X-RateLimit-Limit")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(limit, 1);
}

#[actix_web::test]
async fn block_mode_response_body_is_json_with_error_field() {
    let rl_svc = block_rl_svc("npm", 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    call_service(&app, req).await;

    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 429);

    let body: Value = read_body_json(resp).await;
    assert_eq!(body["error"], "Too Many Requests");
    assert!(body["message"]
        .as_str()
        .map(|m| m.contains("retry after"))
        .unwrap_or(false));
}

#[actix_web::test]
async fn warn_mode_over_limit_still_returns_200() {
    let rl_svc = warn_rl_svc("npm", 2);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    // Exhaust limit.
    for _ in 0..2 {
        let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
        call_service(&app, req).await;
    }

    // Over-limit request must still return 200 in warn mode.
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "warn mode must not block the request");
}

#[actix_web::test]
async fn warn_mode_sets_warning_headers_on_over_limit() {
    let rl_svc = warn_rl_svc("npm", 2);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    for _ in 0..2 {
        let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
        call_service(&app, req).await;
    }

    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let warning = resp
        .headers()
        .get("x-ratelimit-warning")
        .expect("over-limit warn response must carry X-RateLimit-Warning")
        .to_str()
        .unwrap();
    assert_eq!(warning, "rate-limit-exceeded");

    assert!(
        resp.headers().get("x-ratelimit-limit").is_some(),
        "must carry X-RateLimit-Limit"
    );
    assert!(
        resp.headers().get("retry-after").is_some(),
        "must carry Retry-After"
    );
}

#[actix_web::test]
async fn anonymous_request_is_rate_limited_by_ip() {
    // Anonymous requests (no Authorization header) fall back to ip-based bucketing.
    let rl_svc = block_rl_svc("npm", 2);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    // Two requests without auth = ip bucket = allowed.
    for _ in 0..2 {
        let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    // Third anonymous request = ip bucket exhausted = 429.
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        429,
        "anonymous request must be blocked after limit"
    );
}

#[actix_web::test]
async fn authenticated_user_has_separate_bucket_from_anonymous() {
    // Exhaust the anonymous (IP) bucket, then verify an authenticated user is unaffected.
    let rl_svc = block_rl_svc("npm", 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    // Exhaust anonymous bucket.
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    call_service(&app, req).await;
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let anon_resp = call_service(&app, req).await;
    assert_eq!(
        anon_resp.status(),
        429,
        "anonymous bucket should be exhausted"
    );

    // Authenticated user has a separate bucket → first request succeeds.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let auth_resp = call_service(&app, req).await;
    assert_eq!(
        auth_resp.status(),
        200,
        "authenticated user must have an independent bucket"
    );
}

#[actix_web::test]
async fn two_different_users_have_independent_buckets() {
    let rl_svc = block_rl_svc("npm", 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    // user-1 exhausts its bucket.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    call_service(&app, req).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let user1_resp = call_service(&app, req).await;
    assert_eq!(
        user1_resp.status(),
        429,
        "user-1 must be blocked after limit"
    );

    // admin has a different user_id → its bucket is untouched.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let admin_resp = call_service(&app, req).await;
    assert_eq!(
        admin_resp.status(),
        200,
        "admin must have an independent bucket"
    );
}

#[actix_web::test]
async fn group_shared_pool_is_counted_across_members() {
    // Group limit = 2, user limit = 100 (high enough not to interfere).
    let rl_svc = group_rl_svc("npm", 100, GROUP_NAME, 2);
    let app = make_rate_limited_app(rl_svc, test_auth_providers_with_groups()).await;

    // Member 1 takes first slot.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(GROUP_TOKEN_1)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "first group request must succeed");

    // Member 2 takes second slot.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(GROUP_TOKEN_2)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "second group request must succeed");

    // Member 1 again — group pool is now exhausted.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(GROUP_TOKEN_1)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        429,
        "group pool exhausted — third request must be blocked"
    );
}

#[actix_web::test]
async fn non_group_member_is_unaffected_by_group_limit() {
    // Group limit = 1, user limit = 100.
    let rl_svc = group_rl_svc("npm", 100, GROUP_NAME, 1);
    let app = make_rate_limited_app(rl_svc, test_auth_providers_with_groups()).await;

    // Exhaust the group pool.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(GROUP_TOKEN_1)))
        .to_request();
    call_service(&app, req).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(GROUP_TOKEN_1)))
        .to_request();
    let group_resp = call_service(&app, req).await;
    assert_eq!(group_resp.status(), 429, "group pool must be exhausted");

    // Regular user (not in the group) must be unaffected.
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let user_resp = call_service(&app, req).await;
    assert_eq!(
        user_resp.status(),
        200,
        "non-group member must not be blocked by group limit"
    );
}

#[actix_web::test]
async fn registry_without_rate_limit_config_passes_through_freely() {
    // Rate limit is configured only for "npm"; no other registry is listed.
    // The test app only has "npm" registered anyway, but we verify no X-RateLimit-Limit
    // header is present when there's no configured limit for the registry in question.
    let store = Arc::new(InMemoryRateLimitStore::new());
    // Use an empty config map — no registry has any limit.
    let rl_svc = Arc::new(RateLimitService::new(&HashMap::new(), store));
    let app = make_rate_limited_app(rl_svc, test_auth_providers()).await;

    for _ in 0..20 {
        let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            200,
            "unconfigured registry must never be rate limited"
        );
        assert!(
            resp.headers().get("x-ratelimit-limit").is_none(),
            "unconfigured registry must not emit X-RateLimit-Limit"
        );
    }
}
