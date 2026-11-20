//! RFC 0017 — the package- and version-tier grants editor.
//!
//! Three routes, the verb gate on each, §4.4's validation table across HTTP, and
//! the property the whole RFC turns on: **the writer and the filter ship
//! together**. The last of those is asserted end to end here rather than only in
//! `crates/core` — a version index that lists a version the caller may not read
//! is a disclosure of names and numbers, and it is a disclosure through the
//! protocol document rather than through the service.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::{json, Value};

use batlehub_config::schema::RegistryMode;

/// A local npm registry whose RBAC grants anonymous nothing, `user` the read,
/// and `admin` everything — so the admin holds `grants:read`/`grants:write`
/// through §10 rule 5's instance node and the user holds neither.
async fn app_with_grants() -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<batlehub_adapters::in_memory::InMemoryGrantRepository>,
) {
    let parts = local_only_app_parts_with_policy(
        "reg",
        "npm",
        RegistryMode::Local,
        false,
        rbac_policy_deny_anonymous,
    )
    .await;
    let grant_repo = batlehub_adapters::in_memory::InMemoryGrantRepository::new();
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.grant_repo =
            Some(Arc::clone(&grant_repo) as Arc<dyn batlehub_core::ports::GrantRepository>);
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, grant_repo)
}

fn put_body(package: &str, subject: &str, actions: &[&str]) -> Value {
    json!({ "package": package, "subject": subject, "actions": actions })
}

// ── The verb gate (§7) ────────────────────────────────────────────────────────

#[actix_web::test]
async fn writing_a_grant_requires_grants_write() {
    let (app, _) = app_with_grants().await;

    for (token, expected) in [
        (None, 403),
        (Some(USER_TOKEN), 403),
        (Some(ADMIN_TOKEN), 200),
    ] {
        let mut req = TestRequest::put()
            .uri("/api/v1/admin/registries/reg/grants")
            .set_json(put_body("pkg", "group:oidc1:eng", &["releases:read"]));
        if let Some(t) = token {
            req = req.insert_header(("Authorization", bearer(t)));
        }
        let resp = call_service(&app, req.to_request()).await;
        assert_eq!(
            resp.status(),
            expected,
            "token {token:?} on PUT /grants — `grants:write` is an admin verb \
             under §10 rule 5 and a `role:user` holds none of the control set"
        );
    }
}

#[actix_web::test]
async fn reading_grants_requires_grants_read() {
    let (app, _) = app_with_grants().await;

    for (token, expected) in [
        (None, 403),
        (Some(USER_TOKEN), 403),
        (Some(ADMIN_TOKEN), 200),
    ] {
        let mut req = TestRequest::get().uri("/api/v1/admin/registries/reg/grants?package=pkg");
        if let Some(t) = token {
            req = req.insert_header(("Authorization", bearer(t)));
        }
        let resp = call_service(&app, req.to_request()).await;
        assert_eq!(resp.status(), expected, "token {token:?} on GET /grants");
    }
}

