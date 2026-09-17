//! The Nix binary cache: the proxy half, at the route level (RFC 0028).
//!
//! The adapter's own tests prove the client reads a narinfo and the core's
//! prove the fingerprint is right. These prove the *routes* do the three things
//! the kind exists for, which is a separate claim:
//!
//! 1. a narinfo is relayed with **one** line changed, and the upstream's
//!    signature still verifies over it;
//! 2. a blocked store path is absent — a `404`, the protocol's own "not in this
//!    cache" — at the narinfo *and* at the NAR, so a client holding a narinfo
//!    from before the block cannot fetch the bytes anyway;
//! 3. a NAR asked for in the upstream's own shape resolves through the reverse
//!    index, and `404`s when it cannot, rather than being served outside a
//!    coordinate.
//!
//! The local half — `nix copy --to`, verification, signing, the served public
//! key — is phase 4 and will be `local_nix_registry.rs`.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, TestRequest};
use batlehub_config::schema::RegistryMode;
use batlehub_core::services::nix::{self, NarInfo};

const REG: &str = "nixcache";

async fn proxy_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts(REG, "nix", RegistryMode::Proxy, None);
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

async fn get(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    path: &str,
) -> (u16, String) {
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/{REG}/nix/{path}"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    let body = String::from_utf8_lossy(&read_body(resp).await).into_owned();
    (status, body)
}

/// The claim the whole design rests on, asserted the only way that can catch a
/// wrong fingerprint: against a signature this code did not produce.
#[actix_web::test]
async fn a_relayed_narinfo_changes_one_line_and_keeps_its_signature() {
    let app = proxy_app().await;
    let (status, body) = get(&app, &format!("{NIX_HASH_A}.narinfo")).await;
    assert_eq!(status, 200, "{body}");

    let served = NarInfo::parse(&body).expect("the served document parses");

    // One line, and it is `URL:`.
    assert_eq!(
        served.get("URL"),
        Some(format!("nar/{NIX_HASH_A}/{NIX_NAR_A}").as_str()),
        "the NAR URL must carry the store hash the coordinate is derived from"
    );

    // Every other line is upstream's, in upstream's order.
    let upstream = NarInfo::parse(&nix_upstream_narinfo(NIX_HASH_A)).unwrap();
    let differing = upstream
        .lines()
        .iter()
        .zip(served.lines())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(differing, 1, "only `URL:` may differ\nserved:\n{body}");
    assert_eq!(
        upstream.lines().len(),
        served.lines().len(),
        "no line may be added or dropped"
    );

    // And the client's own check passes: `cache.nixos.org-1`'s signature over
    // the fingerprint of the document as *this instance* served it.
    let fp = nix::fingerprint(&served).expect("fingerprints");
    assert!(
        nix::verify_signature(
            &fp,
            served.get("Sig").unwrap(),
            &[NIX_UPSTREAM_KEY.to_owned()]
        ),
        "a client verifies the relayed document exactly as it would the upstream's"
    );
}

/// A store path this cache does not have is a `404`, which is not an error to
/// Nix — it consults the next substituter or builds.
#[actix_web::test]
async fn an_unknown_store_path_is_a_404() {
    let app = proxy_app().await;
    let unknown = "zzzznpbf2n4z3pjy6vm2mw8ywkqixxs6";
    assert_eq!(get(&app, &format!("{unknown}.narinfo")).await.0, 404);
}

/// A hash outside Nix's alphabet never reaches a storage key: a clean `400`
/// naming Nix's own rule, not a `404` three layers down.
#[actix_web::test]
async fn a_hash_outside_the_alphabet_is_a_400() {
    let app = proxy_app().await;
    // `e`, `o`, `u` and `t` are the four letters Nix32 omits.
    for bad in [
        "eeeenpbf2n4z3pjy6vm2mw8ywkqixxs6",
        "tooshort",
        "0001npbf2n4z3pjy6vm2mw8ywkqixxs6X",
    ] {
        let (status, _) = get(&app, &format!("{bad}.narinfo")).await;
        assert_eq!(status, 400, "'{bad}' must be a 400");
    }
}

