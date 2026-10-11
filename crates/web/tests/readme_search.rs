//! Searching what a package *says* (RFC 0007-bis §4.3).
//!
//! The ranking and the stemming are Postgres's, and they are tested against a
//! real one in `crates/adapters/tests/pg_readmes.rs`. This file is about
//! everything around them: what `in` means, what happens when the feature is
//! off, that a name match always outranks a prose match, that every row says
//! which it was, and that a snippet is text.
//!
//! The in-memory store's `search` is a substring match and deliberately does not
//! imitate stemming — a test here asserting `retry` finds `retrying` would pass
//! against a double and fail in production, which is exactly backwards.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};

use batlehub_adapters::in_memory::InMemoryReadmeRepository;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{
        AccessAction, AccessEvent, AccessResult, PackageId, PackageReadme, ReadmeFormat,
        ReadmeSource,
    },
    ports::ReadmeRepository,
    services::ReadmeService,
};

const REG: &str = "local-npm";

/// One package, its catalogue row and its README.
struct Seed {
    name: &'static str,
    readme: &'static str,
}

const SEEDS: &[Seed] = &[
    Seed {
        name: "retry",
        // Its own name appears in its own README, which is what makes a query
        // for `retry` match this row **both** ways.
        readme: "A minimal retry helper. See the docs.",
    },
    Seed {
        name: "resilience-toolkit",
        // Says `retry` in prose and not in its name, which is the row that must
        // come *after* the one called `retry`.
        readme: "Implements exponential backoff for flaky upstreams, with jitter. \
                 Use it to retry safely.",
    },
    Seed {
        name: "unrelated",
        readme: "A colour palette for terminals.",
    },
];

async fn app(
    readme_search: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo = InMemoryReadmeRepository::new();
    for seed in SEEDS {
        repo.upsert(PackageReadme {
            registry: REG.to_owned(),
            name: seed.name.to_owned(),
            version: "1.0.0".to_owned(),
            digest: batlehub_core::entities::readme_digest(seed.readme),
            content: seed.readme.to_owned(),
            format: ReadmeFormat::Markdown,
            source: ReadmeSource::LocalPublish,
            truncated: false,
            package_level: false,
            extracted_at: chrono::Utc::now(),
        })
        .await
        .expect("seed readme");
    }

    let readme_svc = Arc::new(ReadmeService::new(
        Arc::clone(&repo) as Arc<dyn ReadmeRepository>
    ));
    let mut parts = local_registry_app_parts_with_readme(
        REG,
        "npm",
        RegistryMode::Local,
        None,
        Some(readme_svc),
    );
    // The prose search scopes by the caller's *explore* set explicitly, where an
    // empty scope means "search nothing" rather than "search everything" — so
    // this app needs a config that names the registry, as production builds from
    // `[registries.rbac.explore]`. The shared helper leaves those sets empty.
    parts.access_config = access_config_with_explore(&[REG]);

    // The catalogue rows themselves. `record_access` is how a package enters the
    // catalogue on every other path, so seeding through it means these rows have
    // the same shape as real ones rather than a shape invented for the test.
    for seed in SEEDS {
        parts
            .proxy_svc
            .repo
            .record_access(AccessEvent {
                id: uuid::Uuid::new_v4(),
                user_id: Some("seed".to_owned()),
                user_role: batlehub_core::entities::Role::User,
                package_id: Some(PackageId::new(REG, seed.name, "1.0.0")),
                action: AccessAction::Download,
                result: AccessResult::Allowed,
                timestamp: chrono::Utc::now(),
                ip_address: None,
                user_agent: None,
                throttled_count: None,
                detail: None,
            })
            .await
            .expect("seed catalogue row");
    }

    build_local_registry_app_with(
        parts,
        batlehub_web::CargoIndexMap::default(),
        None,
        readme_search,
    )
    .await
}

async fn search(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    query: &str,
) -> serde_json::Value {
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!("/api/v1/explore/packages?{query}"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "search should answer 200");
    read_body_json(resp).await
}

fn names(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect()
}

// ── The default: nothing changes ─────────────────────────────────────────────

/// A parameter's absence must not change what the endpoint already did. This is
/// the assertion that keeps the whole feature additive (RFC 0007-bis §9).
#[actix_web::test]
async fn without_the_parameters_the_listing_is_what_it_always_was() {
    let app = app(true).await;
    let body = search(&app, "").await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(body["searched_in"], "name");
    // Every row still says why it is here, and for a plain listing that is
    // "name" — never null, so a client never has to handle an absent label.
    for item in body["items"].as_array().unwrap() {
        assert_eq!(item["matched_in"], "name");
        assert!(item["snippet"].is_null());
    }
}

#[actix_web::test]
async fn the_name_scope_matches_names_and_nothing_else() {
    let app = app(true).await;
    let body = search(&app, "q=retry&in=name").await;
    assert_eq!(names(&body), vec!["retry"]);
    assert_eq!(body["searched_in"], "name");

    // `backoff` appears in a README and in no name.
    let body = search(&app, "q=backoff&in=name").await;
    assert!(names(&body).is_empty(), "{body}");
}

// ── Prose ────────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_readme_scope_matches_prose_and_says_so() {
    let app = app(true).await;
    let body = search(&app, "q=backoff&in=readme").await;
    assert_eq!(names(&body), vec!["resilience-toolkit"]);

    let item = &body["items"][0];
    assert_eq!(item["matched_in"], "readme");
    let snippet = item["snippet"].as_str().expect("a snippet");
    assert!(snippet.contains("backoff"), "{snippet}");
    assert_eq!(body["searched_in"], "readme");
    assert_eq!(body["readme_search_enabled"], true);
}

