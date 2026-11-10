//! Blocking a release hides it from a Git forge's release listing.
//!
//! GitHub, Forgejo/Gitea and GitLab serve the same document shape — a JSON
//! array of releases, newest first, each naming its release by `tag_name` — so
//! one filter covers all three, and this file covers GitHub (which also serves
//! Forgejo) and GitLab.
//!
//! A "version" on a forge is a **tag**, and the same release is `1.2.3` in one
//! repository and `v1.2.3` in the next. A block must not depend on which habit
//! the operator copied, which is what the normalisation test here pins.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::call_service;
use serde_json::Value;

async fn app(name: &str, kind: &str) -> impl TestService {
    proxy_registry_app(name, kind).await
}

async fn releases<S: TestService>(app: &S, uri: &str) -> Vec<String> {
    let doc: Value = get_json(app, uri).await;
    doc.as_array()
        .expect("a release array")
        .iter()
        .map(|r| r["tag_name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

const GH_URI: &str = "/proxy/local-gh/acme/widget/releases";

#[actix_web::test]
async fn github_release_listing_hides_a_blocked_release() {
    let app = app("local-gh", "github").await;

    assert_eq!(
        releases(&app, GH_URI).await,
        ["v2.0.0-beta.1", "v1.1.0", "v1.0.0"]
    );

    block_version(&app, "local-gh", "acme/widget", "v1.1.0").await;

    assert_eq!(
        releases(&app, GH_URI).await,
        ["v2.0.0-beta.1", "v1.0.0"],
        "the blocked release is gone and the newest-first order survives"
    );
}

/// The same release is tagged `1.1.0` in one repository and `v1.1.0` in the
/// next; a block recorded without the prefix must still hide the prefixed tag.
#[actix_web::test]
async fn a_block_matches_a_tag_with_or_without_its_v_prefix() {
    let app = app("local-gh", "github").await;

    block_version(&app, "local-gh", "acme/widget", "1.1.0").await;

    assert!(!releases(&app, GH_URI).await.contains(&"v1.1.0".to_owned()));
}

/// Forgejo shares GitHub's route and its document shape.
#[actix_web::test]
async fn forgejo_release_listing_hides_a_blocked_release() {
    let app = app("local-fj", "forgejo").await;
    let uri = "/proxy/local-fj/acme/widget/releases";

    block_version(&app, "local-fj", "acme/widget", "v1.1.0").await;

    assert_eq!(releases(&app, uri).await, ["v2.0.0-beta.1", "v1.0.0"]);
}

#[actix_web::test]
async fn gitlab_release_listing_hides_a_blocked_release() {
    let app = app("local-gl", "gitlab").await;
    // `{project:.+}` is greedy, so a GitLab project path is written out in full
    // rather than percent-encoded as the API's own `id` parameter would be.
    let uri = "/proxy/local-gl/acme/widget/-/releases";

    block_version(&app, "local-gl", "acme/widget", "v1.1.0").await;

    assert_eq!(releases(&app, uri).await, ["v2.0.0-beta.1", "v1.0.0"]);
}

#[actix_web::test]
async fn another_repository_is_untouched() {
    let app = app("local-gh", "github").await;

    block_version(&app, "local-gh", "acme/widget", "v1.1.0").await;

    assert!(releases(&app, "/proxy/local-gh/acme/other/releases")
        .await
        .contains(&"v1.1.0".to_owned()));
}

/// Hiding governs resolution, not diagnosis.
#[actix_web::test]
async fn a_direct_request_for_a_blocked_release_is_still_denied() {
    let app = app("local-gh", "github").await;

    block_version(&app, "local-gh", "acme/widget", "v1.1.0").await;

    let req = admin_get("/proxy/local-gh/acme/widget/releases/tags/v1.1.0");
    assert_eq!(call_service(&app, req).await.status(), 403);
}

/// Guard (review of #311, RFC 0043): mise 2026.11 asks the GitHub API's
/// `releases/latest` before it reads the mise-versions list. A mise-versions
/// block or age gate governs `latest` only because this kind does not serve
/// that route. Adding it must come with the same block filter the listing
/// applies — otherwise every block on `latest` silently stops working.
#[actix_web::test]
async fn releases_latest_is_not_served_so_mise_falls_back_to_the_filtered_list() {
    let app = app("local-gh", "github").await;
    block_version(&app, "local-gh", "acme/widget", "v1.1.0").await;
    for uri in [
        "/proxy/local-gh/acme/widget/releases/latest",
        "/proxy/local-gh/repos/acme/widget/releases/latest",
    ] {
        let status = call_service(&app, admin_get(uri)).await.status();
        assert!(
            status.is_client_error() && status != 403,
            "{uri} answered {status}: a served `latest` bypasses mise-versions blocks"
        );
    }
}
