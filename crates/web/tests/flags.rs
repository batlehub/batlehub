//! RFC 0002 (recast by §13): pushed flags and the exposure report.
//!
//! The registry here has **no** `[security]` profile, so the flag is judged
//! by `FlagsRule` on every request — the path a `hard_block` takes on an
//! estate that never opted into quarantine. The push endpoint, the
//! administrator's listing, the report and its export are all measured
//! against the same in-memory access log the proxy writes to.

mod common;
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, TestRequest};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use batlehub_adapters::in_memory::InMemoryAdvisoryRepository;
use batlehub_config::schema::{FlagSourceConfig, RegistryMode};
use batlehub_core::{
    ports::AdvisoryRepository,
    rules::{BlockListRule, FlagsRule, Rule},
    services::RegistryPolicy,
};

const REG: &str = "npm-flagged";
const SOURCE: &str = "soc";
const SECRET: &str = "a-key-from-the-vault";

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn push_req(source: &str, secret: &str, body: &str) -> actix_http::Request {
    TestRequest::post()
        .uri(&format!("/api/v1/flags/{source}"))
        .insert_header(("Content-Type", "application/json"))
        .insert_header(("X-Hub-Signature-256", sign(secret, body.as_bytes())))
        .set_payload(body.to_owned())
        .to_request()
}

fn revoke_req(source: &str, secret: &str, external_id: &str) -> actix_http::Request {
    // A DELETE has no body, so the signature covers the method and path. It has
    // to name the flag: signing a constant would make one captured header a
    // standing key to lift every flag the source ever pushed.
    let canonical = format!("DELETE\n/api/v1/flags/{source}/{external_id}");
    TestRequest::delete()
        .uri(&format!("/api/v1/flags/{source}/{external_id}"))
        .insert_header(("X-Hub-Signature-256", sign(secret, canonical.as_bytes())))
        .to_request()
}

/// A revoke signed for a *different* flag of the same source: the signature is
/// valid HMAC over the wrong message, so it must not lift this one.
fn revoke_req_signed_for(
    source: &str,
    secret: &str,
    external_id: &str,
    signed_for: &str,
) -> actix_http::Request {
    let canonical = format!("DELETE\n/api/v1/flags/{source}/{signed_for}");
    TestRequest::delete()
        .uri(&format!("/api/v1/flags/{source}/{external_id}"))
        .insert_header(("X-Hub-Signature-256", sign(secret, canonical.as_bytes())))
        .to_request()
}

fn user_get(uri: &str) -> actix_http::Request {
    TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request()
}

fn tarball(version: &str) -> String {
    format!("/proxy/{REG}/pkg/{version}/tarball")
}

/// A proxy registry with `FlagsRule` beside the block list, one source
/// capped at `max_effect`, and the flag store shared with the endpoints.
async fn lab(max_effect: &str) -> (impl TestService, Arc<dyn AdvisoryRepository>) {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let advisories: Arc<dyn AdvisoryRepository> = Arc::new(
        InMemoryAdvisoryRepository::with_events(Arc::clone(&parts.admin_svc.repo)),
    );
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
                    Box::new(BlockListRule::new(Arc::clone(&parts.admin_svc.repo)))
                        as Box<dyn Rule>,
                    Box::new(FlagsRule::new(Arc::clone(&advisories))),
                ],
            }),
        );
    }
    let defaults = ConfigureAppDefaults {
        advisory_repo: Some(Arc::clone(&advisories)),
        flag_sources: batlehub_web::FlagSources(vec![FlagSourceConfig {
            name: SOURCE.into(),
            secret: SECRET.into(),
            max_effect: max_effect.into(),
            registries: vec![],
            max_flags_per_minute: 600,
        }]),
        ..ConfigureAppDefaults::default()
    };
    let app = build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        defaults,
    )
    .await;
    (app, advisories)
}

