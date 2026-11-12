//! The mise wire's contract (RFC 0045 §6.6, §14).
//!
//! `_wire` is a rewrite, not a route, so it has no `authz_matrix.rs` row: this
//! file is where its behaviour is pinned — each outcome, the security rows
//! (T1–T3, T9–T11), the overlay archive and `/api/v1/mise/auto-wire`.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body, TestRequest};
use actix_web::App;
use utoipa_actix_web::AppExt;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::registry::mise_registry_overlay as overlay;
use batlehub_config::schema::{AppConfig, RegistryMode};
use batlehub_core::entities::{RegistryGrants, RegistryKind};
use batlehub_core::ports::{CacheStore, PackageRepository, RegistryClient, StorageBackend};
use batlehub_core::services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService};
use batlehub_web::mise_wire::{MiseWire, WiredFrom};
use batlehub_web::{
    AuthMiddlewareFactory, HostRoutingMiddlewareFactory, ProxyTrust, RegistryHostMap,
    RegistryModeMap, RepoSignerMap, UpstreamMap,
};

const MV: &str = "mv";

fn config(registries: &str) -> AppConfig {
    toml::from_str(&format!(
        r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"
        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"
        [server]
        host = "127.0.0.1"
        port = 8080
        {registries}
        "#
    ))
    .expect("test config parses")
}

/// One `github`, two `generic` mirrors on one host (§15 Q10) and the wire.
fn base_registries(extra_mv: &str, upstream: &str) -> String {
    format!(
        r#"
        [[registries]]
        name = "gh"
        type = "github"
        upstreams = ["https://api.github.com"]

        [[registries]]
        name = "gen-a"
        type = "generic"
        upstreams = ["https://files.example.com/a"]
        path_allow = ["**"]

        [[registries]]
        name = "gen-b"
        type = "generic"
        upstreams = ["https://files.example.com/b"]
        path_allow = ["**"]

        [[registries]]
        name = "mv"
        type = "mise-versions"
          [registries.auto_wire]
          enabled = true
          registry_upstream = "{upstream}"
          {extra_mv}
        "#
    )
}

fn upstreams(cfg: &AppConfig) -> UpstreamMap {
    UpstreamMap::new(
        cfg.registries
            .iter()
            .filter_map(|r| r.upstreams.first().map(|u| (r.name.clone(), u.clone())))
            .collect(),
    )
    .with_all(
        cfg.registries
            .iter()
            .map(|r| (r.name.clone(), r.upstreams.clone()))
            .collect(),
    )
}

struct Lab {
    cfg: AppConfig,
    /// Registries anonymous callers may not read.
    closed: Vec<&'static str>,
    air_gap: bool,
}

