//! The **Fetch this version** button (RFC 0007-bis §4.4, §5.3, §7.5).
//!
//! The button's entire security argument is that it *is* the download path.
//! That argument is only true while it stays the download path, which is a
//! property of one call site and therefore easy to erode — somebody optimising a
//! slow fetch by "reusing the warming service" would silently remove every gate,
//! and it would look like reuse rather than like a hole.
//!
//! So two of the tests here are the ones that would fail against a
//! warming-service implementation, and they are the reason this file exists:
//!
//! - **the refusal**, not just the success: a caller whose role cannot download
//!   gets `403` with the rule's own reason. Warming calls `fetch_artifact`
//!   directly and would have succeeded;
//! - **the audit event**, with the caller as the actor. Warming records none, so
//!   this is the assertion that fails the moment the paths are swapped.
//!
//! A test that only checked the happy path would pass against either.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body_json, TestRequest};

use std::sync::Arc;

use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{AccessAction, EventFilter};
use batlehub_core::ports::RegistryClient;

const REG: &str = "local-npm";

fn fetch_uri(name: &str, version: &str) -> String {
    format!("/api/v1/explore/packages/{REG}/{name}/{version}/fetch")
}

/// A proxy-mode app with an upstream that answers, so the fetch has something to
/// pull.
async fn app(
    kind: &str,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<dyn batlehub_core::ports::PackageRepository>,
) {
    let parts = local_registry_app_parts(REG, kind, RegistryMode::Proxy, None);
    let repo = Arc::clone(&parts.proxy_svc.repo);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, repo)
}

/// [`app`], with the registry open to anonymous readers as well.
///
/// The default fixture grants nothing to `anonymous`, and the detail endpoint
/// now refuses a registry the caller may not browse — so a signed-out `GET`
/// against it is a `404` and never reaches the question of what the page offers.
/// A test about the *button* needs a reader who can see the page.
async fn app_open_to_anonymous(
    kind: &str,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let mut parts = local_registry_app_parts(REG, kind, RegistryMode::Proxy, None);
    parts.access_config = access_config_for(&[REG]);
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

// ── The happy path ───────────────────────────────────────────────────────────

/// It runs the download, and reports what arrived. The size and duration are
/// there so the row can say what the wait bought — the sampled range is 0.57 MB
/// to 41.7 MB (§13.4), wide enough that "done" alone tells a reader nothing.
#[actix_web::test]
async fn a_fetch_pulls_the_version_and_reports_what_arrived() {
    let (app, _) = app("npm").await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "the fetch should succeed");

    let body: serde_json::Value = read_body_json(resp).await;
    assert_eq!(body["fetched"], true);
    assert_eq!(body["registry"], REG);
    assert_eq!(body["name"], "widget");
    assert_eq!(body["version"], "1.0.0");
    assert!(body["size_bytes"].as_u64().unwrap() > 0, "{body}");
    assert!(body["duration_ms"].is_number(), "{body}");
}

/// The bytes land in storage through the ordinary path, which is what makes the
/// row change from `upstream` to `proxied` — and what makes SBOM and README
/// extraction run without a second mechanism (§4.4).
#[actix_web::test]
async fn the_fetched_bytes_are_actually_held_afterwards() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let storage = Arc::clone(&parts.proxy_svc.storage);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    // The **proxy** key. `artifact_storage_key` is the `local:` one a *published*
    // artifact goes to, and asking it here is a question always answered "no".
    //
    // And the key with npm's `tarball` sub-coordinate, because that is the one
    // `download_tarball` reads back. This assertion used to name the bare
    // coordinate, which is what let the bug through: the fetch really did write
    // a key, the test really did find it, and `npm install` still went upstream
    // because nothing reads there. The bare key is asserted *absent* below for
    // the same reason.
    let held = batlehub_core::services::proxy::proxy_artifact_key(
        &batlehub_core::entities::PackageId::new(REG, "widget", "1.0.0").with_artifact("tarball"),
    );
    let bare = batlehub_core::services::proxy::proxy_artifact_key(
        &batlehub_core::entities::PackageId::new(REG, "widget", "1.0.0"),
    );
    assert!(!storage.exists(&held).await.unwrap(), "nothing held before");

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(
        storage.exists(&held).await.unwrap(),
        "the artifact must be held under the key the download path reads"
    );
    assert!(
        !storage.exists(&bare).await.unwrap(),
        "the bare coordinate is a slot nothing reads; writing it is the bug"
    );
}