#[actix_web::test]
async fn removing_a_grant_requires_grants_write() {
    let (app, _) = app_with_grants().await;
    let resp = call_service(
        &app,
        TestRequest::delete()
            .uri("/api/v1/admin/registries/reg/grants")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_json(json!({"package": "pkg", "subject": "user:alice"}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

// ── The round trip ────────────────────────────────────────────────────────────

#[actix_web::test]
async fn a_grant_is_written_listed_and_removed() {
    let (app, _) = app_with_grants().await;

    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/api/v1/admin/registries/reg/grants")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(put_body("pkg", "group:oidc1:eng", &["releases:read"]))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["actions"], json!(["releases:read"]));

    let listed: Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri("/api/v1/admin/registries/reg/grants?package=pkg")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    assert_eq!(listed["grants"][0]["subject"], "group:oidc1:eng");
    assert_eq!(listed["grants"][0]["node_kind"], "package");
    assert_eq!(listed["grants"][0]["node_key"], "pkg");
    assert_eq!(listed["grants"][0]["from_ownership"], false);

    let removed: Value = read_body_json(
        call_service(
            &app,
            TestRequest::delete()
                .uri("/api/v1/admin/registries/reg/grants")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .set_json(json!({"package": "pkg", "subject": "group:oidc1:eng"}))
                .to_request(),
        )
        .await,
    )
    .await;
    assert_eq!(removed["removed"], true);

    let after: Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri("/api/v1/admin/registries/reg/grants?package=pkg")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    assert_eq!(after["grants"].as_array().map(Vec::len), Some(0));
}

/// §4.2 — expansion happens at write, and the response reports what was stored
/// rather than what was asked for, because they differ.
#[actix_web::test]
async fn a_wildcard_is_reported_expanded() {
    let (app, _) = app_with_grants().await;
    let body: Value = read_body_json(
        call_service(
            &app,
            TestRequest::put()
                .uri("/api/v1/admin/registries/reg/grants")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .set_json(put_body("pkg", "user:alice", &["releases:*"]))
                .to_request(),
        )
        .await,
    )
    .await;
    let actions = body["actions"].as_array().expect("actions");
    assert!(
        actions.len() > 1,
        "`releases:*` names one thing and stores several"
    );
    assert!(actions.iter().any(|a| a == "releases:read"));
    assert!(
        !actions.iter().any(|a| a == "gates:exempt"),
        "`releases:*` must not reach `gates:exempt`"
    );
}

// ── §4.4 validation, across HTTP ──────────────────────────────────────────────

#[actix_web::test]
async fn an_invalid_grant_is_refused_with_400() {
    let (app, _) = app_with_grants().await;

    let cases: &[(&str, Value)] = &[
        (
            "unknown action",
            put_body("pkg", "user:alice", &["releases:teleport"]),
        ),
        (
            "unparseable subject",
            put_body("pkg", "!!!", &["releases:read"]),
        ),
        ("empty action set", put_body("pkg", "user:alice", &[])),
    ];
    for (why, body) in cases {
        let resp = call_service(
            &app,
            TestRequest::put()
                .uri("/api/v1/admin/registries/reg/grants")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .set_json(body)
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 400, "{why} should be a 400");
    }
}

/// A version-tier grant on a coordinate that does not exist is a typo more often
/// than a plan, and the row would resolve for nobody.
#[actix_web::test]
async fn a_version_grant_on_a_missing_version_is_refused() {
    let (app, _) = app_with_grants().await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/api/v1/admin/registries/reg/grants")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({
                "package": "pkg", "version": "9.9.9",
                "subject": "user:alice", "actions": ["releases:read"],
            }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

/// A seal is a config-file statement (§4.3). An empty action set removes, it
/// does not write a row that admits nobody.
#[actix_web::test]
async fn the_editor_cannot_write_a_seal() {
    let (app, repo) = app_with_grants().await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/api/v1/admin/registries/reg/grants")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(put_body("pkg", "*", &[]))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
    assert!(
        repo.grants_on_node("reg", batlehub_core::ports::NodeKind::Package, "pkg")
            .await
            .unwrap()
            .is_empty(),
        "a refused write must leave no row"
    );
}

// ── The unconfigured deployment ───────────────────────────────────────────────

#[actix_web::test]
async fn without_grant_storage_the_editor_answers_503() {
    // The shared factory leaves `hot.grant_repo` unset.
    let parts = local_only_app_parts_with_policy(
        "reg",
        "npm",
        RegistryMode::Local,
        false,
        rbac_policy_deny_anonymous,
    )
    .await;
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/registries/reg/grants?package=pkg")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status(),
        503,
        "a deployment with no grant storage has no editor, which is a deployment \
         fact rather than a bad request"
    );
}

use batlehub_core::ports::GrantRepository as _;

// ── The half that makes the writer safe (§2.3, phase 2) ───────────────────────
//
// A version index that lists a version the caller may not read discloses its
// existence, its number and the shape of the release train. That is why the
// filter is not a follow-up: the release between the writer and the filter is a
// release that under-filters, silently.
//
// # Why these run against rubygems `/info/{gem}` and not the npm packument
//
// §4.4's configuration is a caller holding `releases:list` **without**
// `releases:read`. Most version-document routes gate on `releases:read` at the
// handler (`serve_local_or_proxy_document(..., Action::ReleasesRead, ...)`), so
// such a caller is refused before the funnel is reached and the filter can never
// bite there. The rubygems compact index in `Local` mode authorizes inside the
// funnel instead — `check_read_access` asks `releases:list` — which makes it the
// surface where rule 2's second half is observable. It is also the document that
// is cached under `DocumentAudience`, so rule 3 matters here and nowhere else.

const GEMS: &str = "gems";
const GEM: &str = "rails";

/// A registry granting `role:user` the **list** and not the read.
fn rbac_list_without_read(
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
) -> (batlehub_core::services::RegistryPolicy, RbacFixture) {
    use batlehub_core::entities::Role;
    use std::collections::HashMap;
    let (mut policy, _) = rbac_policy_deny_anonymous(repo);
    policy.rules.clear();
    (
        policy,
        RbacFixture {
            roles: HashMap::from([
                (Role::Anonymous, vec![]),
                (Role::User, vec!["releases:list".to_owned()]),
                (Role::Admin, vec!["*".to_owned()]),
            ]),
            groups: HashMap::new(),
        },
    )
}

/// Three published versions of one gem, a grants repository, and a caller who
/// may list but not read.
async fn gems_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    gems_app_with(false).await
}

/// `gems_app`, with the whole-registry document cache a real server runs with.
///
/// The two are **different code paths**, not the same one made faster.
/// `cached_document` resolves the read set, keys it by `DocumentAudience` and
/// hands the set back; with no cache configured it falls through to
/// `readable_packages` instead. RFC 0017 §4.4 rule 3 lives on the first of
/// those — the key names the caller's version-tier grants — and every test in
/// this file ran on the second, so the keyed path had no test at all.
async fn gems_app_cached() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    gems_app_with(true).await
}

/// `gems_app` on a **Hybrid** registry with an upstream behind it.
///
/// `FixedRegistry` advertises `1.1.0` and `2.0.0-beta.1`, which the local gem
/// does not have — so a test can tell a local answer from an upstream one by
/// looking at the version numbers rather than at a status code.
async fn gems_app_hybrid() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    gems_app_full(false, RegistryMode::Hybrid, true).await
}

