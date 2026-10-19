//! Helm chart repositories as a registry kind (RFC 0029).
//!
//! What `helm` does, and what this proxy therefore has to get right:
//!
//! - it resolves **every** `repo/chart` reference through its cached copy of
//!   `index.yaml`, so a blocked version must be absent from the served index or
//!   helm selects something the download then refuses;
//! - it reads `urls[0]` and nothing else, and sends the repository's basic
//!   auth only to the repository's own host — so every URL in the served index
//!   has to be on this instance;
//! - it fetches `{url}.prov` for `--verify`, byte-exact.
//!
//! The proxy fixture is `FixedRegistry`'s helm index: `demo` at `1.0.0`,
//! `1.1.0` and `2.0.0-beta.1` with absolute upstream URLs, and `ocichart`, a
//! pointer into an OCI registry. See `tests/common/mod.rs`.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, read_body, TestRequest};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    rules::{BlockListRule, DenyLatestRule, Rule},
    services::helm::{parse_index, versions_of, HybridMerge},
    services::RegistryPolicy,
};

const REG: &str = "charts";

fn url(path: &str) -> String {
    format!("/proxy/{REG}/helm/{path}")
}

async fn index_of<S: TestService>(app: &S) -> serde_json::Value {
    parse_index(&get_text(app, &url("index.yaml")).await).expect("the served index parses")
}

/// A packaged chart, laid out as `helm package` lays it out.
fn chart_tgz(name: &str, version: &str) -> Vec<u8> {
    chart_tgz_with(name, version, "")
}

/// [`chart_tgz`] with `extra` appended to its `Chart.yaml`.
fn chart_tgz_with(name: &str, version: &str, extra: &str) -> Vec<u8> {
    // The directory is fixed so a hostile `name` can still be packaged — the
    // `tar` crate refuses to *write* a `..` path, and the server reads the name
    // out of `Chart.yaml`, not out of the directory.
    let files = [
        (
            "chart/Chart.yaml".to_owned(),
            format!("apiVersion: v2\nname: {name}\nversion: {version}\nappVersion: \"1.10\"\ndescription: test chart\n{extra}"),
        ),
        ("chart/values.yaml".to_owned(), "a: 1\n".to_owned()),
        ("chart/README.md".to_owned(), format!("# {name}\n")),
    ];
    let mut tar = tar::Builder::new(Vec::new());
    for (path, contents) in &files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, contents.as_bytes())
            .unwrap();
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

fn publish_raw(body: Vec<u8>, query: &str, token: Option<&str>) -> actix_http::Request {
    let mut req = TestRequest::post().uri(&format!("{}{query}", url("api/charts")));
    if let Some(token) = token {
        req = req.insert_header(("Authorization", bearer(token)));
    }
    req.set_payload(body).to_request()
}