/// A second press is a cache read dressed as a fetch. `409` says which, so the
/// console refreshes the row rather than reporting a download that did not
/// happen.
#[actix_web::test]
async fn fetching_a_version_already_held_is_a_conflict() {
    let (app, _) = app("npm").await;
    let request = || {
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request()
    };

    assert_eq!(call_service(&app, request()).await.status(), 200);
    let second = call_service(&app, request()).await;
    assert_eq!(second.status(), 409);
    let body: serde_json::Value = read_body_json(second).await;
    assert_eq!(body["code"], "fetch.already-held");
}

// ── The two that would pass against a warming-service implementation ─────────

/// **The refusal.** A caller whose role cannot download a version is refused,
/// with the rule's own reason — the same string the download would have given,
/// so the console shows the operator *why* and `/tools/access-check` explains
/// the same verdict.
///
/// `WarmingService` calls `fetch_artifact` directly and would have succeeded
/// here, which is exactly why this test exists (§5.3, §7.5).
#[actix_web::test]
async fn a_caller_the_rules_would_refuse_is_refused_with_the_rules_reason() {
    // A *signed-in* caller the rules refuse.
    //
    // This used to send the request anonymously, because anonymous has
    // `releases:read` and not `source:read` in the shared test policy. That is
    // no longer a rules question: an anonymous caller is now turned away at the
    // door with `401` (see `an_anonymous_caller_cannot_pull_at_all`), which
    // would have made this test pass for the wrong reason and stop defending
    // what it exists to defend. So the policy is narrowed instead — `User`
    // keeps metadata and loses the bytes — and the caller carries a token.
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        let perms = std::collections::HashMap::from([
            (
                batlehub_core::entities::Role::Anonymous,
                vec!["releases:read".to_owned()],
            ),
            (
                batlehub_core::entities::Role::User,
                vec!["releases:read".to_owned()],
            ),
            (batlehub_core::entities::Role::Admin, vec!["*".to_owned()]),
        ]);
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(batlehub_core::services::RegistryPolicy {
                metadata_ttl: Some(std::time::Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![Box::new(
                    batlehub_core::rules::RbacRule::from_patterns(perms)
                        .expect("fixture rbac patterns are valid"),
                )],
            }),
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403, "the rules must refuse this caller");

    let body: serde_json::Value = read_body_json(resp).await;
    assert_eq!(body["code"], "fetch.denied");
    let message = body["message"].as_str().unwrap_or("");
    assert!(
        !message.is_empty(),
        "the refusal must carry a reason: {body}"
    );
}

/// **The audit event**, with the caller as the actor.
///
/// This is the difference from a page view in one line: a page view has no actor
/// because nobody decided anything; a fetch has one, and the audit log names
/// them. Warming records nothing, so this assertion is what fails the moment the
/// two paths are swapped (§7.5).
#[actix_web::test]
async fn the_fetch_is_audited_with_the_caller_as_the_actor() {
    let (app, repo) = app("npm").await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let events = repo
        .list_events(EventFilter {
            limit: 100,
            ..Default::default()
        })
        .await
        .expect("events");
    let download = events
        .iter()
        .find(|e| {
            e.action == AccessAction::Download
                && e.package_id
                    .as_ref()
                    .is_some_and(|p| p.name == "widget" && p.version == "1.0.0")
        })
        .unwrap_or_else(|| panic!("a download event must be recorded; got {events:?}"));

    assert_eq!(
        download.user_id.as_deref(),
        Some("user-1"),
        "the audit log must name the caller, not the server"
    );
}

// ── What it will not offer ───────────────────────────────────────────────────