fn one(external_id: &str, version: &str, effect: &str) -> String {
    serde_json::json!({ "flags": [{
        "external_id": external_id,
        "registry": REG,
        "package_name": "pkg",
        "version": version,
        "kind": "malware",
        "effect": effect,
        "summary": "credential stealer in postinstall",
    }]})
    .to_string()
}

#[actix_web::test]
async fn an_unknown_source_and_a_bad_signature_answer_the_same_404() {
    let (app, _) = lab("hard_block").await;
    let body = one("CASE-1", "1.0.0", "hard_block");
    let resp = call_service(&app, push_req("nobody", SECRET, &body)).await;
    assert_eq!(resp.status(), 404);
    let resp = call_service(&app, push_req(SOURCE, "wrong-key", &body)).await;
    assert_eq!(
        resp.status(),
        404,
        "a bad signature must not confirm the name"
    );
    // Nothing landed.
    let resp = call_service(&app, admin_get("/api/v1/admin/flags")).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(v["total"], 0);
}

#[actix_web::test]
async fn a_push_is_judged_per_item_and_capped_to_the_source_ceiling() {
    let (app, _) = lab("gate").await;
    let body = serde_json::json!({ "flags": [
        // Over the ceiling: stored at `gate`, and told so.
        { "external_id": "A", "registry": REG, "package_name": "pkg", "version": "1.0.0",
          "effect": "hard_block", "summary": "s" },
        // Every version.
        { "external_id": "B", "registry": REG, "package_name": "pkg", "version_range": "*",
          "effect": "inform", "summary": "s" },
        // A range: refused, ranges wait for the version-scheme RFC.
        { "external_id": "C", "registry": REG, "package_name": "pkg", "version_range": "^1.0",
          "effect": "warn", "summary": "s" },
        // A registry that does not exist.
        { "external_id": "D", "registry": "nope", "package_name": "pkg", "version": "1.0.0",
          "effect": "warn", "summary": "s" },
        // Both selectors.
        { "external_id": "E", "registry": REG, "package_name": "pkg", "version": "1.0.0",
          "version_range": "*", "effect": "warn", "summary": "s" },
    ]})
    .to_string();
    let resp = call_service(&app, push_req(SOURCE, SECRET, &body)).await;
    let status = resp.status();
    let text = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert_eq!(status, 200, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["accepted"], 2, "{v}");
    assert_eq!(v["rejected"], 3, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items[0]["status"], "accepted");
    assert_eq!(items[0]["effect"], "gate");
    assert_eq!(items[0]["effect_capped"], true);
    assert_eq!(items[0]["created"], true);
    assert_eq!(items[1]["status"], "accepted");
    assert_eq!(items[1]["effect_capped"], false);
    assert_eq!(items[2]["status"], "rejected");
    assert!(
        items[2]["error"]
            .as_str()
            .unwrap()
            .contains("version-scheme"),
        "{}",
        items[2]
    );
    assert_eq!(items[3]["status"], "rejected");
    assert!(items[3]["error"]
        .as_str()
        .unwrap()
        .contains("unknown registry"));
    assert_eq!(items[4]["status"], "rejected");

    // A re-push of A is an update, not a second row.
    let resp = call_service(&app, push_req(SOURCE, SECRET, &one("A", "1.0.0", "warn"))).await;
    let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(v["items"][0]["created"], false, "{v}");
    let v = get_json(&app, "/api/v1/admin/flags").await;
    assert_eq!(v["total"], 2, "{v}");
}

