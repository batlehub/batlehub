//! Host-based (subdomain) registry routing — RFC 0001.
//!
//! The other `make_app*` helpers in `tests/common/mod.rs` deliberately do not
//! wrap the host-routing middleware (every other suite asserts on the subpath
//! ingress), so this file carries its own factory.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body, TestRequest};
use actix_web::App;
use utoipa_actix_web::AppExt;

use base64::Engine as _;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::rate_limit::InMemoryRateLimitStore;
use batlehub_config::schema::{RateLimitConfig, RateLimitEnforcement, RegistryMode};
use batlehub_core::ports::{CacheStore, PackageRepository, RegistryClient, StorageBackend};
use batlehub_core::services::{
    new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy,
};
use batlehub_web::{
    AuthMiddlewareFactory, HostRoutingMiddlewareFactory, ProxyTrust, RateLimitMiddlewareFactory,
    RateLimitService, RegistryHostMap, RegistryModeMap, RepoSignerMap,
};

// ── Factory ───────────────────────────────────────────────────────────────────

/// What the app under test should contain: which registries exist, which hosts
/// point at them, and which peers may set `X-Forwarded-Host`.
struct HostRoutedSpec<'a> {
    /// `(registry name, registry type)`.
    registries: &'a [(&'a str, &'a str)],
    /// `(normalised host, registry name)`.
    by_host: &'a [(&'a str, &'a str)],
    /// `(registry name, advertised public URL)`.
    public: &'a [(&'a str, &'a str)],
    /// Registries with `path_routing = false`.
    host_only: &'a [&'a str],
    /// `None` reproduces the legacy-permissive policy.
    trusted_proxies: Option<&'a [&'a str]>,
    /// `(registry name, requests per 60s window)`; empty means no rate limiting.
    rate_limits: &'a [(&'a str, u32)],
    /// Registries anonymous callers may see. All registries are always visible
    /// to authenticated users.
    anonymous: &'a [&'a str],
}

impl Default for HostRoutedSpec<'_> {
    fn default() -> Self {
        Self {
            registries: &[("npm1", "npm")],
            by_host: &[("npm.acme.io", "npm1"), ("npm1.hub.example.com", "npm1")],
            public: &[("npm1", "https://npm.acme.io")],
            host_only: &[],
            trusted_proxies: None,
            rate_limits: &[],
            anonymous: &[],
        }
    }
}

fn host_map(spec: &HostRoutedSpec<'_>) -> RegistryHostMap {
    RegistryHostMap::new(
        spec.by_host
            .iter()
            .map(|(h, r)| ((*h).to_owned(), (*r).to_owned()))
            .collect(),
        spec.public
            .iter()
            .map(|(r, u)| ((*r).to_owned(), (*u).to_owned()))
            .collect(),
        spec.host_only
            .iter()
            .map(|r| ((*r).to_owned(), true))
            .collect(),
    )
}

/// A full in-process app for `spec`, wrapped in the host-routing middleware.
///
/// All registries are in `Local` mode so the tests exercise the real handlers
/// (publish, then read back) without an upstream.
async fn make_host_routed_app(
    spec: HostRoutedSpec<'_>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<
        actix_web::body::EitherBody<actix_web::body::EitherBody<actix_web::body::BoxBody>>,
    >,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = spec
        .registries
        .iter()
        .map(|(name, kind)| {
            (
                (*name).to_owned(),
                FixedRegistry::new(*kind) as Arc<dyn RegistryClient>,
            )
        })
        .collect();
    let policies: HashMap<String, Arc<RegistryPolicy>> = spec
        .registries
        .iter()
        .map(|(name, _)| {
            (
                (*name).to_owned(),
                Arc::new(rbac_policy(repo_dyn.clone()).0),
            )
        })
        .collect();

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

    let names: Vec<&str> = spec.registries.iter().map(|(n, _)| *n).collect();
    let mode_map = RegistryModeMap::default();
    for name in &names {
        mode_map.insert((*name).to_owned(), RegistryMode::Local);
    }

    let map = host_map(&spec);
    let trust = match spec.trusted_proxies {
        Some(entries) => ProxyTrust::from_config(Some(
            &entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>(),
        )),
        None => ProxyTrust::legacy_permissive(),
    };

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
            Arc::new(AdminService::new(repo_dyn)),
            Arc::new(NullTokenRepository),
            access_config(spec.anonymous, &names),
            registry_map_for(spec.registries),
            ConfigureAppDefaults::default(),
        ))
        .split_for_parts();
    let app = app
        .app_data(actix_web::web::Data::new(
            batlehub_web::CargoIndexMap::default(),
        ))
        .app_data(actix_web::web::Data::new(local_svc))
        .app_data(actix_web::web::Data::new(mode_map))
        .app_data(actix_web::web::Data::new(RepoSignerMap::default()))
        .app_data(actix_web::web::Data::new(batlehub_web::VulnDbMap::default()))
        .app_data(actix_web::web::Data::new(map.clone()));

    let rate_limit_configs: HashMap<String, RateLimitConfig> = spec
        .rate_limits
        .iter()
        .map(|(name, per_window)| {
            (
                (*name).to_owned(),
                RateLimitConfig {
                    requests_per_window: *per_window,
                    window_secs: 60,
                    enforcement: RateLimitEnforcement::Block,
                    groups: vec![],
                },
            )
        })
        .collect();
    let rate_limit_svc = Arc::new(RateLimitService::new(
        &rate_limit_configs,
        Arc::new(InMemoryRateLimitStore::new()),
    ));

    init_service(
        app.wrap(RateLimitMiddlewareFactory::new(rate_limit_svc))
            .wrap(AuthMiddlewareFactory::new(test_auth_providers()))
            // Outermost, exactly as `server_factory` registers it.
            .wrap(HostRoutingMiddlewareFactory::new(map, trust)),
    )
    .await
}