/// "Fetch this version" has no single meaning for a kind whose artifact is a set
/// of files, so the endpoint refuses with the reason rather than fetching
/// something arbitrary. The console renders the same string beside a button it
/// does not draw (§4.4).
#[actix_web::test]
async fn a_kind_with_no_single_artifact_per_version_refuses_with_its_reason() {
    // `pypi` and `conda` are here because they used to be *offered*, and could
    // not work: with no filename the PyPI client looks for a file named `""` in
    // the version's `urls` and 404s, and conda carries the channel platform in
    // the version slot, so a fetch of `numpy 1.24.0` asked upstream for
    // `{base}/1.24.0/repodata.json` — a path that cannot exist. A button that
    // always fails is worse than a stated reason.
    for (kind, expected) in [
        ("maven", "set of files"),
        ("terraform", "architecture"),
        ("pypi", "wheel"),
        ("conda", "build string"),
    ] {
        let (app, _) = app(kind).await;
        let resp = call_service(
            &app,
            TestRequest::post()
                .uri(&fetch_uri("widget", "1.0.0"))
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 400, "{kind} must refuse");

        let body: serde_json::Value = read_body_json(resp).await;
        assert_eq!(body["code"], "fetch.unsupported", "{kind}");
        let message = body["message"].as_str().unwrap_or("");
        assert!(
            message.contains(expected),
            "{kind}: the refusal must quote the kind's own reason, got {message:?}"
        );
    }
}

/// The operator's switch. It admits nothing, so its only job is to let an
/// operator keep the console strictly read-only — and it has to actually do
/// that.
#[actix_web::test]
async fn console_fetch_off_refuses_before_anything_is_fetched() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let storage = Arc::clone(&parts.proxy_svc.storage);
    {
        let mut hot = parts.local_svc.hot.write().await;
        hot.console_fetch.insert(REG.to_owned(), false);
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = read_body_json(resp).await;
    assert_eq!(body["code"], "fetch.unsupported");

    // And nothing was pulled: a switch that refused the response after making
    // the request would not be the read-only posture an operator set it for.
    let key = batlehub_core::services::proxy::proxy_artifact_key(
        &batlehub_core::entities::PackageId::new(REG, "widget", "1.0.0"),
    );
    assert!(!storage.exists(&key).await.unwrap());
}

/// A registry this instance does not have answers `404`, in the vocabulary of
/// registries — not a `400` about an unknown registry *type*, which would be an
/// answer to a question the caller did not ask.
#[actix_web::test]
async fn an_unknown_registry_is_a_404_rather_than_a_complaint_about_its_type() {
    let (app, _) = app("npm").await;
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/explore/packages/not-a-registry/widget/1.0.0/fetch")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404, "unexpected {}", resp.status());
}

// ── A pull is an authenticated act ───────────────────────────────────────────

/// **An unauthenticated reader cannot pull.**
///
/// The endpoint used to admit an anonymous caller, on the argument that the
/// fetch downloads exactly what that caller could already pull through the proxy
/// with `curl`, so it grants no *read* they did not have. The argument is sound
/// about reading and incomplete about the act: a fetch is a write to this
/// instance — it fills the cache, spends bandwidth on both sides, extracts an
/// SBOM and writes an audit row whose actor would read `anonymous`.
///
/// Measured against a running instance before this check existed, an anonymous
/// `POST …/strip-ansi/7.2.0/fetch` answered `409 already-held`: it had passed
/// visibility, the operator's switch and the kind check, and was stopped only by
/// the artifact happening to already be there.
#[actix_web::test]
async fn an_anonymous_caller_cannot_pull_at_all() {
    let (app, _) = app("npm").await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .to_request(),
    )
    .await;

    assert_eq!(resp.status(), 401, "a pull requires a session");
    let body: serde_json::Value = read_body_json(resp).await;
    assert_eq!(body["code"], "fetch.unauthenticated");
}