async fn gems_app_with(
    cached: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    gems_app_full(cached, RegistryMode::Local, false).await
}

async fn gems_app_full(
    cached: bool,
    mode: RegistryMode,
    upstream: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    use batlehub_core::services::local_registry::PublishRequest;

    let parts =
        local_only_app_parts_with_policy(GEMS, "rubygems", mode, upstream, rbac_list_without_read)
            .await;
    // Built before either lock is taken. `local_svc.hot` and `proxy_svc.hot` are
    // the *same* `Arc<RwLock<_>>` in this harness, so writing one while reading
    // the other deadlocks — and a deadlocked async test does not fail, it hangs,
    // which is how this first showed up: as three tests "running for over 60
    // seconds" under `cargo llvm-cov` rather than as a red assertion.
    let grant_repo = batlehub_adapters::in_memory::InMemoryGrantRepository::new()
        as Arc<dyn batlehub_core::ports::GrantRepository>;
    let document_cache = cached.then(batlehub_core::services::document_cache::DocumentCache::new);
    for lock in [&parts.proxy_svc.hot, &parts.local_svc.hot] {
        let mut hot = lock.write().await;
        hot.grant_repo = Some(Arc::clone(&grant_repo));
        hot.document_cache = document_cache.clone();
    }

    let admin = batlehub_core::entities::Identity {
        user_id: Some("admin".to_owned()),
        role: batlehub_core::entities::Role::Admin,
        auth_provider: Some("static-token".to_owned()),
        groups: vec![],
    };
    for v in ["1.0.0", "2.0.0", "3.0.0"] {
        parts
            .local_svc
            .publish(PublishRequest {
                registry: GEMS.to_owned(),
                name: GEM.to_owned(),
                version: v.to_owned(),
                artifact: actix_web::web::Bytes::from_static(b"gem"),
                checksum: "abc".to_owned(),
                index_metadata: json!({ "name": GEM, "version": v }),
                unlisted: false,
                publisher: admin.clone(),
                signature_bytes: None,
                signature_type: None,
            })
            .await
            .expect("publish");
    }

    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

/// The whole-registry compact index `/versions`, as `token` receives it.
///
/// The document Bundler resolves from, and the one cached under
/// `DocumentAudience` — so it is where §4.4 rule 3 is observable.
async fn registry_index<S>(app: &S, token: &str) -> String
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/{GEMS}/versions"))
            .insert_header(("Authorization", bearer(token)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "the compact index should be served");
    let body = actix_web::test::read_body(resp).await;
    String::from_utf8_lossy(&body).into_owned()
}

/// The versions the compact `/info/{gem}` document lists for `token`.
async fn info_versions<S>(app: &S, token: &str) -> Vec<String>
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/{GEMS}/info/{GEM}"))
            .insert_header(("Authorization", bearer(token)))
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "the compact info document should be served"
    );
    let body = actix_web::test::read_body(resp).await;
    String::from_utf8_lossy(&body)
        .lines()
        .skip(1) // the `---` header line
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().next().unwrap_or_default().to_owned())
        .collect()
}