/// The wire payload `npm publish` sends.
fn npm_publish_payload(name: &str, version: &str) -> serde_json::Value {
    let tarball = base64::engine::general_purpose::STANDARD.encode(b"fake-tarball-content");
    serde_json::json!({
        "name": name,
        "versions": {
            version: { "name": name, "version": version, "dist": { "shasum": "abc123" } }
        },
        "_attachments": {
            format!("{name}-{version}.tgz"): {
                "content_type": "application/octet-stream",
                "data": tarball,
                "length": 20,
            }
        }
    })
}

// ── Host request ≡ subpath request ────────────────────────────────────────────

#[actix_web::test]
async fn a_host_request_and_a_subpath_request_return_the_same_body() {
    let app = make_host_routed_app(HostRoutedSpec::default()).await;

    let publish = TestRequest::put()
        .uri("/proxy/npm1/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("lodash", "4.17.21"))
        .to_request();
    assert_eq!(call_service(&app, publish).await.status(), 200);

    // The tarball carries no self-referencing URL, so the two ingresses must
    // agree byte for byte. (Documents that *do* embed one differ by design —
    // each advertises the origin its client used; see the generated-URL tests.)
    let via_path = TestRequest::get()
        .uri("/proxy/npm1/lodash/4.17.21/tarball")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let via_path = call_service(&app, via_path).await;
    assert_eq!(via_path.status(), 200);
    let via_path = read_body(via_path).await;

    let via_host = TestRequest::get()
        .uri("/lodash/4.17.21/tarball")
        .insert_header(("host", "npm.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let via_host = call_service(&app, via_host).await;
    assert_eq!(via_host.status(), 200);
    let via_host = read_body(via_host).await;

    assert_eq!(via_path, via_host);
    assert_eq!(via_path, "fake-tarball-content");
}

#[actix_web::test]
async fn a_wildcard_host_and_a_vanity_host_reach_the_same_registry() {
    let app = make_host_routed_app(HostRoutedSpec::default()).await;

    let publish = TestRequest::put()
        .uri("/proxy/npm1/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("lodash", "1.0.0"))
        .to_request();
    call_service(&app, publish).await;

    for host in ["npm.acme.io", "npm1.hub.example.com"] {
        let req = TestRequest::get()
            .uri("/lodash/1.0.0")
            .insert_header(("host", host))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 200, "host {host}");
    }
}

// ── Host exclusivity (§4.4) ───────────────────────────────────────────────────

#[actix_web::test]
async fn a_cargo_host_serves_the_publish_api_but_not_the_admin_api() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("cargo1", "cargo")],
        by_host: &[("cargo1.hub.example.com", "cargo1")],
        public: &[("cargo1", "https://cargo1.hub.example.com")],
        ..Default::default()
    })
    .await;

    // `cargo publish` PUTs a length-prefixed body to /api/v1/crates/new. A
    // truncated body is fine here: reaching the handler at all is the point, and
    // it is a 4xx that is *not* 404-from-the-router.
    let publish = TestRequest::put()
        .uri("/api/v1/crates/new")
        .insert_header(("host", "cargo1.hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(vec![0u8; 8])
        .to_request();
    let status = call_service(&app, publish).await.status();
    assert!(
        status.is_client_error() && status != 404,
        "cargo publish must reach the registry handler on its host, got {status}"
    );

    // …while the admin API does not exist there: it becomes
    // /proxy/cargo1/api/v1/registries, which no route matches.
    let admin = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("host", "cargo1.hub.example.com"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, admin).await.status(), 404);
}