/// And it is refused *before* the download, not after.
///
/// The status code alone would pass against a handler that pulled the bytes and
/// then declined to say so, which would leave the instance holding an artifact
/// nobody was authorised to ask for. Storage is the assertion that cannot be
/// satisfied that way.
#[actix_web::test]
async fn an_anonymous_attempt_pulls_nothing() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let storage = Arc::clone(&parts.proxy_svc.storage);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let key = batlehub_core::services::proxy::proxy_artifact_key(
        &batlehub_core::entities::PackageId::new(REG, "widget", "1.0.0"),
    );

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 401);

    assert!(
        !storage.exists(&key).await.unwrap(),
        "a refused fetch must leave nothing behind"
    );
}

/// **And the refusal says nothing about whether the package exists.**
///
/// The anonymous check used to run *after* the visibility check, on the
/// reasoning that a package a caller may not see must answer `404` first. It is
/// the wrong way round: `check_visibility` answers `Public` for any name with no
/// `local_packages` row, so the pair `401`/`404` told an unauthenticated caller
/// exactly which names are published `internal` or `team` — an enumeration
/// oracle for private package names, handed to someone with no session at all.
///
/// Refusing first leaks nothing, which is what this asserts: the same `401` for
/// a name that is internal here and for one that does not exist.
#[actix_web::test]
async fn an_anonymous_refusal_does_not_disclose_a_private_package() {
    use batlehub_core::entities::Visibility;
    use batlehub_core::ports::TeamNamespacePort;

    let ns_store = batlehub_adapters::in_memory::InMemoryTeamNamespaceStore::new();
    ns_store
        .set_visibility(REG, "widget", Visibility::Internal)
        .await
        .unwrap();

    let ns_port: std::sync::Arc<dyn TeamNamespacePort> = ns_store;
    let mut parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let base = Arc::clone(&parts.local_svc);
    parts.local_svc = Arc::new(batlehub_core::services::LocalRegistryService {
        backend: Arc::clone(&base.backend),
        storage: Arc::clone(&base.storage),
        hot: Arc::clone(&base.hot),
        quota: None,
        ownership: None,
        team_namespace: Some(ns_port),
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    });
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    for name in ["widget", "no-such-package"] {
        let resp = call_service(
            &app,
            TestRequest::post()
                .uri(&fetch_uri(name, "1.0.0"))
                .to_request(),
        )
        .await;
        assert_eq!(
            resp.status(),
            401,
            "{name}: an anonymous caller must be refused identically, existing or not"
        );
    }
}

/// The **listing** is told too (RFC 0007-bis §11 q3).
///
/// The catalogue offers the fetch on an upstream-only row, and it has to ask the
/// same question the package page asks — one screen earlier and once per
/// registry, because a listing with no registry filter spans every registry the
/// caller may browse and `console_fetch` is per registry.
///
/// Anonymous first, for the reason the test below gives: a page that drew the
/// button for a signed-out reader would be promising a `401`.
#[actix_web::test]
async fn the_listing_is_told_whether_it_may_offer_a_fetch() {
    let app = app_open_to_anonymous("npm").await;
    let uri = "/api/v1/explore/upstream?name=fixed";

    let anon: serde_json::Value =
        read_body_json(call_service(&app, TestRequest::get().uri(uri).to_request()).await).await;
    let items = anon["items"].as_array().expect("items");
    assert!(
        !items.is_empty(),
        "the fixture upstream should answer: {anon}"
    );
    for item in items {
        assert_eq!(
            item["fetch"]["offered"], false,
            "a signed-out reader is offered nothing: {item}"
        );
    }

    let signed_in: serde_json::Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri(uri)
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    for item in signed_in["items"].as_array().expect("items") {
        assert_eq!(
            item["fetch"]["offered"], true,
            "a signed-in reader is: {item}"
        );
        assert!(
            item["latest_version"]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
            "the row must name the version the button would fetch: {item}"
        );
    }
}