/// A package literally called `retry` must come before one that merely mentions
/// retrying, however densely. Not a tuning parameter — it is what a reader means
/// when they type a name (RFC 0007-bis §4.3).
#[actix_web::test]
async fn a_name_match_always_outranks_a_prose_match() {
    let app = app(true).await;
    // Both rows match: `retry` by its name (and its README), and
    // `resilience-toolkit` by prose alone.
    let body = search(&app, "q=retry&in=both").await;
    assert_eq!(names(&body), vec!["retry", "resilience-toolkit"]);

    let items = body["items"].as_array().unwrap();
    let first_prose_only = items.iter().position(|i| i["matched_in"] == "readme");
    let last_named = items.iter().rposition(|i| i["matched_in"] != "readme");
    assert_eq!(first_prose_only, Some(1));
    assert_eq!(last_named, Some(0));
}

/// A row that matched both ways says `both` and keeps the snippet — the reader
/// gets the label *and* the reason.
#[actix_web::test]
async fn a_row_matching_both_ways_is_labelled_both() {
    let app = app(true).await;
    let body = search(&app, "q=retry&in=both").await;
    let items = body["items"].as_array().unwrap();

    assert_eq!(items[0]["name"], "retry");
    assert_eq!(items[0]["matched_in"], "both");
    let snippet = items[0]["snippet"]
        .as_str()
        .expect("a snippet on a both row");
    assert!(snippet.contains("retry"), "{snippet}");

    // And the prose-only row is labelled for what it is.
    assert_eq!(items[1]["name"], "resilience-toolkit");
    assert_eq!(items[1]["matched_in"], "readme");
}

/// The snippet is package-authored content on a second surface, and it is text.
/// It never reaches `v-html` (RFC 0007-bis §7.4).
#[actix_web::test]
async fn a_snippet_is_text_and_carries_no_markup_of_ours() {
    let app = app(true).await;
    let body = search(&app, "q=jitter&in=readme").await;
    let snippet = body["items"][0]["snippet"].as_str().expect("a snippet");
    assert!(!snippet.contains("<b>"), "{snippet}");
    assert!(!snippet.contains("</b>"), "{snippet}");
    assert!(
        !snippet.contains('\n'),
        "a snippet is one line: {snippet:?}"
    );
}

// ── With the feature off ─────────────────────────────────────────────────────

/// `in=readme` with prose search off is accepted and answers exactly as
/// `in=name` does — **and says so**. A parameter that silently means something
/// else is the failure this RFC family keeps finding; one that reports
/// "prose search is not enabled on this instance" is one an operator can act on
/// (RFC 0007-bis §4.3).
#[actix_web::test]
async fn the_scope_is_accepted_and_reported_when_prose_search_is_off() {
    let app = app(false).await;
    let body = search(&app, "q=backoff&in=readme").await;

    assert_eq!(body["readme_search_enabled"], false);
    assert_eq!(
        body["searched_in"], "name",
        "the scope actually applied, not the one asked for"
    );
    // `backoff` is a name nothing has, so the answer is empty — as a name search
    // for it would be.
    assert!(names(&body).is_empty(), "{body}");

    // And a name search is unaffected.
    let body = search(&app, "q=retry&in=readme").await;
    assert_eq!(names(&body), vec!["retry"]);
    assert_eq!(body["items"][0]["matched_in"], "name");
}

// ── What it does not widen ───────────────────────────────────────────────────

/// An `internal` package does not become discoverable by quoting a phrase from
/// its README, which would otherwise be a neat oracle: the name is hidden, the
/// prose is not, so guess the prose (RFC 0007-bis §7.3).
///
/// Asserted through a registry the caller cannot explore, which is the coarse
/// half of the same gate and the one this endpoint applies first.
#[actix_web::test]
async fn a_registry_the_caller_cannot_explore_is_not_searched() {
    let app = app(true).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/explore/packages?q=backoff&in=readme&registry=not-mine")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = read_body_json(resp).await;
    assert!(names(&body).is_empty(), "{body}");
    assert_eq!(body["total"], 0);
}

/// An empty or whitespace query is a listing, not a search that matches
/// everything and not one that errors.
#[actix_web::test]
async fn an_empty_query_lists_rather_than_searching() {
    let app = app(true).await;
    for query in ["q=&in=readme", "q=%20%20&in=both", "in=readme"] {
        let body = search(&app, query).await;
        assert_eq!(
            body["items"].as_array().unwrap().len(),
            3,
            "{query} should list everything"
        );
    }
}

/// Pagination still means something on the merged path: the page is a window
/// over the merged order, and `total` counts what the window is cut from.
#[actix_web::test]
async fn the_merged_result_paginates() {
    let app = app(true).await;
    let first = search(&app, "q=a&in=both&per_page=1&page=0").await;
    let second = search(&app, "q=a&in=both&per_page=1&page=1").await;

    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["per_page"], 1);
    assert_eq!(first["page"], 0);
    assert_eq!(second["page"], 1);
    // Different rows, and both counted in the same total.
    if !second["items"].as_array().unwrap().is_empty() {
        assert_ne!(first["items"][0]["name"], second["items"][0]["name"]);
    }
    assert_eq!(first["total"], second["total"]);
    // Nothing was silently dropped.
    assert_eq!(first["truncated"], false);
}