#[actix_web::test]
async fn an_unknown_host_still_serves_the_admin_api_and_the_subpath() {
    let app = make_host_routed_app(HostRoutedSpec::default()).await;

    let admin = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, admin).await.status(), 200);

    let publish = TestRequest::put()
        .uri("/proxy/npm1/on-main-host")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("on-main-host", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, publish).await.status(), 200);
}

#[actix_web::test]
async fn the_subpath_on_a_registry_host_double_prefixes_and_404s() {
    // Documented corollary of §4.4: pick one ingress per client.
    let app = make_host_routed_app(HostRoutedSpec::default()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm1/lodash")
        .insert_header(("host", "npm.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── Authorisation is unchanged by the ingress (§7) ────────────────────────────

#[actix_web::test]
async fn an_unauthenticated_publish_is_refused_identically_on_both_ingresses() {
    let app = make_host_routed_app(HostRoutedSpec::default()).await;

    let via_path = TestRequest::put()
        .uri("/proxy/npm1/pkg")
        .insert_header(("host", "hub.example.com"))
        .set_json(npm_publish_payload("pkg", "1.0.0"))
        .to_request();
    let path_status = call_service(&app, via_path).await.status();

    let via_host = TestRequest::put()
        .uri("/pkg")
        .insert_header(("host", "npm.acme.io"))
        .set_json(npm_publish_payload("pkg", "1.0.0"))
        .to_request();
    let host_status = call_service(&app, via_host).await.status();

    assert_eq!(
        path_status, host_status,
        "routing must not change the authorisation outcome"
    );
    assert!(
        path_status.is_client_error(),
        "expected a refusal, got {path_status}"
    );
}

// ── Proxy trust (§4.5) ────────────────────────────────────────────────────────

#[actix_web::test]
async fn a_spoofed_forwarded_host_from_an_untrusted_peer_does_not_route() {
    let app = make_host_routed_app(HostRoutedSpec {
        trusted_proxies: Some(&["10.42.0.0/16"]),
        ..Default::default()
    })
    .await;

    // The forwarded header names a registry host, but the peer is not the
    // ingress — so the request stays on the main host and 404s at the router.
    let req = TestRequest::get()
        .uri("/lodash")
        .peer_addr("203.0.113.9:1234".parse().unwrap())
        .insert_header(("host", "hub.example.com"))
        .insert_header(("x-forwarded-host", "npm.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn the_same_forwarded_host_from_a_trusted_peer_routes() {
    let app = make_host_routed_app(HostRoutedSpec {
        trusted_proxies: Some(&["10.42.0.0/16"]),
        ..Default::default()
    })
    .await;

    let publish = TestRequest::put()
        .uri("/proxy/npm1/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("lodash", "1.0.0"))
        .to_request();
    call_service(&app, publish).await;

    let req = TestRequest::get()
        .uri("/lodash/1.0.0")
        .peer_addr("10.42.7.1:1234".parse().unwrap())
        .insert_header(("host", "hub.example.com"))
        .insert_header(("x-forwarded-host", "npm.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn an_empty_trusted_proxy_list_ignores_forwarded_headers_entirely() {
    let app = make_host_routed_app(HostRoutedSpec {
        trusted_proxies: Some(&[]),
        ..Default::default()
    })
    .await;

    let req = TestRequest::get()
        .uri("/lodash")
        .peer_addr("10.42.0.1:1234".parse().unwrap())
        .insert_header(("host", "hub.example.com"))
        .insert_header(("x-forwarded-host", "npm.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── path_routing = false (§4.6) ───────────────────────────────────────────────

fn host_only_spec() -> HostRoutedSpec<'static> {
    HostRoutedSpec {
        registries: &[("npm1", "npm"), ("npm2", "npm")],
        by_host: &[("private.acme.io", "npm1"), ("shared.acme.io", "npm2")],
        public: &[
            ("npm1", "https://private.acme.io"),
            ("npm2", "https://shared.acme.io"),
        ],
        host_only: &["npm1"],
        trusted_proxies: None,
        rate_limits: &[],
        anonymous: &[],
    }
}

#[actix_web::test]
async fn a_host_only_registry_is_unreachable_by_path_but_reachable_by_host() {
    let app = make_host_routed_app(host_only_spec()).await;

    let via_path = TestRequest::put()
        .uri("/proxy/npm1/pkg")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("pkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, via_path).await.status(), 404);

    let via_host = TestRequest::put()
        .uri("/pkg")
        .insert_header(("host", "private.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("pkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, via_host).await.status(), 200);
}

#[actix_web::test]
async fn a_sibling_registry_keeps_its_subpath() {
    let app = make_host_routed_app(host_only_spec()).await;
    let req = TestRequest::put()
        .uri("/proxy/npm2/pkg")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("pkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn the_disabled_subpath_is_indistinguishable_from_an_unknown_registry() {
    let app = make_host_routed_app(host_only_spec()).await;

    let disabled = TestRequest::get()
        .uri("/proxy/npm1/pkg/1.0.0")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let disabled = call_service(&app, disabled).await;
    assert_eq!(disabled.status(), 404);

    let unknown = TestRequest::get()
        .uri("/proxy/does-not-exist/pkg/1.0.0")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let unknown = call_service(&app, unknown).await;
    assert_eq!(unknown.status(), 404);
    // 404, not 403: the closed ingress reveals nothing about whether the
    // registry exists.
    assert_eq!(disabled.status(), unknown.status());
}

// ── Rate limiting keys on the registry, not the ingress ───────────────────────

#[actix_web::test]
async fn the_rate_limiter_keys_host_routed_requests_on_the_registry() {
    // The limiter reads `/proxy/{registry}/…` from the path. Host-routed requests
    // have already been rewritten to that shape by the time it runs, so the two
    // ingresses must share one bucket rather than each getting a fresh budget.
    let app = make_host_routed_app(HostRoutedSpec {
        rate_limits: &[("npm1", 2)],
        ..Default::default()
    })
    .await;

    let via_path = || {
        TestRequest::get()
            .uri("/proxy/npm1/lodash")
            .insert_header(("host", "hub.example.com"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request()
    };
    let via_host = || {
        TestRequest::get()
            .uri("/lodash")
            .insert_header(("host", "npm.acme.io"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request()
    };

    // Two requests, one per ingress, exhaust the shared budget of 2.
    assert_ne!(call_service(&app, via_path()).await.status(), 429);
    assert_ne!(call_service(&app, via_host()).await.status(), 429);

    assert_eq!(
        call_service(&app, via_host()).await.status(),
        429,
        "a host-routed request must draw from the same bucket as the subpath"
    );
}

// ── Generated URLs are rooted at the ingress the client used (§5.3) ───────────

/// Read a JSON body, or panic with the status when the request failed.
async fn json_ok(
    resp: actix_web::dev::ServiceResponse<
        actix_web::body::EitherBody<actix_web::body::EitherBody<actix_web::body::BoxBody>>,
    >,
) -> serde_json::Value {
    assert!(
        resp.status().is_success(),
        "unexpected status {}",
        resp.status()
    );
    let body = read_body(resp).await;
    serde_json::from_slice(&body).expect("json body")
}

/// Every generated URL in `body` must start with `expected_base` and contain no
/// `/proxy/` segment when the request was host-routed.
fn assert_rooted_at(body: &serde_json::Value, expected_base: &str, host_routed: bool) {
    let text = body.to_string();
    assert!(
        text.contains(expected_base),
        "expected URLs under {expected_base}, got {text}"
    );
    if host_routed {
        assert!(
            !text.contains("/proxy/"),
            "a host-routed response must not advertise a /proxy/ path: {text}"
        );
    }
}

#[actix_web::test]
async fn npm_dist_tarball_points_at_the_ingress_used() {
    let app = make_host_routed_app(HostRoutedSpec::default()).await;
    let publish = TestRequest::put()
        .uri("/proxy/npm1/lodash")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("lodash", "4.17.21"))
        .to_request();
    call_service(&app, publish).await;

    let via_host = TestRequest::get()
        .uri("/lodash/4.17.21")
        .insert_header(("host", "npm.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_host).await).await;
    assert_eq!(
        body["dist"]["tarball"],
        "http://npm.acme.io/lodash/4.17.21/tarball"
    );

    let via_path = TestRequest::get()
        .uri("/proxy/npm1/lodash/4.17.21")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_path).await).await;
    assert_eq!(
        body["dist"]["tarball"], "http://hub.example.com/proxy/npm1/lodash/4.17.21/tarball",
        "the subpath ingress must be byte-identical to pre-RFC behaviour"
    );
}

#[actix_web::test]
async fn the_nuget_service_index_is_rooted_at_the_ingress_used() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("nuget1", "nuget")],
        by_host: &[("nuget.acme.io", "nuget1")],
        public: &[("nuget1", "https://nuget.acme.io")],
        ..Default::default()
    })
    .await;

    let via_host = TestRequest::get()
        .uri("/nuget/v3/index.json")
        .insert_header(("host", "nuget.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_host).await).await;
    assert_rooted_at(&body, "http://nuget.acme.io/nuget/v3/", true);

    let via_path = TestRequest::get()
        .uri("/proxy/nuget1/nuget/v3/index.json")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_path).await).await;
    assert_rooted_at(
        &body,
        "http://hub.example.com/proxy/nuget1/nuget/v3/",
        false,
    );
}

#[actix_web::test]
async fn the_cargo_index_config_is_rooted_at_the_ingress_used() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("cargo1", "cargo")],
        by_host: &[("cargo.acme.io", "cargo1")],
        public: &[("cargo1", "https://cargo.acme.io")],
        ..Default::default()
    })
    .await;

    let via_host = TestRequest::get()
        .uri("/registry/config.json")
        .insert_header(("host", "cargo.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_host).await).await;
    assert_eq!(
        body["dl"],
        "http://cargo.acme.io/{crate}/{version}/download"
    );
    assert_eq!(body["api"], "http://cargo.acme.io");

    let via_path = TestRequest::get()
        .uri("/proxy/cargo1/registry/config.json")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_path).await).await;
    assert_eq!(
        body["dl"],
        "http://hub.example.com/proxy/cargo1/{crate}/{version}/download"
    );
    assert_eq!(body["api"], "http://hub.example.com/proxy/cargo1");
}

#[actix_web::test]
async fn the_composer_metadata_url_is_rooted_at_the_ingress_used() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("composer1", "composer")],
        by_host: &[("composer.acme.io", "composer1")],
        public: &[("composer1", "https://composer.acme.io")],
        ..Default::default()
    })
    .await;

    let via_host = TestRequest::get()
        .uri("/packages.json")
        .insert_header(("host", "composer.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_host).await).await;
    assert_eq!(
        body["metadata-url"],
        "http://composer.acme.io/p2/%package%.json"
    );

    let via_path = TestRequest::get()
        .uri("/proxy/composer1/packages.json")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_path).await).await;
    assert_eq!(
        body["metadata-url"],
        "http://hub.example.com/proxy/composer1/p2/%package%.json"
    );
}

#[actix_web::test]
async fn the_terraform_provider_download_url_is_rooted_at_the_ingress_used() {
    const MANIFEST: &str = r#"{
      "version": "5.0.0",
      "protocols": ["5.0"],
      "platforms": [
        {"os": "linux", "arch": "amd64", "filename": "p.zip", "shasum": "deadbeef"}
      ]
    }"#;

    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("tf1", "terraform")],
        by_host: &[("tf.acme.io", "tf1")],
        public: &[("tf1", "https://tf.acme.io")],
        ..Default::default()
    })
    .await;

    let upload = TestRequest::post()
        .uri("/proxy/tf1/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(MANIFEST)
        .to_request();
    assert_eq!(call_service(&app, upload).await.status(), 201);

    let via_host = TestRequest::get()
        .uri("/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("host", "tf.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_host).await).await;
    assert_eq!(
        body["download_url"],
        "http://tf.acme.io/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64"
    );

    let via_path = TestRequest::get()
        .uri("/proxy/tf1/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, via_path).await).await;
    assert_eq!(
        body["download_url"],
        "http://hub.example.com/proxy/tf1/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64"
    );
}

#[actix_web::test]
async fn the_pypi_simple_index_is_rooted_at_the_ingress_used() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("pypi1", "pypi")],
        by_host: &[("pypi.acme.io", "pypi1")],
        public: &[("pypi1", "https://pypi.acme.io")],
        ..Default::default()
    })
    .await;

    let (body, content_type) = pypi_publish_body("my-pkg", "1.0.0");
    let publish = TestRequest::post()
        .uri("/proxy/pypi1/legacy/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    assert!(call_service(&app, publish).await.status().is_success());

    let via_host = TestRequest::get()
        .uri("/simple/my-pkg/")
        .insert_header(("host", "pypi.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let html = read_body(call_service(&app, via_host).await).await;
    let html = String::from_utf8(html.to_vec()).unwrap();
    assert!(
        html.contains("http://pypi.acme.io/packages/my-pkg-1.0.0.tar.gz"),
        "{html}"
    );
    assert!(!html.contains("/proxy/"), "{html}");

    let via_path = TestRequest::get()
        .uri("/proxy/pypi1/simple/my-pkg/")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let html = read_body(call_service(&app, via_path).await).await;
    let html = String::from_utf8(html.to_vec()).unwrap();
    assert!(
        html.contains("http://hub.example.com/proxy/pypi1/packages/my-pkg-1.0.0.tar.gz"),
        "{html}"
    );
}

/// A `twine upload`-style `multipart/form-data` body.
fn pypi_publish_body(name: &str, version: &str) -> (Vec<u8>, String) {
    let boundary = "pypiboundary";
    let mut body = Vec::new();
    for (field, value) in [
        (":action", "file_upload"),
        ("name", name),
        ("version", version),
    ] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"content\"; \
             filename=\"{name}-{version}.tar.gz\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"fake-pypi-sdist-content");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

// ── path_routing = false: links must be absolute (§4.6) ───────────────────────

#[actix_web::test]
async fn a_host_only_registry_roots_generated_urls_at_its_own_host() {
    // Nothing can reach `/proxy/npm1/…`, so the only origin a generated URL may
    // carry is the host the client actually used.
    let app = make_host_routed_app(host_only_spec()).await;

    let publish = TestRequest::put()
        .uri("/pkg")
        .insert_header(("host", "private.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(npm_publish_payload("pkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, publish).await.status(), 200);

    let req = TestRequest::get()
        .uri("/pkg/1.0.0")
        .insert_header(("host", "private.acme.io"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, req).await).await;
    assert_eq!(
        body["dist"]["tarball"],
        "http://private.acme.io/pkg/1.0.0/tarball"
    );
}

// ── `public_url` on GET /api/v1/registries (§6.6, §11) ────────────────────────

#[actix_web::test]
async fn the_registries_listing_advertises_the_public_url() {
    let app = make_host_routed_app(HostRoutedSpec::default()).await;
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("host", "hub.example.com"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, req).await).await;
    assert_eq!(body[0]["name"], "npm1");
    assert_eq!(body[0]["public_url"], "https://npm.acme.io");
}

#[actix_web::test]
async fn a_registry_with_no_host_has_no_public_url() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("npm1", "npm")],
        by_host: &[],
        public: &[],
        ..Default::default()
    })
    .await;
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, req).await).await;
    assert_eq!(body[0]["name"], "npm1");
    assert!(
        body[0].get("public_url").is_none(),
        "the field is omitted, so a client falls back to /proxy/{{name}}: {body}"
    );
}