async fn make_app(
    lab: Lab,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
    Error = actix_web::Error,
> {
    make_app_with(lab, InMemoryStorage::new()).await
}

/// A lab's proxy service, its repository, and each registry's `(name, kind)`.
type LabProxy = (
    Arc<ProxyService>,
    Arc<dyn PackageRepository>,
    Vec<(String, String)>,
);

/// The proxy service of `lab` over `storage`, the registry kinds, and the repo.
fn proxy_for(lab: &Lab, storage: Arc<dyn StorageBackend>) -> LabProxy {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let kinds: Vec<(String, String)> = lab
        .cfg
        .registries
        .iter()
        .map(|r| (r.name.clone(), r.registry_type.clone()))
        .collect();
    let registries: HashMap<String, Arc<dyn RegistryClient>> = kinds
        .iter()
        .map(|(n, k)| {
            (
                n.clone(),
                FixedRegistry::new(k.as_str()) as Arc<dyn RegistryClient>,
            )
        })
        .collect();
    let policies = kinds
        .iter()
        .map(|(n, _)| {
            let mut p = rbac_policy(repo_dyn.clone()).0;
            p.serve_stale_metadata = true;
            (n.clone(), Arc::new(p))
        })
        .collect();
    let grants = lab
        .closed
        .iter()
        .map(|n| {
            let kind = kinds
                .iter()
                .find(|(name, _)| name == n)
                .and_then(|(_, k)| k.parse::<RegistryKind>().ok())
                .unwrap_or(RegistryKind::Generic);
            ((*n).to_owned(), Arc::new(RegistryGrants::empty(n, kind)))
        })
        .collect();
    let air_gap = batlehub_core::entities::AirGapPolicy {
        enabled: lab.air_gap,
        ..Default::default()
    };
    let hot = new_hot_lock(HotConfig {
        instance: Some(Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
        air_gap,
        ..Default::default()
    });
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
    (proxy_svc, repo_dyn, kinds)
}

/// [`make_app`] over a storage the test can seed, as a bundle import would.
async fn make_app_with(
    lab: Lab,
    storage: Arc<dyn StorageBackend>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
    Error = actix_web::Error,
> {
    let (proxy_svc, repo_dyn, kinds) = proxy_for(&lab, storage);
    let hot = proxy_svc.hot.clone();
    let local_svc = make_local_svc(hot.clone(), proxy_svc.storage.clone());
    let um = upstreams(&lab.cfg);
    let wire = MiseWire::build(&lab.cfg, &um).expect("the wire builds");
    // The config's own host bindings and opt-outs, as the server builds them,
    // so the middleware's registry-host branch is reachable (T11).
    let map = RegistryHostMap::from_app_config(&lab.cfg).with_wire(wire);
    let names: Vec<&str> = kinds.iter().map(|(n, _)| n.as_str()).collect();
    let anonymous: Vec<&str> = names
        .iter()
        .filter(|n| !lab.closed.contains(n))
        .copied()
        .collect();
    let pairs: Vec<(&str, &str)> = kinds
        .iter()
        .map(|(n, k)| (n.as_str(), k.as_str()))
        .collect();
    let mode_map = RegistryModeMap::default();
    for n in &names {
        mode_map.insert((*n).to_owned(), RegistryMode::Proxy);
    }
    let (app, _) = App::new()
        .app_data(actix_web::web::Data::new(hot.clone()))
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            Arc::new(AdminService::new(repo_dyn)),
            Arc::new(NullTokenRepository),
            access_config(&anonymous, &names),
            registry_map_for(&pairs),
            ConfigureAppDefaults {
                upstream_map: um,
                ..ConfigureAppDefaults::default()
            },
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
    init_service(
        app.wrap(AuthMiddlewareFactory::new(test_auth_providers()))
            .wrap(HostRoutingMiddlewareFactory::new(
                map,
                ProxyTrust::legacy_permissive(),
            )),
    )
    .await
}

fn gapped(cfg: String) -> String {
    format!("{cfg}\n        [air_gap]\n        enabled = true\n")
}

fn lab(extra_mv: &str, upstream: &str) -> Lab {
    Lab {
        cfg: config(&base_registries(extra_mv, upstream)),
        closed: Vec::new(),
        air_gap: false,
    }
}

fn wire(path: &str) -> String {
    format!("/proxy/{MV}/mise-versions/_wire/{path}")
}

fn get(uri: &str, token: Option<&str>) -> actix_http::Request {
    let mut r = TestRequest::get()
        .uri(uri)
        .insert_header(("host", "hub.example.com"));
    if let Some(t) = token {
        r = r.insert_header(("Authorization", bearer(t)));
    }
    r.to_request()
}

async fn body_of<B: actix_web::body::MessageBody>(
    resp: actix_web::dev::ServiceResponse<B>,
) -> String {
    String::from_utf8(read_body(resp).await.to_vec()).unwrap_or_default()
}

// ── Rewrites ─────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn a_wired_github_request_is_the_typed_request() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    let resp = call_service(
        &app,
        get(
            &wire("api.github.com/repos/jqlang/jq/releases?per_page=5"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.request().path(), "/proxy/gh/jqlang/jq/releases");
    assert_eq!(
        resp.request().query_string(),
        "per_page=5",
        "the query survives"
    );
    use actix_web::HttpMessage as _;
    assert_eq!(
        resp.request().extensions().get::<WiredFrom>(),
        Some(&WiredFrom("api.github.com".into()))
    );
}

/// §15 Q10's proof: two prefixes on one host, each to its own registry, and a
/// third prefix of that host is not wired.
#[actix_web::test]
async fn two_generic_prefixes_on_one_host_each_receive_their_own_paths() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    let a = call_service(
        &app,
        get(&wire("files.example.com/a/tool-1.0.tgz"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(a.request().path(), "/proxy/gen-a/generic/tool-1.0.tgz");
    let b = call_service(
        &app,
        get(&wire("files.example.com/b/tool-1.0.tgz"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(b.request().path(), "/proxy/gen-b/generic/tool-1.0.tgz");
    let c = call_service(
        &app,
        get(&wire("files.example.com/c/tool-1.0.tgz"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(c.status(), 404);
    assert!(body_of(c).await.contains("not_wired"));
}

#[actix_web::test]
async fn an_untyped_path_under_a_wired_prefix_bounces_but_never_on_a_forge_api() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    let resp = call_service(
        &app,
        get(
            &wire("github.com/jqlang/jq/blob/main/README.md"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 307);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://github.com/jqlang/jq/blob/main/README.md"
    );
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    // api.github.com is a forge API: an untyped path there is `not_wired`.
    let api = call_service(
        &app,
        get(&wire("api.github.com/users/octocat"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(api.status(), 404);
}

#[actix_web::test]
async fn unwired_prefix_is_never_redirected() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    for path in ["evil.example.com/x", "files.example.com/other/x"] {
        let resp = call_service(&app, get(&wire(path), Some(ADMIN_TOKEN))).await;
        assert_eq!(resp.status(), 404, "{path}");
        assert!(resp.headers().get("location").is_none(), "{path}");
    }
}

#[actix_web::test]
async fn wire_refuses_dot_segments_and_encodings() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    for path in [
        "github.com/../../api/v1/admin/users",
        "github.com/a/%2e%2e/b",
        "github.com/a/%2E%2e/b",
        "github.com/a%2fb",
        "github.com/a%5Cb",
        "github.com/a/%252e%252e/b",
        "github.com//x",
        "GitHub.com/x",
    ] {
        let resp = call_service(&app, get(&wire(path), Some(ADMIN_TOKEN))).await;
        assert_eq!(resp.status(), 400, "{path}");
    }
}

#[actix_web::test]
async fn own_host_wire_only_reaches_proxy_paths() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    let ok = call_service(
        &app,
        get(
            &wire("hub.example.com/proxy/gh/jqlang/jq/releases"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(ok.request().path(), "/proxy/gh/jqlang/jq/releases");
    let admin = call_service(
        &app,
        get(
            &wire("hub.example.com/api/v1/admin/users"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(admin.status(), 404);
}

#[actix_web::test]
async fn wire_answer_without_extension_is_404() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    for outcome in ["bounce", "not-wired", "malformed", "sink"] {
        let resp = call_service(
            &app,
            get(
                &format!("/proxy/{MV}/mise-versions/_wire-answer/{outcome}"),
                Some(ADMIN_TOKEN),
            ),
        )
        .await;
        assert_eq!(resp.status(), 404, "{outcome}");
        assert!(resp.headers().get("location").is_none());
    }
}

#[actix_web::test]
async fn enabled_false_is_not_rewritten() {
    let app = make_app(Lab {
        cfg: config(
            &base_registries("", "http://127.0.0.1:9").replace("enabled = true", "enabled = false"),
        ),
        closed: Vec::new(),
        air_gap: false,
    })
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("api.github.com/repos/jqlang/jq/releases"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 404);
    assert!(resp.request().path().contains("/_wire/"), "not rewritten");
}

#[actix_web::test]
async fn wire_respects_host_only() {
    let mut cfg = base_registries("", "http://127.0.0.1:9");
    cfg = cfg.replacen(
        "type = \"github\"",
        "type = \"github\"\n        path_routing = false\n        hosts = [\"gh.acme.io\"]",
        1,
    );
    let app = make_app(Lab {
        cfg: config(&cfg),
        closed: Vec::new(),
        air_gap: false,
    })
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("api.github.com/repos/jqlang/jq/releases"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 404);
    assert!(!resp.request().path().starts_with("/proxy/gh/"));
}

// ── T2 / T9 / Q15: the wire adds no access and maps no host ──────────────────

#[actix_web::test]
async fn anonymous_cannot_tell_wired_from_unwired() {
    // `mv` readable anonymously, `gh` closed: a rewrite into `gh` answers as an
    // unwired host would, byte for byte.
    let app = make_app(Lab {
        cfg: config(&base_registries("", "http://127.0.0.1:9")),
        closed: vec!["gh"],
        air_gap: false,
    })
    .await;
    let wired = call_service(
        &app,
        get(&wire("api.github.com/repos/acme/secret/releases"), None),
    )
    .await;
    let unwired = call_service(&app, get(&wire("api.github.com/users/acme"), None)).await;
    assert_eq!(wired.status(), 404);
    assert_eq!(unwired.status(), 404);
    assert_eq!(body_of(wired).await, body_of(unwired).await);

    // Air gap, `mv` closed: the sink needs read on `mv` too, so wired, unwired
    // and sink all answer the same refusal.
    let gapped = make_app(Lab {
        cfg: config(&gapped(base_registries("", "http://127.0.0.1:9"))),
        closed: vec!["gh", "mv"],
        air_gap: true,
    })
    .await;
    let a = call_service(
        &gapped,
        get(&wire("api.github.com/repos/acme/secret/releases"), None),
    )
    .await;
    let b = call_service(&gapped, get(&wire("nowhere.example.com/x"), None)).await;
    assert_eq!(a.status(), b.status());
    assert!(matches!(a.status().as_u16(), 401 | 403), "{}", a.status());
}

#[actix_web::test]
async fn a_wired_unmirrored_host_lands_in_the_sink_in_an_air_gap() {
    let app = make_app(Lab {
        cfg: config(&gapped(base_registries("", "http://127.0.0.1:9"))),
        closed: Vec::new(),
        air_gap: true,
    })
    .await;
    let resp = call_service(
        &app,
        get(&wire("nowhere.example.com/tool.tgz"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(resp.status(), 501);
    assert!(body_of(resp).await.contains("nowhere.example.com"));
}

// ── /api/v1/mise/auto-wire ───────────────────────────────────────────────────

#[actix_web::test]
async fn auto_wire_hosts_are_filtered_by_visibility() {
    let app = make_app(Lab {
        cfg: config(&base_registries("", "http://127.0.0.1:9")),
        closed: vec!["gh"],
        air_gap: false,
    })
    .await;
    let resp = call_service(&app, get("/api/v1/mise/auto-wire", None)).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = serde_json::from_str(&body_of(resp).await).unwrap();
    let hosts: Vec<&str> = v["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h.as_str())
        .collect();
    assert!(!hosts.contains(&"api.github.com"), "{hosts:?}");
    assert!(hosts.contains(&"files.example.com"), "{hosts:?}");
    assert_eq!(v["hidden_hosts"], 4, "the four github hosts");
    assert!(v["replacement"]
        .as_str()
        .unwrap()
        .ends_with("/proxy/mv/mise-versions/_wire/$1/$2"));
    assert_eq!(v["registry_floating"], false);
    // Every listed host routes: the rewrite agrees with what the API prints.
    for h in hosts {
        let resp = call_service(&app, get(&wire(&format!("{h}/a/x")), Some(ADMIN_TOKEN))).await;
        assert_ne!(
            resp.request().path(),
            wire(&format!("{h}/a/x")),
            "{h} was not rewritten"
        );
    }
}

// ── The overlay archive (Q17 and phase 3) ────────────────────────────────────

const JQ: &str = "backends = [\"aqua:jqlang/jq\"]\nversion_order = \"source\"\n";

fn upstream_archive(extra: &[(&str, &str)]) -> Vec<u8> {
    let mut m: std::collections::BTreeMap<String, String> =
        [("jq".to_owned(), JQ.to_owned())].into();
    for (n, s) in extra {
        m.insert((*n).to_owned(), (*s).to_owned());
    }
    overlay::pack(&m).expect("packs")
}

const OVERLAY: &str = r#"overlay = true
          retired = ["acme-old"]
          [[registries.auto_wire.tools]]
          name = "acme-deploy"
          aliases = ["deploy"]
          backends = ["github:acme/deploy"]"#;

#[actix_web::test]
async fn overlay_off_serves_the_public_archive_through_the_wire() {
    let mut up = mockito::Server::new_async().await;
    let raw = upstream_archive(&[]);
    up.mock(
        "GET",
        "/overlay_off_serves_the_public_archive_through_the_wire/registry/latest.tar.zst",
    )
    .with_body(raw.clone())
    .create_async()
    .await;
    let app = make_app(lab(
        "",
        &format!(
            "{}/overlay_off_serves_the_public_archive_through_the_wire",
            up.url()
        ),
    ))
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("mise.jdx.dev/registry/latest.tar.zst"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        read_body(resp).await.to_vec(),
        raw,
        "raw passthrough (§15 Q17)"
    );
}

#[actix_web::test]
async fn the_overlay_is_merged_and_its_names_never_reach_upstream() {
    let mut up = mockito::Server::new_async().await;
    up.mock(
        "GET",
        "/the_overlay_is_merged_and_its_names_never_reach_upstream/registry/latest.tar.zst",
    )
    .with_body(upstream_archive(&[]))
    .create_async()
    .await;
    let app = make_app(lab(
        OVERLAY,
        &format!(
            "{}/the_overlay_is_merged_and_its_names_never_reach_upstream",
            up.url()
        ),
    ))
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("mise.jdx.dev/registry/latest.tar.zst"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let merged = overlay::check(&read_body(resp).await).expect("mise would accept it");
    assert!(merged.entries.contains_key("jq"));
    assert!(merged.entries["acme-deploy"].contains("github:acme/deploy"));

    // T6: listings of overlay and retired names are answered here.
    for name in ["acme-deploy", "deploy", "acme-old"] {
        let resp = call_service(
            &app,
            get(
                &format!("/proxy/{MV}/mise-versions/data/{name}.toml"),
                Some(ADMIN_TOKEN),
            ),
        )
        .await;
        assert_eq!(resp.status(), 404, "{name}");
        assert!(body_of(resp).await.contains("overlay_tool"), "{name}");
    }
}

#[actix_web::test]
async fn overlay_cannot_replace_an_upstream_entry_without_force() {
    let mut up = mockito::Server::new_async().await;
    up.mock(
        "GET",
        "/overlay_cannot_replace_an_upstream_entry_without_force/registry/latest.tar.zst",
    )
    .with_body(upstream_archive(&[(
        "acme-deploy",
        "backends = [\"github:evil/deploy\"]\nversion_order = \"semver\"\n",
    )]))
    .create_async()
    .await;
    let app = make_app(lab(
        OVERLAY,
        &format!(
            "{}/overlay_cannot_replace_an_upstream_entry_without_force",
            up.url()
        ),
    ))
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("mise.jdx.dev/registry/latest.tar.zst"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    let status = resp.status();
    let body = read_body(resp).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let merged = overlay::check(&body).expect("valid");
    assert!(
        !merged.entries.contains_key("acme-deploy"),
        "both dropped by default"
    );
}

#[actix_web::test]
async fn a_failed_upstream_is_remembered_and_answers_at_once() {
    let mut up = mockito::Server::new_async().await;
    let m = up
        .mock(
            "GET",
            "/a_failed_upstream_is_remembered_and_answers_at_once/registry/latest.tar.zst",
        )
        .with_status(429)
        .expect(1)
        .create_async()
        .await;
    let app = make_app(lab(
        "",
        &format!(
            "{}/a_failed_upstream_is_remembered_and_answers_at_once",
            up.url()
        ),
    ))
    .await;
    for _ in 0..3 {
        let resp = call_service(
            &app,
            get(
                &wire("mise.jdx.dev/registry/latest.tar.zst"),
                Some(ADMIN_TOKEN),
            ),
        )
        .await;
        assert_eq!(resp.status(), 503);
    }
    m.assert_async().await;
}

#[actix_web::test]
async fn a_garbage_upstream_is_never_cached() {
    let mut up = mockito::Server::new_async().await;
    up.mock(
        "GET",
        "/a_garbage_upstream_is_never_cached/registry/latest.tar.zst",
    )
    .with_body("<html>captive portal</html>")
    .create_async()
    .await;
    let app = make_app(lab(
        "",
        &format!("{}/a_garbage_upstream_is_never_cached", up.url()),
    ))
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("mise.jdx.dev/registry/latest.tar.zst"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        503,
        "an invalid archive is a failure, not a 200"
    );
}

// ── §15 Q3: the bundle carries the raw archive ───────────────────────────────

fn archive(raw: bool) -> String {
    let q = if raw { "?raw=1" } else { "" };
    format!("/proxy/{MV}/mise-versions/_registry/latest.tar.zst{q}")
}

#[actix_web::test]
async fn raw_archive_is_unmerged_and_names_its_bundle_key() {
    let mut up = mockito::Server::new_async().await;
    let raw = upstream_archive(&[]);
    up.mock(
        "GET",
        "/raw_archive_is_unmerged_and_names_its_bundle_key/registry/latest.tar.zst",
    )
    .with_body(raw.clone())
    .create_async()
    .await;
    let app = make_app(lab(
        OVERLAY,
        &format!(
            "{}/raw_archive_is_unmerged_and_names_its_bundle_key",
            up.url()
        ),
    ))
    .await;
    let resp = call_service(&app, get(&archive(true), Some(ADMIN_TOKEN))).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("X-BatleHub-Storage-Key")
            .and_then(|v| v.to_str().ok()),
        Some("artifact:mv/_registry/raw/latest.tar.zst")
    );
    assert_eq!(read_body(resp).await.to_vec(), raw, "no overlay merged in");
    // Without `raw`, the same instance merges.
    let resp = call_service(&app, get(&archive(false), Some(ADMIN_TOKEN))).await;
    let merged = overlay::check(&read_body(resp).await).expect("valid");
    assert!(merged.entries.contains_key("acme-deploy"));
}

#[actix_web::test]
async fn raw_archive_needs_the_same_read_as_the_route() {
    let mut up = mockito::Server::new_async().await;
    let m = up
        .mock(
            "GET",
            "/raw_archive_needs_the_same_read_as_the_route/registry/latest.tar.zst",
        )
        .with_body(upstream_archive(&[]))
        .expect(0)
        .create_async()
        .await;
    let app = make_app(Lab {
        cfg: config(&base_registries(
            "",
            &format!("{}/raw_archive_needs_the_same_read_as_the_route", up.url()),
        )),
        closed: vec!["mv"],
        air_gap: false,
    })
    .await;
    for uri in [archive(true), archive(false)] {
        let resp = call_service(&app, get(&uri, None)).await;
        assert!(
            matches!(resp.status().as_u16(), 401 | 403),
            "{uri}: {}",
            resp.status()
        );
    }
    m.assert_async().await;
}

#[actix_web::test]
async fn raw_archive_refuses_a_garbage_upstream() {
    let mut up = mockito::Server::new_async().await;
    up.mock(
        "GET",
        "/raw_archive_refuses_a_garbage_upstream/registry/latest.tar.zst",
    )
    .with_body("<html>captive portal</html>")
    .create_async()
    .await;
    let app = make_app(lab(
        "",
        &format!("{}/raw_archive_refuses_a_garbage_upstream", up.url()),
    ))
    .await;
    let resp = call_service(&app, get(&archive(true), Some(ADMIN_TOKEN))).await;
    assert_eq!(
        resp.status(),
        503,
        "the export never carries what mise would refuse"
    );
}

/// An air-gapped app whose storage holds `held` where a bundle import files
/// it; the archive route's status, and on a `200` the merge is checked.
async fn gapped_with_held(held: Option<Vec<u8>>, upstream: &str) -> u16 {
    let storage = InMemoryStorage::new();
    if let Some(bytes) = held {
        storage
            .store(
                "artifact:mv/_registry/raw/latest.tar.zst",
                bytes.into(),
                Default::default(),
            )
            .await
            .expect("seeded");
    }
    let app = make_app_with(
        Lab {
            cfg: config(&gapped(base_registries(OVERLAY, upstream))),
            closed: Vec::new(),
            air_gap: true,
        },
        storage,
    )
    .await;
    let resp = call_service(&app, get(&archive(false), Some(ADMIN_TOKEN))).await;
    let status = resp.status().as_u16();
    if status == 200 {
        let merged = overlay::check(&read_body(resp).await).expect("mise would accept it");
        assert!(merged.entries.contains_key("jq"), "the held public entries");
        assert!(
            merged.entries["acme-deploy"].contains("github:acme/deploy"),
            "and this side's overlay"
        );
    }
    status
}

#[actix_web::test]
async fn an_air_gap_serves_the_held_archive_with_its_own_overlay() {
    let status = gapped_with_held(
        Some(upstream_archive(&[])),
        "http://127.0.0.1:9/an_air_gap_serves_the_held_archive",
    )
    .await;
    assert_eq!(status, 200);
}

#[actix_web::test]
async fn an_air_gap_refuses_a_corrupt_or_absent_held_archive() {
    let corrupt = gapped_with_held(
        Some(b"not a zstd archive".to_vec()),
        "http://127.0.0.1:9/an_air_gap_refuses_a_corrupt_held_archive",
    )
    .await;
    assert_eq!(corrupt, 503);
    let absent = gapped_with_held(
        None,
        "http://127.0.0.1:9/an_air_gap_refuses_an_absent_held_archive",
    )
    .await;
    assert_eq!(absent, 503);
}

// ── The eager overlay build (§4.3) ───────────────────────────────────────────

/// A tool that does not collide with [`upstream_archive`]'s entries.
const OVERLAY_LINT: &str = r#"overlay = true
          [[registries.auto_wire.tools]]
          name = "acme-lint"
          backends = ["github:acme/lint"]"#;

/// [`OVERLAY_LINT`] plus `acme-deploy`, which the test's upstream also has.
const OVERLAY_COLLIDING: &str = r#"overlay = true
          [[registries.auto_wire.tools]]
          name = "acme-lint"
          backends = ["github:acme/lint"]
          [[registries.auto_wire.tools]]
          name = "acme-deploy"
          backends = ["github:acme/deploy"]"#;

/// The full config file text, as [`config`] wraps it.
fn config_text(extra_mv: &str, upstream: &str) -> String {
    format!(
        r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"
        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"
        [server]
        host = "127.0.0.1"
        port = 8080
        {}
        "#,
        base_registries(extra_mv, upstream)
    )
}

fn no_access() -> batlehub_web::AccessConfig {
    batlehub_web::AccessConfig {
        anonymous: Default::default(),
        user: Default::default(),
        admin: Default::default(),
        groups: Default::default(),
        explore_anonymous: Default::default(),
        explore_user: Default::default(),
        explore_admin: Default::default(),
    }
}

/// What the server's hot builder does for the wire: the host map with the
/// config's wire, everything else empty.
fn wire_only_builder() -> batlehub_web::services::HotConfigBuilder {
    Arc::new(|cfg: &AppConfig| {
        let um = upstreams(cfg);
        let wire = MiseWire::build(cfg, &um)?;
        Ok(batlehub_web::services::BuiltHotState {
            hot: HotConfig::default(),
            access: no_access(),
            search_readmes: false,
            registry_map: batlehub_web::RegistryMap::new(HashMap::new()),
            registry_mode_map: RegistryModeMap::new(HashMap::new()),
            upstream_map: um,
            cargo_index_map: batlehub_web::CargoIndexMap::new(HashMap::new()),
            repo_signer_map: RepoSignerMap::default(),
            apk_signer_map: batlehub_web::ApkSignerMap::default(),
            vuln_db_map: batlehub_web::VulnDbMap::default(),
            sumdb_map: batlehub_web::SumDbMap::default(),
            registry_host_map: RegistryHostMap::default().with_wire(wire),
            text_config_owed: false,
        })
    })
}

fn warning_codes(wire: &MiseWire) -> Vec<(String, String)> {
    batlehub_web::handlers::proxy::mise_versions::mise_wire_warnings(wire, &|_| true)
        .into_iter()
        .map(|w| (w.code, w.path))
        .collect()
}

fn collision_rows(map: &RegistryHostMap) -> Vec<String> {
    warning_codes(&map.wire().expect("the wire is on"))
        .into_iter()
        .filter(|(code, _)| code == "mise.overlay.collision")
        .map(|(_, path)| path)
        .collect()
}

fn lab_of(registries: &str) -> Lab {
    Lab {
        cfg: config(registries),
        closed: Vec::new(),
        air_gap: false,
    }
}

#[actix_web::test]
async fn the_overlay_warnings_are_known_at_load_and_after_a_reload_without_a_client() {
    use std::io::Write as _;

    let mut up = mockito::Server::new_async().await;
    // One fetch for both builds: the reload's merge reads the cached archive.
    let fetch = up
        .mock(
            "GET",
            "/the_overlay_warnings_are_known_at_load/registry/latest.tar.zst",
        )
        .with_body(upstream_archive(&[(
            "acme-deploy",
            "backends = [\"github:evil/deploy\"]\nversion_order = \"semver\"\n",
        )]))
        .expect(1)
        .create_async()
        .await;
    let upstream = format!("{}/the_overlay_warnings_are_known_at_load", up.url());
    let start = config_text(OVERLAY_LINT, &upstream);
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(start.as_bytes()).expect("written");
    let regs = base_registries(OVERLAY_LINT, &upstream);
    let cfg = config(&regs);
    let (svc, _, _) = proxy_for(&lab_of(&regs), InMemoryStorage::new());
    let um = upstreams(&cfg);
    let map = RegistryHostMap::default().with_wire(MiseWire::build(&cfg, &um).expect("builds"));
    let reload = batlehub_web::services::ConfigReloadService::new(
        batlehub_web::services::ConfigReloadParams {
            hot: svc.hot.clone(),
            access: batlehub_web::new_access_lock(no_access()),
            search: batlehub_web::new_search_lock(false),
            registry_map: batlehub_web::RegistryMap::new(HashMap::new()),
            registry_mode_map: RegistryModeMap::new(HashMap::new()),
            upstream_map: um,
            cargo_index_map: batlehub_web::CargoIndexMap::new(HashMap::new()),
            repo_signer_map: RepoSignerMap::default(),
            apk_signer_map: batlehub_web::ApkSignerMap::default(),
            vuln_db_map: batlehub_web::VulnDbMap::default(),
            sumdb_map: batlehub_web::SumDbMap::default(),
            registry_host_map: map.clone(),
            proxy_trust: ProxyTrust::default(),
            discovery: Default::default(),
            config_path: file.path().to_str().expect("utf-8").to_owned(),
            config_overlays: Vec::new(),
            startup_layers: vec![start],
            config_change_repo: None,
            hot_reload_enabled: true,
            builder: wire_only_builder(),
            banner: None,
        },
    );

    // Load: the build runs with no archive request, and finds no collision.
    reload
        .build_mise_overlay_eagerly(Arc::clone(&svc))
        .expect("the overlay is on")
        .await
        .expect("the build task");
    let wire = map.wire().expect("on");
    assert!(
        wire.last_build
            .lock()
            .expect("lock")
            .upstream_invalid
            .is_none(),
        "the upstream archive was fetched and passed"
    );
    assert!(collision_rows(&map).is_empty());

    // A reload adding a colliding tool: the † row appears, still with no request.
    reload
        .load_pending_from_content(&config_text(OVERLAY_COLLIDING, &upstream), "test")
        .await
        .expect("staged");
    reload.apply("test").await.expect("applied");
    let mut rows = Vec::new();
    for _ in 0..200 {
        rows = collision_rows(&map);
        if !rows.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(rows, ["registries[3].auto_wire.tools[1].name"]);
    fetch.assert_async().await;
}

#[actix_web::test]
async fn an_eager_build_failure_is_the_upstream_invalid_warning() {
    let mut up = mockito::Server::new_async().await;
    up.mock("GET", "/an_eager_build_failure/registry/latest.tar.zst")
        .with_body("not an archive")
        .create_async()
        .await;
    let regs = base_registries(
        OVERLAY_LINT,
        &format!("{}/an_eager_build_failure", up.url()),
    );
    let cfg = config(&regs);
    let (svc, _, _) = proxy_for(&lab_of(&regs), InMemoryStorage::new());
    let wire = Arc::new(
        MiseWire::build(&cfg, &upstreams(&cfg))
            .expect("builds")
            .expect("on"),
    );
    batlehub_web::handlers::proxy::mise_versions::spawn_overlay_build(svc, Arc::clone(&wire))
        .expect("the overlay is on")
        .await
        .expect("the build task");
    let codes = warning_codes(&wire);
    assert!(
        codes
            .iter()
            .any(|(c, _)| c == "mise.overlay.upstream-invalid"),
        "{codes:?}"
    );
}

#[actix_web::test]
async fn no_eager_build_without_an_overlay() {
    let regs = base_registries("", "http://127.0.0.1:9/no_eager_build");
    let cfg = config(&regs);
    let (svc, _, _) = proxy_for(&lab_of(&regs), InMemoryStorage::new());
    let wire = MiseWire::build(&cfg, &upstreams(&cfg))
        .expect("builds")
        .expect("on");
    assert!(
        batlehub_web::handlers::proxy::mise_versions::spawn_overlay_build(svc, Arc::new(wire))
            .is_none()
    );
}

// ── T10 / T11: the span on every outcome, and the registry-host boundary ─────

use actix_web::HttpMessage as _;

#[actix_web::test]
async fn request_span_keeps_the_wired_host_for_every_wire_outcome() {
    let app = make_app(lab("", "http://127.0.0.1:9")).await;
    // (wired path, status, the host `WiredFrom` must carry)
    let cases = [
        // rewrite
        (
            "api.github.com/repos/jqlang/jq/releases",
            None,
            "api.github.com",
        ),
        // bounce
        (
            "github.com/jqlang/jq/blob/main/README.md",
            Some(307),
            "github.com",
        ),
        // not_wired
        ("nowhere.example.com/x", Some(404), "nowhere.example.com"),
        // malformed (an empty segment)
        ("github.com//x", Some(400), "github.com"),
    ];
    for (path, status, host) in cases {
        let resp = call_service(&app, get(&wire(path), Some(ADMIN_TOKEN))).await;
        if let Some(s) = status {
            assert_eq!(resp.status(), s, "{path}");
        }
        assert_eq!(
            resp.request().extensions().get::<WiredFrom>(),
            Some(&WiredFrom(host.into())),
            "{path}"
        );
    }
    // sink, in an air gap
    let gapped = make_app(Lab {
        cfg: config(&gapped(base_registries("", "http://127.0.0.1:9"))),
        closed: Vec::new(),
        air_gap: true,
    })
    .await;
    let resp = call_service(
        &gapped,
        get(&wire("nowhere.example.com/tool.tgz"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(resp.status(), 501);
    assert_eq!(
        resp.request().extensions().get::<WiredFrom>(),
        Some(&WiredFrom("nowhere.example.com".into()))
    );
}

/// `get` on a registry's own host rather than the main one.
fn get_on(host: &str, uri: &str) -> actix_http::Request {
    TestRequest::get()
        .uri(uri)
        .insert_header(("host", host))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request()
}

#[actix_web::test]
async fn wire_on_registry_host_is_refused() {
    // `gh` has its own host. A `_wire` path sent there is that host's path,
    // routed under `gh` like any other: never rewritten, never the archive
    // route, never the admin API, and the archive upstream is never dialled.
    let mut up = mockito::Server::new_async().await;
    let never = up
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let cfg = base_registries("", &format!("{}/wire_on_registry_host", up.url())).replacen(
        "type = \"github\"",
        "type = \"github\"\n        hosts = [\"gh.acme.io\"]",
        1,
    );
    let app = make_app(Lab {
        cfg: config(&cfg),
        closed: Vec::new(),
        air_gap: false,
    })
    .await;
    for path in [
        "mise.jdx.dev/registry/latest.tar.zst",
        "hub.example.com/api/v1/admin/users",
        "gh.acme.io/proxy/mv/mise-versions/_registry/latest.tar.zst",
        "api.github.com/repos/jqlang/jq/releases",
    ] {
        let resp = call_service(&app, get_on("gh.acme.io", &wire(path))).await;
        let routed = resp.request().path().to_owned();
        assert!(
            resp.request().extensions().get::<WiredFrom>().is_none(),
            "{path}: wired on a registry host"
        );
        assert!(
            routed.starts_with("/proxy/gh/proxy/mv/mise-versions/_wire/"),
            "{path}: routed as {routed}"
        );
        assert!(
            !matches!(resp.status().as_u16(), 200 | 307),
            "{path}: answered {}",
            resp.status()
        );
    }
    never.assert_async().await;
}

#[actix_web::test]
async fn a_wired_rewrite_into_a_host_only_registry_is_refused() {
    // `gh` is host-only, so the typed table never wires it; the own-host form
    // still reaches `/proxy/gh/…`, and the middleware's opt-out refuses it.
    let cfg = base_registries("", "http://127.0.0.1:9").replacen(
        "type = \"github\"",
        "type = \"github\"\n        path_routing = false\n        hosts = [\"gh.acme.io\"]",
        1,
    );
    let app = make_app(Lab {
        cfg: config(&cfg),
        closed: Vec::new(),
        air_gap: false,
    })
    .await;
    let resp = call_service(
        &app,
        get(
            &wire("hub.example.com/proxy/gh/jqlang/jq/releases"),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 404);
    assert!(body_of(resp).await.contains("unknown registry"));
}