#[actix_web::test]
async fn a_hard_block_denies_the_download_and_the_report_names_who_pulled_before_it() {
    let (app, _) = lab("hard_block").await;

    // A developer pulls the version while nobody has flagged it.
    let resp = call_service(&app, user_get(&tarball("1.0.0"))).await;
    assert_eq!(resp.status(), 200);
    // And another version, which no flag will ever cover.
    let resp = call_service(&app, user_get(&tarball("1.1.0"))).await;
    assert_eq!(resp.status(), 200);

    // The SOC flags 1.0.0.
    let resp = call_service(
        &app,
        push_req(SOURCE, SECRET, &one("CASE-7", "1.0.0", "hard_block")),
    )
    .await;
    assert_eq!(resp.status(), 200);

    // From here the version is refused, and the refusal names the source.
    let resp = call_service(&app, user_get(&tarball("1.0.0"))).await;
    assert_eq!(resp.status(), 403);
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("soc") && body.contains("CASE-7"), "{body}");
    // 1.1.0 is untouched.
    let resp = call_service(&app, user_get(&tarball("1.1.0"))).await;
    assert_eq!(resp.status(), 200);

    // The report: one consumer, one coordinate, one flag, pulled once before
    // the flag was known.
    let v = get_json(&app, "/api/v1/admin/exposure").await;
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{v}");
    let row = &rows[0];
    assert_eq!(row["version"], "1.0.0");
    assert_eq!(row["source"], SOURCE);
    assert_eq!(row["external_id"], "CASE-7");
    assert_eq!(row["effect"], "hard_block");
    assert_eq!(row["pulls"], 1);
    assert_eq!(row["pulls_before_flag"], 1);
    assert!(row["consumer"].as_str().unwrap() != "anonymous");
    assert!(v["next"].is_null());
    // The coverage block says what it could see.
    assert_eq!(v["coverage"]["registries_total"], 1, "{v}");
    assert_eq!(v["coverage"]["flag_sources"][0]["source"], SOURCE);
    assert_eq!(v["coverage"]["flag_sources"][0]["live_flags"], 1);
    assert!(v["coverage"]["last_scan"].as_array().unwrap().is_empty());

    // The retroactive filter keeps it; the other one does not.
    let v = get_json(&app, "/api/v1/admin/exposure?when=before_flag").await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 1);
    let v = get_json(&app, "/api/v1/admin/exposure?when=after_flag").await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 0);

    // The export walks the same rows.
    let csv = get_text(&app, "/api/v1/admin/exposure/export?format=csv").await;
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 2, "{csv}");
    assert!(lines[0].starts_with("consumer,consumer_role,registry,package_name,version,source"));
    assert!(
        lines[1].contains("CASE-7") && lines[1].contains(",1,1,"),
        "{csv}"
    );

    // A revoke signature is bound to the flag it names. One captured from
    // another case of the same source must not lift this block — otherwise a
    // single observed header is a standing key to every flag the source pushed.
    let resp = call_service(
        &app,
        revoke_req_signed_for(SOURCE, SECRET, "CASE-7", "CASE-OTHER"),
    )
    .await;
    assert_eq!(
        resp.status(),
        404,
        "a signature issued for another flag must not revoke this one"
    );
    let resp = call_service(&app, user_get(&tarball("1.0.0"))).await;
    assert_eq!(resp.status(), 403, "the block must still stand");

    // A revoke lifts the block and keeps the tombstone for the report.
    let resp = call_service(&app, revoke_req(SOURCE, SECRET, "CASE-7")).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(v["revoked"], true);
    let resp = call_service(&app, user_get(&tarball("1.0.0"))).await;
    assert_eq!(resp.status(), 200);
    let v = get_json(&app, "/api/v1/admin/flags").await;
    assert_eq!(v["total"], 0, "a revoked flag is not live: {v}");
    let v = get_json(&app, "/api/v1/admin/flags?include_dead=true").await;
    assert_eq!(v["total"], 1);
    assert!(!v["items"][0]["revoked_at"].is_null());
    // A second revoke of the same id changes nothing.
    let resp = call_service(&app, revoke_req(SOURCE, SECRET, "CASE-7")).await;
    let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(v["revoked"], false);
}