async fn grant_version<S>(app: &S, version: &str, subject: &str)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let resp = call_service(
        app,
        TestRequest::put()
            .uri(&format!("/api/v1/admin/registries/{GEMS}/grants"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({
                "package": GEM, "version": version,
                "subject": subject, "actions": ["releases:read"],
            }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "granting {version} to {subject}");
}

/// §9's promise through a protocol document: until the editor is used, a
/// list-only caller's version index is exactly what it always was.
#[actix_web::test]
async fn until_a_version_grant_exists_the_index_is_unchanged() {
    let app = gems_app().await;
    assert_eq!(
        info_versions(&app, USER_TOKEN).await,
        ["1.0.0", "2.0.0", "3.0.0"]
    );
}

/// The RFC in one assertion: a version-tier grant is written through the new
/// route, and the version index served to that caller narrows to what they may
/// actually read.
///
/// Shipping phase 1 without phase 2 is this test with the old expectation —
/// three versions listed, one downloadable — which is §2.3's silent
/// under-filtering.
#[actix_web::test]
async fn a_version_grant_narrows_the_index_to_what_the_caller_may_read() {
    let app = gems_app().await;
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;
    assert_eq!(info_versions(&app, USER_TOKEN).await, ["2.0.0"]);
}

/// §4.4 rule 3, asserted rather than assumed. The narrower caller asks first, so
/// a key that did not name the version tier would populate the entry with the
/// filtered document and replay it to the admin.
#[actix_web::test]
async fn two_callers_with_different_grants_get_different_documents() {
    let app = gems_app().await;
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;

    assert_eq!(info_versions(&app, USER_TOKEN).await, ["2.0.0"]);
    assert_eq!(
        info_versions(&app, ADMIN_TOKEN).await,
        ["1.0.0", "2.0.0", "3.0.0"],
        "the admin holds the read at the instance tier, so the filter removes \
         nothing for them — and the narrower caller's document must not be \
         replayed here"
    );
}

/// The whole-registry index must name a gem the caller holds one version of.
///
/// Found by `tests/heavy/authz.sh rubygems`, not by any route test, and the
/// distance between the two is the point. `/versions` gates each name on
/// `Readable::contains`, which composed the config and package tiers and stopped
/// — so a caller whose only grant was a version-tier row was told the gem does
/// not exist. Every assertion about `/info` still passed, because `/info` was
/// reached directly by `curl`.
///
/// Bundler does not reach it directly. It resolves from `/versions`, and an
/// empty one sends it to the legacy `specs.4.8.gz` full index, which this server
/// answers `404` — so the install failed with "could not fetch specs" and the
/// filtered `/info` document was never requested. The grant was correct,
/// reachable through the API, visible in `explain`, asserted in `/info`, and
/// unusable by the one client that resolves from these documents.
#[actix_web::test]
async fn the_registry_index_names_a_gem_the_caller_holds_one_version_of() {
    let app = gems_app().await;
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;

    let body = registry_index(&app, USER_TOKEN).await;

    let line = body
        .lines()
        .find(|l| l.starts_with(GEM))
        .unwrap_or_else(|| panic!("the compact index does not name {GEM}: {body:?}"));
    // …and it names only the version they hold, so the fix widened the *name*
    // gate without widening the version list behind it.
    assert!(
        line.contains("2.0.0") && !line.contains("1.0.0") && !line.contains("3.0.0"),
        "the index line must offer 2.0.0 alone, got {line:?}"
    );
}

/// §4.4 rule 3, through the cache it is a rule about.
///
/// `two_callers_with_different_grants_get_different_documents` above asserts the
/// same property on `/info`, which is not cached — so until this test the rule
/// was asserted everywhere except where it can fail. `DocumentAudience` gained
/// `version_read_grants` for exactly this key; a key that omitted them would
/// populate the entry with the narrow caller's document and replay it.
///
/// The narrow caller asks **first**, deliberately: that is the order in which a
/// broken key discloses, and asking the admin first would pass either way.
#[actix_web::test]
async fn the_cached_registry_index_is_not_replayed_across_grant_sets() {
    let app = gems_app_cached().await;
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;

    let narrow = registry_index(&app, USER_TOKEN).await;
    let broad = registry_index(&app, ADMIN_TOKEN).await;

    let narrow_line = narrow
        .lines()
        .find(|l| l.starts_with(GEM))
        .unwrap_or_else(|| panic!("the list-only caller's index does not name {GEM}: {narrow:?}"));
    assert!(
        narrow_line.contains("2.0.0")
            && !narrow_line.contains("1.0.0")
            && !narrow_line.contains("3.0.0"),
        "the granted version alone: {narrow_line:?}"
    );

    let broad_line = broad
        .lines()
        .find(|l| l.starts_with(GEM))
        .unwrap_or_else(|| panic!("the admin's index does not name {GEM}: {broad:?}"));
    assert!(
        broad_line.contains("1.0.0") && broad_line.contains("3.0.0"),
        "the admin holds the read above, so the filter removes nothing for them — \
         and the narrow caller's cached document must not have been replayed here: \
         {broad_line:?}"
    );
}

/// The keyed path filters the same way the unkeyed one does.
///
/// Two code paths, not one made faster: with a cache configured
/// `cached_document` resolves and keys the read set, without one the service
/// falls through to `readable_packages`. Both had to learn the version tier
/// separately, and a document that differed between them would be the cache
/// deciding authorization.
#[actix_web::test]
async fn the_cached_and_uncached_indexes_agree() {
    let cached = gems_app_cached().await;
    let uncached = gems_app().await;
    grant_version(&cached, "2.0.0", "user:static-token:user-1").await;
    grant_version(&uncached, "2.0.0", "user:static-token:user-1").await;

    assert_eq!(
        registry_index(&cached, USER_TOKEN).await,
        registry_index(&uncached, USER_TOKEN).await,
        "a document built with a cache and one built without must be the same bytes"
    );
}

// ── The Hybrid fall-through, and why an empty listing is not an absent one ───

/// The `/info/{gem}` document, as `token` receives it, with its status.
async fn info_raw<S>(app: &S, token: &str) -> (u16, String)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/{GEMS}/info/{GEM}"))
            .insert_header(("Authorization", bearer(token)))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    let body = actix_web::test::read_body(resp).await;
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A listing the grant filter emptied must **not** be answered with upstream's
/// package of the same name.
///
/// This is the dependency-confusion substitution `load_visible_versions_or_not_found`
/// was written to prevent for administratively blocked packages, arriving
/// through the door RFC 0017 opened: §4.4 rule 2 takes the last version a
/// caller may read, the funnel returns empty, `emptied_by_blocking` is false
/// because no block is involved, and a plain `NotFound` tells every Hybrid
/// handler *"we do not host this, ask upstream"*.
///
/// The internal gem is real, it is hosted here, and the caller is simply not
/// allowed to see it. Answering with rubygems.org's `rails` is the worst
/// available outcome — worse than the `403` and worse than the `404` — because
/// the resolver installs it.
#[actix_web::test]
async fn a_grant_filtered_listing_does_not_fall_through_to_upstream() {
    let app = gems_app_hybrid().await;
    // Granted to somebody else, so this caller may read no version at all.
    grant_version(&app, "2.0.0", "user:someone-else").await;

    let (status, body) = info_raw(&app, USER_TOKEN).await;

    assert!(
        !body.contains("1.1.0") && !body.contains("2.0.0-beta.1"),
        "upstream's versions reached a caller asking about a gem this instance \
         hosts privately — that is the substitution the withholding exists to \
         prevent (status {status}): {body:?}"
    );
    assert_eq!(
        status, 404,
        "hidden means absent (RFC 0006, RFC 0011-bis §4.5), so the client still \
         gets a 404 — the distinction is for the fall-through, not for them"
    );
}

/// …and the fix must not close the door on genuine fall-through: a gem this
/// instance has never hosted is still answered from upstream.
///
/// The positive control. Without it, "does not serve upstream" passes against a
/// server that stopped proxying altogether.
#[actix_web::test]
async fn a_gem_that_was_never_published_here_still_falls_through() {
    let app = gems_app_hybrid().await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!("/proxy/{GEMS}/info/never-published-here"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "a gem with no local rows is a routing question, and Hybrid answers it \
         upstream — `NotFound` still means what it meant"
    );
}

// ── `explain`'s provenance across the two new tiers (§10) ────────────────────
//
// §6.3 read "no change needed; `explain` already reports package- and
// version-tier provenance because it resolves through the same path". It did
// not. `resolution_path` composes the instance, registry and namespace tiers —
// the ones a config file declares — and the stored tiers were appended by
// `authorize_grants` alone, *after* a short-circuit the diagnostics never reach
// because they are asked precisely about the callers the config tiers do not
// satisfy.
//
// So the endpoint answered `deny` where the server answers `allow`, while
// naming `package:…` and `version:…` in `tiers_walked` — a page reporting it had
// looked where it had not. RFC 0017 is what makes it reachable for any verb:
// before the editor the only stored rows were the ownership projection's three.
// §11.6: *"a diagnostic that can disagree with reality is worse than none,
// because it is trusted."*

/// The explain answer for one subject on one coordinate.
async fn explain<S>(app: &S, package: &str, version: Option<&str>, subject: &str) -> Value
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let version = version.map(|v| format!("&version={v}")).unwrap_or_default();
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!(
                "/api/v1/admin/authz/explain?registry={GEMS}&package={package}{version}\
                 &subject={subject}&action=releases:read"
            ))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "explain should answer");
    read_body_json(resp).await
}