/// The `already_cached` flag has to be asked by the **names that came back**.
///
/// An upstream search is a relevance search: npm answers `left-pad` with
/// `pad-left`, `lpad` and `@stdlib/string-left-pad`, and not one of those
/// contains the query as a substring. The flag used to be computed by asking
/// the catalogue for packages whose name *contains the query*, so every such
/// row was reported as not held however many times the instance had pulled it.
///
/// Nothing noticed until the Fetch button reached the listing (§11 q3): pressing
/// it fetched the version, the row went on offering to fetch it, and a second
/// press answered `409 fetch.already-held`. Found in `console_fetch.sh` against
/// a real browser and the real npm registry (§14.11).
///
/// The `before` call is not scene-setting. It is what makes this a test of both
/// halves: it populates the ten-minute explore cache with the answer "we hold
/// none of these", and the fetch has to invalidate that cache or the `after`
/// call is served the same stale row whatever the filter asks.
///
/// `FixedRegistry` cannot reproduce it — its search filters on
/// `name.contains(query)`, so its answers always contain the query. This client
/// answers any query with one fixed name, which is what a relevance search does.
#[derive(Clone)]
struct RelevanceSearch {
    inner: Arc<dyn RegistryClient>,
    /// The one name every query is answered with, in the spelling the upstream
    /// *displays* — which is not always the one the read path stores.
    answers: String,
}

#[async_trait::async_trait]
impl RegistryClient for RelevanceSearch {
    fn registry_type(&self) -> &str {
        self.inner.registry_type()
    }
    async fn resolve_metadata(
        &self,
        pkg: &batlehub_core::entities::PackageId,
    ) -> Result<batlehub_core::entities::PackageMetadata, batlehub_core::error::CoreError> {
        self.inner.resolve_metadata(pkg).await
    }
    async fn fetch_artifact(
        &self,
        pkg: &batlehub_core::entities::PackageId,
    ) -> Result<batlehub_core::ports::FetchedArtifact, batlehub_core::error::CoreError> {
        self.inner.fetch_artifact(pkg).await
    }
    async fn list_versions(
        &self,
        name: &str,
    ) -> Result<Vec<String>, batlehub_core::error::CoreError> {
        self.inner.list_versions(name).await
    }
    /// Whatever you asked for, here is the one name — `widget` for the
    /// relevance test, whose name shares no substring with the queries below.
    async fn search_packages(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<batlehub_core::ports::UpstreamPackage>, batlehub_core::error::CoreError> {
        Ok(vec![batlehub_core::ports::UpstreamPackage {
            name: self.answers.clone(),
            latest_version: "1.0.0".to_owned(),
            description: None,
        }])
    }
}

/// Swap the fixture's client for one that answers every search with `answers`.
async fn answer_every_search_with(parts: &LocalRegistryAppParts, registry: &str, answers: &str) {
    let mut hot = parts.proxy_svc.hot.write().await;
    let inner = hot
        .registries
        .get(registry)
        .expect("the fixture registry")
        .clone();
    hot.registries.insert(
        registry.to_owned(),
        Arc::new(RelevanceSearch {
            inner,
            answers: answers.to_owned(),
        }),
    );
}

#[actix_web::test]
async fn a_hit_the_instance_already_holds_is_marked_however_it_was_matched() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    answer_every_search_with(&parts, REG, "widget").await;
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    // A query `widget` does not contain, which is the whole point.
    let search_uri = "/api/v1/explore/upstream?name=tegdiw";
    macro_rules! search {
        () => {{
            let resp = call_service(
                &app,
                TestRequest::get()
                    .uri(search_uri)
                    .insert_header(("Authorization", bearer(USER_TOKEN)))
                    .to_request(),
            )
            .await;
            read_body_json::<serde_json::Value, _>(resp).await
        }};
    }

    let before = search!();
    assert_eq!(before["items"][0]["name"], "widget", "{before}");
    assert_eq!(
        before["items"][0]["already_cached"], false,
        "nothing is held yet: {before}"
    );

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "the fetch should succeed");

    let after = search!();
    assert_eq!(
        after["items"][0]["already_cached"], true,
        "the instance holds it now, and the row has to say so however the \
         upstream matched it: {after}"
    );
}