#[actix_web::test]
async fn a_host_only_registry_still_appears_in_the_listing() {
    let app = make_host_routed_app(host_only_spec()).await;
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = json_ok(call_service(&app, req).await).await;
    let npm1 = body
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "npm1")
        .expect("a registry with no subpath is still listed");
    assert_eq!(npm1["public_url"], "https://private.acme.io");
}

#[actix_web::test]
async fn an_anonymous_caller_sees_the_public_url_of_registries_it_may_reach() {
    let app = make_host_routed_app(HostRoutedSpec {
        registries: &[("npm1", "npm"), ("npm2", "npm")],
        by_host: &[("npm.acme.io", "npm1"), ("private.acme.io", "npm2")],
        public: &[
            ("npm1", "https://npm.acme.io"),
            ("npm2", "https://private.acme.io"),
        ],
        anonymous: &["npm1"],
        ..Default::default()
    })
    .await;

    let req = TestRequest::get().uri("/api/v1/registries").to_request();
    let body = json_ok(call_service(&app, req).await).await;
    let listed = body.as_array().unwrap();

    assert_eq!(listed.len(), 1, "the filter, not the field, is the gate");
    assert_eq!(listed[0]["name"], "npm1");
    assert_eq!(listed[0]["public_url"], "https://npm.acme.io");
    // The restricted registry is absent entirely, so its host is never disclosed.
    assert!(!body.to_string().contains("private.acme.io"));
}