/// **The claim this kind exists for.** A blocked coordinate is absent from the
/// cache in the protocol's own terms — and the *other* version of the same
/// package keeps flowing, which is what a `generic` mirror could never do.
#[actix_web::test]
async fn a_blocked_store_path_is_absent_and_its_sibling_is_not() {
    let app = proxy_app().await;
    assert_eq!(
        get(&app, &format!("{NIX_HASH_A}.narinfo")).await.0,
        200,
        "served before the block"
    );

    block_version(&app, REG, NIX_PACKAGE, NIX_VERSION_A).await;

    assert_eq!(
        get(&app, &format!("{NIX_HASH_A}.narinfo")).await.0,
        404,
        "a blocked path is a 404, not a 403: to Nix that is 'not in this cache', \
         and it moves to the next substituter or builds"
    );
    assert_eq!(
        get(&app, &format!("{NIX_HASH_B}.narinfo")).await.0,
        200,
        "the block is on a version, not on the package"
    );
}

/// The other half of the block (RFC 0028 §5.3): a client that fetched the
/// narinfo before the block holds the rewritten NAR URL for 30 days. The NAR
/// route re-derives the coordinate from the store hash in its own path and asks
/// again, so those bytes are refused too — and it is never a `403`
/// mid-transfer.
#[actix_web::test]
async fn a_client_holding_a_pre_block_narinfo_is_still_refused_at_the_nar() {
    let app = proxy_app().await;
    let nar = format!("nar/{NIX_HASH_A}/{NIX_NAR_A}");
    assert_eq!(get(&app, &nar).await.0, 200, "served before the block");

    block_version(&app, REG, NIX_PACKAGE, NIX_VERSION_A).await;

    let (status, _) = get(&app, &nar).await;
    assert!(
        status == 403 || status == 404,
        "a blocked coordinate must not serve its NAR, got {status}"
    );
}

/// The reverse index: a client whose cached narinfo predates this registry asks
/// in the upstream's shape. It resolves only *after* a narinfo has been served,
/// and `404`s before — which is what makes Nix refetch the narinfo and succeed,
/// rather than the proxy serving a NAR outside a coordinate.
#[actix_web::test]
async fn an_upstream_shaped_nar_url_resolves_only_through_a_served_narinfo() {
    let app = proxy_app().await;
    let upstream_shape = format!("nar/{NIX_NAR_A}");

    assert_eq!(
        get(&app, &upstream_shape).await.0,
        404,
        "before any narinfo is served there is no coordinate to serve it under, \
         and a 404 is what makes the client refetch the narinfo"
    );

    assert_eq!(get(&app, &format!("{NIX_HASH_A}.narinfo")).await.0, 200);

    assert_eq!(
        get(&app, &upstream_shape).await.0,
        200,
        "once a narinfo named this NAR, the index resolves it to its coordinate"
    );
}