/// …and matched in the spelling the *read path* stores, not the one the
/// upstream displays.
///
/// NuGet ids are case-insensitive: the search API answers `Widget.Core`, while
/// `dotnet restore` — and the flat handler, the client and the fetch coordinate
/// with it — address `widget.core`. PyPI (PEP 503) and the GOPROXY case
/// encoding do the same thing in their own alphabets. Compared exactly, the two
/// spellings never match, so the row reported a package the instance holds as
/// missing and the button beside it answered `409 fetch.already-held` — §14.11's
/// symptom surviving in the ecosystems that spell a name two ways.
///
/// The fetch here is the real one, so nothing in this test asserts against a
/// name a test wrote by hand: the coordinate is built by the kind, stored by the
/// proxy, and read back through the same canonicalisation the flag uses.
#[actix_web::test]
async fn a_hit_is_matched_in_the_spelling_the_read_path_stores() {
    const NUGET_REG: &str = "local-npm";
    let parts = local_registry_app_parts(NUGET_REG, "nuget", RegistryMode::Proxy, None);
    answer_every_search_with(&parts, NUGET_REG, "Widget.Core").await;
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let search_uri = "/api/v1/explore/upstream?name=widget";
    macro_rules! search {
        () => {{
            let resp = call_service(
                &app,
                TestRequest::get()
                    .uri(search_uri)
                    .insert_header(("Authorization", bearer(USER_TOKEN)))
                    .to_request(),
            )
            .await;
            read_body_json::<serde_json::Value, _>(resp).await
        }};
    }

    let before = search!();
    assert_eq!(
        before["items"][0]["name"], "Widget.Core",
        "the row keeps the upstream's display spelling: {before}"
    );
    assert_eq!(
        before["items"][0]["already_cached"], false,
        "nothing is held yet: {before}"
    );

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("Widget.Core", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "the fetch should succeed");

    let after = search!();
    assert_eq!(
        after["items"][0]["already_cached"], true,
        "the instance holds it as `widget.core`, and the row has to say so \
         though the search spells it `Widget.Core`: {after}"
    );
}

/// What the caller asks for is not what the registries are asked for.
///
/// The search fans out across every registry the reader may browse, and each
/// hit becomes up to two coordinates in the held-set query's array and one unit
/// of its `LIMIT`. An unbounded `limit` is therefore a caller sizing this
/// instance's database work, and the honest bound is ours to set rather than
/// one borrowed from whatever each upstream happens to enforce.
///
/// A client that reports the number it was handed, because that is the fact
/// under test — not how many rows came back, which every real client caps on
/// its own long before the ceiling is reached.
#[derive(Clone)]
struct EchoLimit(Arc<dyn RegistryClient>);

#[async_trait::async_trait]
impl RegistryClient for EchoLimit {
    fn registry_type(&self) -> &str {
        self.0.registry_type()
    }
    async fn resolve_metadata(
        &self,
        pkg: &batlehub_core::entities::PackageId,
    ) -> Result<batlehub_core::entities::PackageMetadata, batlehub_core::error::CoreError> {
        self.0.resolve_metadata(pkg).await
    }
    async fn fetch_artifact(
        &self,
        pkg: &batlehub_core::entities::PackageId,
    ) -> Result<batlehub_core::ports::FetchedArtifact, batlehub_core::error::CoreError> {
        self.0.fetch_artifact(pkg).await
    }
    async fn list_versions(
        &self,
        name: &str,
    ) -> Result<Vec<String>, batlehub_core::error::CoreError> {
        self.0.list_versions(name).await
    }
    async fn search_packages(
        &self,
        _query: &str,
        limit: usize,
    ) -> Result<Vec<batlehub_core::ports::UpstreamPackage>, batlehub_core::error::CoreError> {
        Ok(vec![batlehub_core::ports::UpstreamPackage {
            name: format!("asked-for-{limit}"),
            latest_version: "1.0.0".to_owned(),
            description: None,
        }])
    }
}