/// A version-tier grant is reported as *allow*, and the provenance names the
/// version node it came from.
///
/// This is the assertion §10 asks for, and it is the one that fails against a
/// diagnostic resolving over the config tiers alone: `user:user-1` holds
/// `releases:read` on `rails@2.0.0` and nowhere else, so every tier `explain`
/// used to walk denies.
#[actix_web::test]
async fn explain_reports_a_version_tier_grant_s_provenance() {
    let app = gems_app().await;
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;

    let body = explain(&app, GEM, Some("2.0.0"), "user:static-token:user-1").await;
    assert_eq!(body["decision"], "allow");

    let from = body["resolved"]
        .as_array()
        .expect("resolved")
        .iter()
        .find(|v| v["action"] == "releases:read")
        .map(|v| v["granted_by"].clone())
        .expect("releases:read in the provenance");
    assert_eq!(
        from, "version:2.0.0",
        "the verb came from the version node, and the answer has to say so — \
         naming a tier it did not read is what §11.6 calls worse than none"
    );

    assert!(
        body["tiers_walked"]
            .as_array()
            .expect("tiers_walked")
            .iter()
            .filter(|t| *t == "version:2.0.0")
            .count()
            == 1,
        "the version tier is named once: twice would suggest two nodes and a \
         precedence between them, and the model has neither — got {}",
        body["tiers_walked"]
    );
}