#[actix_web::test]
async fn a_gate_flag_is_judged_by_severity_and_a_warn_flag_never_denies() {
    let (app, _) = lab("hard_block").await;
    // Below the default `high` threshold: served.
    let body = serde_json::json!({ "flags": [{
        "external_id": "G-low", "registry": REG, "package_name": "pkg", "version": "1.0.0",
        "effect": "gate", "severity": "medium", "summary": "s" }]})
    .to_string();
    call_service(&app, push_req(SOURCE, SECRET, &body)).await;
    let resp = call_service(&app, user_get(&tarball("1.0.0"))).await;
    assert_eq!(resp.status(), 200);
    // At it: refused.
    let body = serde_json::json!({ "flags": [{
        "external_id": "G-high", "registry": REG, "package_name": "pkg", "version": "1.0.0",
        "effect": "gate", "severity": "critical", "summary": "s" }]})
    .to_string();
    call_service(&app, push_req(SOURCE, SECRET, &body)).await;
    let resp = call_service(&app, user_get(&tarball("1.0.0"))).await;
    assert_eq!(resp.status(), 403);
    // A warn on every version of the package denies nothing.
    let resp = call_service(&app, user_get(&tarball("1.1.0"))).await;
    assert_eq!(resp.status(), 200);
    let body = serde_json::json!({ "flags": [{
        "external_id": "W", "registry": REG, "package_name": "pkg", "version_range": "*",
        "effect": "warn", "summary": "s" }]})
    .to_string();
    call_service(&app, push_req(SOURCE, SECRET, &body)).await;
    let resp = call_service(&app, user_get(&tarball("1.1.0"))).await;
    assert_eq!(resp.status(), 200);
    // But it is in the report, on both versions the developer pulled.
    let v = get_json(&app, "/api/v1/admin/exposure?source=soc&min_effect=warn").await;
    let versions: Vec<&str> = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["external_id"] == "W")
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions.len(), 2, "{v}");
}

#[actix_web::test]
async fn the_listing_needs_flags_read_and_the_report_audit_read() {
    let (app, _) = lab("gate").await;
    for uri in [
        "/api/v1/admin/flags",
        "/api/v1/admin/exposure",
        "/api/v1/admin/exposure/export",
    ] {
        let resp = call_service(&app, user_get(uri)).await;
        assert_eq!(resp.status(), 403, "{uri}");
        let resp = call_service(&app, admin_get(uri)).await;
        assert_eq!(resp.status(), 200, "{uri}");
    }
    let resp = call_service(&app, admin_get("/api/v1/admin/exposure?after=garbage")).await;
    assert_eq!(resp.status(), 400);
    let resp = call_service(&app, admin_get("/api/v1/admin/exposure?min_effect=loud")).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn the_report_pages_by_keyset() {
    let (app, _) = lab("gate").await;
    // Three pulls of three versions by the same developer, one flag on `*`.
    for v in ["1.0.0", "1.1.0", "2.0.0-beta.1"] {
        let resp = call_service(&app, user_get(&tarball(v))).await;
        assert_eq!(resp.status(), 200, "{v}");
    }
    let body = serde_json::json!({ "flags": [{
        "external_id": "ALL", "registry": REG, "package_name": "pkg", "version_range": "*",
        "effect": "inform", "summary": "s" }]})
    .to_string();
    call_service(&app, push_req(SOURCE, SECRET, &body)).await;
    let first = get_json(&app, "/api/v1/admin/exposure?limit=2").await;
    assert_eq!(first["rows"].as_array().unwrap().len(), 2, "{first}");
    let next = first["next"].as_str().expect("a cursor when more follow");
    let second = get_json(
        &app,
        &format!("/api/v1/admin/exposure?limit=2&after={next}"),
    )
    .await;
    assert_eq!(second["rows"].as_array().unwrap().len(), 1, "{second}");
    assert!(second["next"].is_null());
    let mut seen: Vec<String> = first["rows"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["rows"].as_array().unwrap())
        .map(|r| r["version"].as_str().unwrap().to_owned())
        .collect();
    seen.sort();
    assert_eq!(seen, ["1.0.0", "1.1.0", "2.0.0-beta.1"]);
}