#[actix_web::test]
async fn a_registry_is_never_asked_for_more_than_the_ceiling() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        let inner = hot
            .registries
            .get(REG)
            .expect("the fixture registry")
            .clone();
        hot.registries
            .insert(REG.to_owned(), Arc::new(EchoLimit(inner)));
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let asked_for = |uri: &'static str| {
        let app = &app;
        async move {
            let resp = call_service(
                app,
                TestRequest::get()
                    .uri(uri)
                    .insert_header(("Authorization", bearer(USER_TOKEN)))
                    .to_request(),
            )
            .await;
            let body: serde_json::Value = read_body_json(resp).await;
            body["items"][0]["name"].as_str().unwrap_or("").to_owned()
        }
    };

    assert_eq!(
        asked_for("/api/v1/explore/upstream?name=x&limit=100000").await,
        format!(
            "asked-for-{}",
            batlehub_web::handlers::front_office::explore::MAX_UPSTREAM_SEARCH_LIMIT
        ),
        "a caller may not size this instance's work"
    );
    // A ceiling, not a fixed size: below it the caller's number is the one used,
    // and the default is what an unasked caller gets.
    assert_eq!(
        asked_for("/api/v1/explore/upstream?name=x&limit=3").await,
        "asked-for-3",
        "asking for less is honoured"
    );
    assert_eq!(
        asked_for("/api/v1/explore/upstream?name=x").await,
        "asked-for-10",
        "the default is unchanged"
    );
}

/// The console is told, so it does not draw a button the API will refuse.
///
/// The offer and the endpoint have to agree — that is the whole reason the
/// server answers this rather than letting the page guess (§4.4). A page that
/// drew the button for a signed-out reader would be promising a `401`.
#[actix_web::test]
async fn the_button_is_not_offered_to_an_anonymous_reader() {
    let app = app_open_to_anonymous("npm").await;
    let uri = format!("/api/v1/explore/packages/{REG}/widget");

    let anon: serde_json::Value =
        read_body_json(call_service(&app, TestRequest::get().uri(&uri).to_request()).await).await;
    assert_eq!(
        anon["fetch"]["offered"], false,
        "a signed-out reader is not offered the button: {anon}"
    );

    let signed_in: serde_json::Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri(&uri)
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    assert_eq!(
        signed_in["fetch"]["offered"], true,
        "a signed-in reader still is: {signed_in}"
    );
}

/// A registry `rbac.explore` denies is not fetchable from the console either.
///
/// The detail, README and image endpoints all refuse a registry the caller may
/// not browse; this one did not, and that made the console's own API a door
/// around the gate. `409 fetch.already-held` is an inventory oracle — it says
/// this instance holds that exact version — and the success path drives the
/// instance into fetching the ones it does not, into a registry the operator
/// has said this role may not look at.
///
/// `404`, not `403`: denied and absent read the same from outside, so the
/// refusal does not confirm the package exists. And nothing is pulled.
#[actix_web::test]
async fn a_registry_the_caller_may_not_browse_is_not_fetchable_either() {
    let mut parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    // Full proxy access, no explore access — `[registries.rbac.explore]` with
    // every tier off, which is what an operator writes to keep a registry out
    // of the console while package managers go on using it.
    //
    // Said in the **hierarchy** rather than only in `AccessConfig`: RFC 0015 §4.2
    // resolves `catalogue:browse` from grants now, and §10 rule 2's conjunction
    // is what turns these flags into the grant. A fixture that changed only the
    // access config would be describing a mechanism the handler no longer reads —
    // and would pass while testing nothing (§13.5).
    parts.access_config = access_config_explore_denied(&[REG]);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.grants = [(
            REG.to_owned(),
            Arc::new(fixture_grants_with_explore(
                REG,
                "npm",
                &RegistryMode::Proxy,
                &rbac_policy_perms(),
                false,
            )),
        )]
        .into();
    }
    let storage = Arc::clone(&parts.proxy_svc.storage);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let key = batlehub_core::services::proxy::proxy_artifact_key(
        &batlehub_core::entities::PackageId::new(REG, "widget", "1.0.0"),
    );

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&fetch_uri("widget", "1.0.0"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status(),
        404,
        "a registry the caller cannot browse must not be fetchable"
    );

    assert!(
        !storage.exists(&key).await.unwrap(),
        "a refused fetch must leave nothing behind"
    );
}
