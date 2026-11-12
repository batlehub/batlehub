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
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
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
    let um = upstreams(&lab.cfg);
    let wire = MiseWire::build(&lab.cfg, &um).expect("the wire builds");
    let map = RegistryHostMap::new(HashMap::new(), HashMap::new(), HashMap::new()).with_wire(wire);
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
