//! Alpine `apk`: the proxy half — the route level (RFC 0026 §4.3, §4.4).
//!
//! The adapter's own tests prove the wrapper reads an index and dates a
//! package; these prove the *route* turns a `.apk` path into a coordinate the
//! rule chain can act on, which is the whole claim that separates `apk` from
//! `deb`, `rpm` and `pacman`. Three kinds shipped with green unit tests and no
//! usable route, which is why the two are asserted separately.
//!
//! The local half — publish, the signed index, the served key, the
//! block-change hook — is in `local_apk_registry.rs`.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, TestRequest};

use batlehub_config::schema::RegistryMode;

const PKG_PATH: &str = "v3.22/main/x86_64/busybox-1.37.0-r20.apk";
const INDEX_PATH: &str = "v3.22/main/x86_64/APKINDEX.tar.gz";

async fn proxy_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts("alpine", "apk", RegistryMode::Proxy, None);
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

async fn get(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    path: &str,
) -> u16 {
    call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/alpine/apk/{path}"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await
    .status()
    .as_u16()
}

/// The ordinary case: a `.apk` and the index beside it both stream through.
#[actix_web::test]
async fn a_package_and_its_index_are_both_served() {
    let app = proxy_app().await;
    assert_eq!(get(&app, PKG_PATH).await, 200);
    assert_eq!(get(&app, INDEX_PATH).await, 200);
}

/// **The claim this kind exists for.** A blocked `busybox`/`1.37.0-r20` is
/// refused at the route, which means the file name really did become the
/// `PackageId` the block list matches on — and it is the `403` the download
/// gate emits, which apk 2.14 prints as "Permission denied" and apk 3 as
/// "HTTP 403: Forbidden".
#[actix_web::test]
async fn a_blocked_package_is_refused_with_403() {
    let app = proxy_app().await;
    assert_eq!(get(&app, PKG_PATH).await, 200, "served before the block");

    block_version(&app, "alpine", "busybox", "1.37.0-r20").await;

    assert_eq!(get(&app, PKG_PATH).await, 403, "refused after the block");
}

/// The block is on a coordinate, not on a path: a *different* version of the
/// same package keeps flowing. A path-addressed kind with one synthetic package
/// could not tell these two apart.
#[actix_web::test]
async fn blocking_one_version_leaves_the_others_alone() {
    let app = proxy_app().await;
    block_version(&app, "alpine", "busybox", "1.37.0-r20").await;

    assert_eq!(get(&app, PKG_PATH).await, 403);
    assert_eq!(
        get(&app, "v3.22/main/x86_64/busybox-1.37.0-r19.apk").await,
        200,
        "a different version of the same package is untouched"
    );
    assert_eq!(
        get(&app, "v3.22/main/x86_64/curl-8.14.1-r3.apk").await,
        200,
        "a different package is untouched"
    );
}

/// **Nothing blocks a listing.** The index is the synthetic `repo`/`_`
/// coordinate, so a block on a package it lists cannot accidentally take the
/// whole repository offline — which would break every `apk update` in the
/// fleet, not just the one install.
#[actix_web::test]
async fn blocking_a_package_does_not_refuse_the_index() {
    let app = proxy_app().await;
    block_version(&app, "alpine", "busybox", "1.37.0-r20").await;

    assert_eq!(
        get(&app, INDEX_PATH).await,
        200,
        "the index is still served — it is relayed byte-exact and has no coordinate"
    );
}

/// The same file under two branch URLs is two paths and one identity: blocking
/// the coordinate refuses both, which is the point of putting the identity in
/// the `PackageId` while the cache key stays the path.
#[actix_web::test]
async fn a_block_follows_the_identity_across_branches() {
    let app = proxy_app().await;
    block_version(&app, "alpine", "busybox", "1.37.0-r20").await;

    for path in [
        PKG_PATH,
        "latest-stable/main/x86_64/busybox-1.37.0-r20.apk",
        "edge/main/x86_64/busybox-1.37.0-r20.apk",
    ] {
        assert_eq!(get(&app, path).await, 403, "blocked through {path}");
    }
}

/// A `.apk` whose name carries no `-r<digits>` release token is a `400` at the
/// edge. Not a guess: inventing a version would let a block be bypassed by
/// misspelling a file name, and treating it as a listing would bypass the block
/// list entirely.
#[actix_web::test]
async fn a_malformed_package_name_is_400() {
    let app = proxy_app().await;
    for path in [
        "v3.22/main/x86_64/x-1.0.apk",
        "v3.22/main/x86_64/noversion.apk",
        "v3.22/main/x86_64/thing-1.0-rc1.apk",
    ] {
        assert_eq!(get(&app, path).await, 400, "expected 400 for {path}");
    }
}

/// The adversarial name the branch actually contains: a package whose own name
/// ends in `-r<digits>`. It must resolve, and it must resolve to the right
/// halves — a left-anchored split would call it `linux` at version
/// `firmware-r128`.
#[actix_web::test]
async fn a_name_ending_in_a_release_suffix_resolves_and_blocks() {
    let app = proxy_app().await;
    let path = "v3.22/main/x86_64/linux-firmware-r128-20250613-r0.apk";
    assert_eq!(get(&app, path).await, 200);

    block_version(&app, "alpine", "linux-firmware-r128", "20250613-r0").await;
    assert_eq!(
        get(&app, path).await,
        403,
        "the split named the right package"
    );
}

/// Traversal is refused before a storage key is built from the path.
#[actix_web::test]
async fn traversal_in_the_path_is_refused() {
    let app = proxy_app().await;
    let status = get(&app, "../../etc/passwd").await;
    assert!(
        status == 400 || status == 403,
        "traversal refused, got {status}"
    );
}