/// `helm cm-push`'s shape: multipart with `chart` and `prov`.
fn publish_multipart(chart: &[u8], prov: Option<&[u8]>) -> actix_http::Request {
    const B: &str = "----batlehubhelm";
    let mut body = Vec::new();
    let mut part = |field: &str, file: &str, bytes: &[u8]| {
        body.extend_from_slice(
            format!(
                "--{B}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    };
    part("chart", "c.tgz", chart);
    if let Some(prov) = prov {
        part("prov", "c.tgz.prov", prov);
    }
    body.extend_from_slice(format!("--{B}--\r\n").as_bytes());
    TestRequest::post()
        .uri(&url("api/charts"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("Content-Type", format!("multipart/form-data; boundary={B}")))
        .set_payload(body)
        .to_request()
}

// ── proxy mode: the index is the chokepoint ──────────────────────────────────

#[actix_web::test]
async fn every_url_in_the_served_index_is_on_this_instance() {
    let app = proxy_registry_app(REG, "helm").await;
    let resp = call_service(&app, admin_get(&url("index.yaml"))).await;
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/yaml"));
    let index = parse_index(std::str::from_utf8(&read_body(resp).await).unwrap()).unwrap();

    assert_eq!(
        versions_of(&index, "demo"),
        ["2.0.0-beta.1", "1.1.0", "1.0.0"]
    );
    for entry in index["entries"]["demo"].as_array().unwrap() {
        let urls = entry["urls"].as_array().unwrap();
        let v = entry["version"].as_str().unwrap();
        assert_eq!(
            urls.len(),
            1,
            "helm reads urls[0]; a second one is a way past the proxy"
        );
        assert_eq!(
            urls[0].as_str().unwrap(),
            format!("http://localhost:8080/proxy/{REG}/helm/charts/demo-{v}.tgz")
        );
    }
    // Every other field is relayed.
    let first = &index["entries"]["demo"][0];
    assert_eq!(first["appVersion"], "1.10");
    assert_eq!(first["annotations"]["artifacthub.io/license"], "MIT");
    assert_eq!(
        first["digest"],
        "0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[actix_web::test]
async fn an_oci_entry_is_dropped_by_default() {
    let app = proxy_registry_app(REG, "helm").await;
    let index = index_of(&app).await;
    assert!(
        index["entries"].get("ocichart").is_none(),
        "an entry the proxy cannot serve must not be advertised"
    );
}

#[actix_web::test]
async fn a_blocked_version_is_absent_from_the_index_and_refused_at_the_archive() {
    let app = proxy_registry_app(REG, "helm").await;
    block_version(&app, REG, "demo", "1.1.0").await;

    let index = index_of(&app).await;
    assert_eq!(versions_of(&index, "demo"), ["2.0.0-beta.1", "1.0.0"]);

    // A client holding an index from before the block still asks for the
    // archive; the download gate is what refuses it (RFC 0029 §5.3).
    let resp = call_service(&app, admin_get(&url("charts/demo-1.1.0.tgz"))).await;
    assert_eq!(resp.status(), 403);
    let resp = call_service(&app, admin_get(&url("charts/demo-1.0.0.tgz"))).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn the_archive_and_its_provenance_are_separate_coordinates() {
    let app = proxy_registry_app(REG, "helm").await;
    let chart = get_text(&app, &url("charts/demo-1.0.0.tgz")).await;
    let prov = get_text(&app, &url("charts/demo-1.0.0.tgz.prov")).await;
    assert_eq!(chart, format!("artifact:helm:{REG}/demo/1.0.0/chart"));
    assert_eq!(prov, format!("artifact:helm:{REG}/demo/1.0.0/prov"));
}

#[actix_web::test]
async fn a_chart_file_name_is_validated_at_the_edge() {
    let app = proxy_registry_app(REG, "helm").await;
    for bad in ["demo.tgz", "..%2Fdemo-1.0.0.tgz", "demo-1.0.0.zip"] {
        let resp = call_service(&app, admin_get(&url(&format!("charts/{bad}")))).await;
        assert_eq!(resp.status(), 400, "{bad}");
    }
}

#[actix_web::test]
async fn the_chartmuseum_api_is_not_served_in_proxy_mode() {
    let app = proxy_registry_app(REG, "helm").await;
    let resp = call_service(
        &app,
        publish_raw(chart_tgz("mine", "0.1.0"), "", Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let resp = call_service(&app, admin_get(&url("api/charts"))).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn the_routes_belong_to_a_helm_registry_only() {
    // On an npm registry, `…/helm/index.yaml` is an npm package called `helm`
    // at a version called `index.yaml`, and the helm handler must not claim it.
    let app = proxy_registry_app("npmreg", "npm").await;
    let resp = call_service(&app, admin_get("/proxy/npmreg/helm/index.yaml")).await;
    assert_ne!(
        resp.headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().starts_with("text/yaml")),
        Some(true)
    );
}

// ── local mode: ChartMuseum's API, the index composed from rows ──────────────

#[actix_web::test]
async fn a_published_chart_is_in_the_index_and_served_back_byte_exact() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    let tgz = chart_tgz("mine", "0.1.0");
    let resp = call_service(&app, publish_raw(tgz.clone(), "", Some(ADMIN_TOKEN))).await;
    assert_eq!(resp.status(), 201);
    let saved: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(saved, serde_json::json!({"saved": true}));

    let index = index_of(&app).await;
    let entry = &index["entries"]["mine"][0];
    assert_eq!(entry["version"], "0.1.0");
    assert_eq!(
        entry["appVersion"], "1.10",
        "Chart.yaml fields are read at publish"
    );
    assert_eq!(entry["description"], "test chart");
    assert_eq!(entry["digest"].as_str().unwrap(), sha256_hex(&tgz));
    assert!(entry["created"].as_str().is_some());
    assert_eq!(
        entry["urls"][0],
        format!("http://localhost:8080/proxy/{REG}/helm/charts/mine-0.1.0.tgz")
    );

    assert_eq!(get_bytes(&app, &url("charts/mine-0.1.0.tgz")).await, tgz);
    // Published without a `.prov`, so there is none.
    let resp = call_service(&app, admin_get(&url("charts/mine-0.1.0.tgz.prov"))).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn a_cm_push_with_provenance_serves_the_prov_beside_the_archive() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    let prov = b"-----BEGIN PGP SIGNED MESSAGE-----\nfake\n";
    let resp = call_service(
        &app,
        publish_multipart(&chart_tgz("signed", "1.0.0"), Some(prov)),
    )
    .await;
    assert_eq!(resp.status(), 201);
    assert_eq!(
        get_bytes(&app, &url("charts/signed-1.0.0.tgz.prov")).await,
        prov
    );
}

#[actix_web::test]
async fn publishing_twice_is_a_conflict_even_with_force() {
    // `?force` on a new version is an ordinary publish.
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    let tgz = chart_tgz("mine", "0.1.0");
    assert_eq!(
        call_service(&app, publish_raw(tgz.clone(), "", Some(ADMIN_TOKEN)))
            .await
            .status(),
        201
    );
    assert_eq!(
        call_service(&app, publish_raw(tgz.clone(), "", Some(ADMIN_TOKEN)))
            .await
            .status(),
        409
    );
    // `?force` meets the same refusal: the row store replaces no published
    // version (RFC 0016), so it asks for no verb either.
    assert_eq!(
        call_service(&app, publish_raw(tgz, "?force", Some(ADMIN_TOKEN)))
            .await
            .status(),
        409
    );
    assert_eq!(
        call_service(
            &app,
            publish_raw(chart_tgz("mine", "0.2.0"), "?force", Some(ADMIN_TOKEN))
        )
        .await
        .status(),
        201
    );
}

#[actix_web::test]
async fn an_anonymous_publish_is_refused() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    let resp = call_service(&app, publish_raw(chart_tgz("mine", "0.1.0"), "", None)).await;
    assert!(
        resp.status() == 401 || resp.status() == 403,
        "{}",
        resp.status()
    );
}

#[actix_web::test]
async fn helm_publish_traversal_version_returns_400() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    for (name, version) in [
        ("mine", "../../etc/x"),
        ("../evil", "1.0.0"),
        ("mine", "1.x"),
    ] {
        let resp = call_service(
            &app,
            publish_raw(chart_tgz(name, version), "", Some(ADMIN_TOKEN)),
        )
        .await;
        assert_eq!(resp.status(), 400, "{name} {version}");
    }
    let resp = call_service(
        &app,
        publish_raw(b"not a chart".to_vec(), "", Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn delete_removes_the_version_from_the_next_index() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    for v in ["0.1.0", "0.2.0"] {
        assert_eq!(
            call_service(
                &app,
                publish_raw(chart_tgz("mine", v), "", Some(ADMIN_TOKEN))
            )
            .await
            .status(),
            201
        );
    }
    assert_eq!(
        versions_of(&index_of(&app).await, "mine"),
        ["0.2.0", "0.1.0"]
    );

    let req = TestRequest::delete()
        .uri(&url("api/charts/mine/0.1.0"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
    assert_eq!(versions_of(&index_of(&app).await, "mine"), ["0.2.0"]);

    let req = TestRequest::delete()
        .uri(&url("api/charts/mine/9.9.9"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);

    // The coordinate is validated at the edge, not left to the storage guard.
    for bad in ["mine/..", "..%2Fetc/0.2.0", "mine/..%2F..%2Fx", "mine/1.x"] {
        let req = TestRequest::delete()
            .uri(&url(&format!("api/charts/{bad}")))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 400, "{bad}");
    }
    assert_eq!(versions_of(&index_of(&app).await, "mine"), ["0.2.0"]);
}

#[actix_web::test]
async fn a_blocked_local_version_is_absent_from_the_index() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    for v in ["0.1.0", "0.2.0"] {
        let resp = call_service(
            &app,
            publish_raw(chart_tgz("mine", v), "", Some(ADMIN_TOKEN)),
        )
        .await;
        assert_eq!(resp.status(), 201, "{v}");
    }
    block_version(&app, REG, "mine", "0.2.0").await;
    assert_eq!(versions_of(&index_of(&app).await, "mine"), ["0.1.0"]);
}

#[actix_web::test]
async fn the_chartmuseum_listings_describe_what_is_published() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    for (n, v) in [("alpha", "1.0.0"), ("alpha", "1.1.0"), ("beta", "0.1.0")] {
        call_service(&app, publish_raw(chart_tgz(n, v), "", Some(ADMIN_TOKEN))).await;
    }
    let all = get_json(&app, &url("api/charts")).await;
    assert_eq!(
        all.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(all["alpha"][0]["urls"][0], "charts/alpha-1.1.0.tgz");

    let paged = get_json(&app, &url("api/charts?offset=1&limit=1")).await;
    assert_eq!(
        paged.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["beta"]
    );

    let alpha = get_json(&app, &url("api/charts/alpha")).await;
    assert_eq!(alpha.as_array().unwrap().len(), 2);
    let one = get_json(&app, &url("api/charts/alpha/1.0.0")).await;
    assert_eq!(one["version"], "1.0.0");

    for missing in ["api/charts/nope", "api/charts/alpha/9.9.9"] {
        assert_eq!(
            call_service(&app, admin_get(&url(missing))).await.status(),
            404,
            "{missing}"
        );
    }
}

// ── hybrid: local rows over the upstream index ───────────────────────────────

/// A hybrid helm registry with `hybrid_merge` as given, `None` leaving it unset.
async fn hybrid_app(merge: Option<HybridMerge>) -> impl TestService {
    let parts = local_registry_app_parts(REG, "helm", RegistryMode::Hybrid, None);
    if let Some(merge) = merge {
        parts
            .proxy_svc
            .hot
            .write()
            .await
            .helm_hybrid_merge
            .insert(REG.to_owned(), merge);
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

#[actix_web::test]
async fn hybrid_merge_version_lists_upstream_versions_under_local_ones() {
    let app = hybrid_app(Some(HybridMerge::Version)).await;
    for (n, v) in [("demo", "1.1.0"), ("mine", "0.1.0")] {
        assert_eq!(
            call_service(&app, publish_raw(chart_tgz(n, v), "", Some(ADMIN_TOKEN)))
                .await
                .status(),
            201
        );
    }
    let index = index_of(&app).await;
    assert_eq!(versions_of(&index, "mine"), ["0.1.0"]);
    let demo = versions_of(&index, "demo");
    assert_eq!(
        demo.iter().filter(|v| *v == "1.1.0").count(),
        1,
        "one entry per version: {demo:?}"
    );
    let local = batlehub_core::services::helm::find_entry(&index, "demo", "1.1.0").unwrap();
    assert_eq!(
        local["description"], "test chart",
        "local wins on a collision"
    );
    // An upstream-only version still falls through to the upstream archive.
    assert_eq!(
        get_text(&app, &url("charts/demo-1.0.0.tgz")).await,
        format!("artifact:helm:{REG}/demo/1.0.0/chart")
    );
}

#[actix_web::test]
async fn a_chart_published_here_hides_the_upstream_chart_of_its_name() {
    // The default: a public `demo 1.0.0` must not be installable in place of
    // the internal `demo` — not listed, and not fetched on a direct request.
    for merge in [None, Some(HybridMerge::Chart)] {
        let app = hybrid_app(merge).await;
        let resp = call_service(
            &app,
            publish_raw(chart_tgz("demo", "1.1.0"), "", Some(ADMIN_TOKEN)),
        )
        .await;
        assert_eq!(resp.status(), 201);
        let index = index_of(&app).await;
        assert_eq!(versions_of(&index, "demo"), ["1.1.0"], "{merge:?}");
        let resp = call_service(&app, admin_get(&url("charts/demo-1.0.0.tgz"))).await;
        assert_eq!(resp.status(), 404, "{merge:?}");
        let resp = call_service(&app, admin_get(&url("charts/demo-1.0.0.tgz.prov"))).await;
        assert_eq!(resp.status(), 404, "{merge:?}");
    }
}

#[actix_web::test]
async fn an_upstream_chart_with_no_local_namesake_still_falls_through() {
    let app = hybrid_app(None).await;
    let resp = call_service(
        &app,
        publish_raw(chart_tgz("mine", "0.1.0"), "", Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(resp.status(), 201);
    let index = index_of(&app).await;
    assert_eq!(versions_of(&index, "mine"), ["0.1.0"]);
    assert!(versions_of(&index, "demo").contains(&"1.0.0".to_owned()));
    assert_eq!(
        get_text(&app, &url("charts/demo-1.0.0.tgz")).await,
        format!("artifact:helm:{REG}/demo/1.0.0/chart")
    );
}

// ── the review fixes ─────────────────────────────────────────────────────────

/// A helm registry whose policy carries `deny_latest` beside the block list.
async fn deny_latest_app(mode: RegistryMode) -> impl TestService {
    let parts = local_registry_app_parts(REG, "helm", mode, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![
                    Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo))),
                    Box::new(DenyLatestRule::new(vec![])) as Box<dyn Rule>,
                ],
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

#[actix_web::test]
async fn a_version_rule_does_not_refuse_the_whole_local_index() {
    // The index is a listing: only the grants gate it. Run as a rule against
    // a pseudo-coordinate `index.yaml@latest`, `deny_latest` refused it to
    // everyone and broke `helm repo update` for the whole repository.
    let app = deny_latest_app(RegistryMode::Local).await;
    assert_eq!(
        call_service(
            &app,
            publish_raw(chart_tgz("mine", "0.1.0"), "", Some(ADMIN_TOKEN))
        )
        .await
        .status(),
        201
    );
    assert_eq!(versions_of(&index_of(&app).await, "mine"), ["0.1.0"]);
    assert_eq!(
        get_json(&app, &url("api/charts/mine")).await[0]["version"],
        "0.1.0"
    );
}

#[actix_web::test]
async fn a_chart_yaml_field_helm_would_refuse_in_an_index_is_left_out() {
    // helm reads index.yaml strictly: `github` under a maintainer would fail
    // the document for every client of the repository.
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    let tgz = chart_tgz_with(
        "mine",
        "0.1.0",
        "maintainers:\n- name: a\n  github: b\nannotations:\n  n: 3\n  k: v\n",
    );
    assert_eq!(
        call_service(&app, publish_raw(tgz, "", Some(ADMIN_TOKEN)))
            .await
            .status(),
        201
    );
    let entry = &index_of(&app).await["entries"]["mine"][0];
    assert_eq!(entry["maintainers"], serde_json::json!([{"name": "a"}]));
    assert_eq!(entry["annotations"], serde_json::json!({"k": "v"}));
}

#[actix_web::test]
async fn a_provenance_file_is_stored_as_a_pgp_signature() {
    // `signing.allowed_types = ["pgp"]` is how an operator admits OpenPGP; a
    // `.prov` must be that, not a type no configuration can name.
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    let prov = b"-----BEGIN PGP SIGNED MESSAGE-----\nfake\n";
    call_service(
        &app,
        publish_multipart(&chart_tgz("signed", "1.0.0"), Some(prov)),
    )
    .await;
    assert_eq!(batlehub_core::services::helm::PROV_SIGNATURE_TYPE, "pgp");
    assert_eq!(
        get_bytes(&app, &url("charts/signed-1.0.0.tgz.prov")).await,
        prov
    );
}

#[actix_web::test]
async fn the_index_tells_cm_push_where_to_post() {
    // `helm cm-push` posts to `{serverInfo.contextPath}/api/charts`.
    for mode in [RegistryMode::Local, RegistryMode::Proxy] {
        let index = index_of(&registry_app(REG, "helm", mode).await).await;
        assert_eq!(
            index["serverInfo"]["contextPath"],
            format!("/proxy/{REG}/helm")
        );
    }
}

#[actix_web::test]
async fn a_block_holds_whatever_spelling_of_the_version_it_names() {
    // helm reads `v1.1.0` and `1.1.0` as one version.
    let app = proxy_registry_app(REG, "helm").await;
    block_version(&app, REG, "demo", "v1.1.0").await;
    assert_eq!(
        versions_of(&index_of(&app).await, "demo"),
        ["2.0.0-beta.1", "1.0.0"]
    );
    let resp = call_service(&app, admin_get(&url("charts/demo-1.1.0.tgz"))).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn every_version_helm_accepts_is_served_and_an_ambiguous_file_finds_its_chart() {
    let app = registry_app(REG, "helm", RegistryMode::Local).await;
    // `2024.01.15` is a chart version to helm and not SemVer; `app-1.0.0` at
    // `2.0.0` is a file name that also reads as `app` at `1.0.0-2.0.0`.
    for (n, v) in [("cal", "2024.01.15"), ("app-1.0.0", "2.0.0")] {
        let tgz = chart_tgz(n, v);
        assert_eq!(
            call_service(&app, publish_raw(tgz.clone(), "", Some(ADMIN_TOKEN)))
                .await
                .status(),
            201,
            "{n} {v}"
        );
        assert_eq!(
            get_bytes(&app, &url(&format!("charts/{n}-{v}.tgz"))).await,
            tgz,
            "{n} {v}"
        );
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