/// The same for the package tier, and the coordinate matters: the grant is on
/// `rails@2.0.0`, so asking about `3.0.0` must still answer *deny*.
#[actix_web::test]
async fn explain_does_not_carry_a_version_grant_to_another_version() {
    let app = gems_app().await;
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;

    assert_eq!(
        explain(&app, GEM, Some("3.0.0"), "user:static-token:user-1").await["decision"],
        "deny",
        "a row on 2.0.0 resolves for 2.0.0"
    );
    assert_eq!(
        explain(&app, GEM, None, "user:static-token:user-1").await["decision"],
        "deny",
        "and the package-tier question is not the version-tier one"
    );
}

/// A package-tier grant — the tier the ownership projection has been writing
/// since RFC 0015 — reaches the answer too, and reaches every version beneath.
#[actix_web::test]
async fn explain_reports_a_package_tier_grant_s_provenance() {
    let app = gems_app().await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri(&format!("/api/v1/admin/registries/{GEMS}/grants"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(put_body(
                GEM,
                "user:static-token:user-1",
                &["releases:read"],
            ))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);

    for version in [None, Some("1.0.0"), Some("3.0.0")] {
        let body = explain(&app, GEM, version, "user:static-token:user-1").await;
        assert_eq!(body["decision"], "allow", "at {version:?}");
        assert!(
            body["resolved"]
                .as_array()
                .expect("resolved")
                .iter()
                .any(|v| v["action"] == "releases:read"
                    && v["granted_by"] == format!("package:{GEM}")),
            "the package node is the provenance at {version:?}, got {}",
            body["resolved"]
        );
    }
}

/// A grant naming someone else changes nothing for this caller — and when the
/// filter removes every version, the document is **absent, not empty**.
///
/// `load_visible_versions_or_not_found` turns an empty visible set into
/// `NotFound`, which is RFC 0006's rule and RFC 0011-bis §4.5's: hidden means
/// absent. A `200` with an empty index would confirm that `rails` exists here
/// and that its versions are all withheld, which is the disclosure the filter is
/// for.
#[actix_web::test]
async fn when_the_filter_removes_everything_the_document_is_absent() {
    let app = gems_app().await;
    grant_version(&app, "2.0.0", "user:someone-else").await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!("/proxy/{GEMS}/info/{GEM}"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status(),
        404,
        "hidden means absent: an empty index would confirm the gem is here"
    );

    // …and the grantee still sees theirs, so the filter narrowed rather than
    // broke the route.
    grant_version(&app, "2.0.0", "user:static-token:user-1").await;
    assert_eq!(info_versions(&app, USER_TOKEN).await, ["2.0.0"]);
}