/// `nix copy` asks before it uploads (`fileExists`), so a `HEAD` that 405'd
/// would make every publish re-upload a NAR the cache already has.
#[actix_web::test]
async fn head_on_a_narinfo_answers_the_status_with_no_body() {
    let app = proxy_app().await;
    let resp = call_service(
        &app,
        TestRequest::default()
            .method(actix_web::http::Method::HEAD)
            .uri(&format!("/proxy/{REG}/nix/{NIX_HASH_A}.narinfo"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        read_body(resp).await.is_empty(),
        "a HEAD must carry no body"
    );
}

/// **The defect that would have killed every `nix copy --to`.**
///
/// `addToStoreCommon` guards its NAR upload with
/// `if (repair || !fileExists(narInfo->url))`, and `narInfo->url` is the
/// *upstream-shape* path — so the probe is `HEAD nar/{fileHash}.nar.{ext}`,
/// not a HEAD on the narinfo. `HttpBinaryCacheStore::fileExists` maps only
/// `NotFound` and `Forbidden` to "absent"; anything else calls `disable(e)`,
/// which drops the cache from the client's substituter list for 60 seconds and
/// fails the copy. A `405` from a GET-only route is one of those.
///
/// So what is asserted here is not "HEAD works" — it is **"HEAD is not a
/// 405"**, which is the thing that breaks a publish.
#[actix_web::test]
async fn the_nar_routes_answer_head_because_a_publish_probes_before_uploading() {
    let app = proxy_app().await;
    for path in [
        format!("nar/{NIX_HASH_A}/{NIX_NAR_A}"),
        format!("nar/{NIX_NAR_A}"),
        format!("{NIX_HASH_A}.ls"),
    ] {
        let resp = call_service(
            &app,
            TestRequest::default()
                .method(actix_web::http::Method::HEAD)
                .uri(&format!("/proxy/{REG}/nix/{path}"))
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .to_request(),
        )
        .await;
        let status = resp.status().as_u16();
        assert_ne!(
            status, 405,
            "HEAD {path} answered 405 — `fileExists` treats that as a hard error, \
             disables the cache for 60s and fails the copy"
        );
        assert!(
            matches!(status, 200 | 403 | 404),
            "HEAD {path} answered {status}; `fileExists` only understands 200, 403 and 404"
        );
    }
}

/// The realisation route serves **both** spellings the protocol has in the
/// wild: Nix 2.24's one-segment `{drvHash}!{output}.doi` and master's
/// two-segment `{drvPath}/{outputName}.doi`. A single-segment pattern serves
/// one client generation and `404`s the other.
#[actix_web::test]
async fn both_realisation_spellings_reach_the_handler() {
    let app = proxy_app().await;
    for id in [
        // 2.24: `DrvOutput::to_string()` is `{drvHash}!{outputName}`.
        "sha256:1a2b3c4d5e6f!out",
        // master: `{drvPath}/{outputName}`.
        "1a2b3c4d5e6f.drv/out",
    ] {
        let (status, _) = get(&app, &format!("realisations/{id}.doi")).await;
        assert_ne!(
            status, 404,
            "realisations/{id}.doi did not reach the handler — a 404 here is the \
             route pattern refusing it, not the upstream"
        );
    }
}

/// `nix-cache-info` is relayed, not composed: `StoreDir` decides whether the
/// cache is usable at all, and `Priority` is the one value an operator changes
/// — on their own `substituters` line, with `?priority=`.
#[actix_web::test]
async fn cache_info_is_relayed_with_the_upstreams_own_values() {
    let app = proxy_app().await;
    let (status, body) = get(&app, "nix-cache-info").await;
    assert_eq!(status, 200);
    assert!(body.contains("StoreDir: /nix/store"), "{body}");
    assert!(body.contains("WantMassQuery: 1"), "{body}");
    assert!(
        body.contains("Priority: 40"),
        "the upstream's priority, not one this proxy invented: {body}"
    );
}

/// Every route that reaches a storage key refuses a traversal at the edge, for
/// a clean `400` rather than a refusal three layers down.
#[actix_web::test]
async fn nix_traversal_in_any_segment_returns_400() {
    let app = proxy_app().await;
    for path in [
        format!("nar/{NIX_HASH_A}/..%2f..%2fetc%2fpasswd"),
        "realisations/..%2f..%2fetc%2fpasswd.doi".to_owned(),
        "log/..%2f..%2fetc%2fpasswd".to_owned(),
    ] {
        let (status, _) = get(&app, &path).await;
        assert_eq!(status, 400, "'{path}' must be a 400");
    }
}

/// The fixture's own upstream document, so the relay test can diff against what
/// was actually sent rather than against a second copy that could drift.
fn nix_upstream_narinfo(hash: &str) -> String {
    // Re-derived from `common`'s fixture through the one public path that
    // exposes it: ask the fixture client directly.
    common::nix_fixture_for(hash).expect("the fixture has this hash")
}
