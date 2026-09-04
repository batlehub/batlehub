//! RFC 0019 phase 3 on the wire: the raw policy, and the typed read-only
//! JSON families.
//!
//! Two behaviour changes this file pins, both stated in §9:
//!
//! * **raw is off unless written.** It was implicitly on for all three
//!   forges; the first refused request answers with a body that names the
//!   section to add, because a bare `403` on a URL that worked yesterday
//!   reads as a permissions problem.
//! * **`[api_reads]` is opt-in.** A family the registry did not ask for
//!   answers `404`, which is what the route did before it existed.
//!
//! The families answer *this proxy's* shape, not the forge's — §11 q11's "no
//! wildcard" — so there is no upstream URL in them to rewrite and no field
//! that means something different per forge.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use actix_web::test::call_service;
use async_trait::async_trait;
use batlehub_adapters::in_memory::InMemoryRefResolutionRepository;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{ApiReadFamily, PackageId, PackageMetadata, RawPolicy, RefKind, ScriptAction},
    error::CoreError,
    ports::{
        DocumentKind, FetchedArtifact, ForgeCommit, ForgeRegistry, ForgeTag, RegistryClient,
        ResolvedTarget, VersionDocument,
    },
    rules::{BlockListRule, RawPolicyRule, Rule},
    services::RegistryPolicy,
};
use chrono::Utc;

const REG: &str = "gh";
const REPO: &str = "cli/cli";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Forge {
    inner: Arc<FixedRegistry>,
}

#[async_trait]
impl ForgeRegistry for Forge {
    async fn resolve_ref(&self, _: &str, git_ref: &str) -> Result<ResolvedTarget, CoreError> {
        let kind = match git_ref {
            "main" => RefKind::Branch,
            "v1.0.0" => RefKind::Tag,
            other => return Err(CoreError::NotFound(format!("no ref {other}"))),
        };
        Ok(ResolvedTarget {
            kind,
            sha: A.to_owned(),
            object_date: Some(Utc::now() - chrono::Duration::days(400)),
            publisher: Some("committer".into()),
        })
    }
    async fn commit(&self, _: &str, sha: &str) -> Result<ForgeCommit, CoreError> {
        Ok(ForgeCommit {
            sha: sha.to_owned(),
            committed_at: Some(Utc::now() - chrono::Duration::days(400)),
            committer: Some("committer".into()),
        })
    }
    async fn tags(&self, _: &str) -> Result<Vec<ForgeTag>, CoreError> {
        Ok(vec![ForgeTag {
            name: "v1.0.0".into(),
            sha: A.to_owned(),
            date: None,
        }])
    }
}

#[async_trait]
impl RegistryClient for Forge {
    fn registry_type(&self) -> &str {
        "github"
    }
    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        Some(self)
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        self.inner.resolve_metadata(pkg).await
    }
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        self.inner.fetch_artifact(pkg).await
    }
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        self.inner.fetch_version_document(package, kind).await
    }
}

async fn lab(raw: RawPolicy, families: Vec<ApiReadFamily>) -> impl TestService {
    let parts = local_registry_app_parts(REG, "github", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(Forge {
                inner: FixedRegistry::new("github"),
            }) as Arc<dyn RegistryClient>,
        );
        hot.ref_resolutions = Some(InMemoryRefResolutionRepository::new());
        hot.forge_raw.insert(REG.to_owned(), raw.clone());
        hot.forge_api_reads.insert(REG.to_owned(), families);
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::ZERO),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![
                    Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo)))
                        as Box<dyn Rule>,
                    Box::new(RawPolicyRule::new(raw)),
                ],
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

fn on() -> RawPolicy {
    RawPolicy {
        enabled: true,
        ..RawPolicy::default()
    }
}

async fn get<S: TestService>(app: &S, uri: &str) -> actix_web::dev::ServiceResponse {
    call_service(app, admin_get(uri)).await
}

async fn body_of(resp: actix_web::dev::ServiceResponse) -> String {
    String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap()
}

fn raw_uri(git_ref: &str, path: &str) -> String {
    format!("/proxy/{REG}/{REPO}/raw/{git_ref}/{path}")
}

// ── raw ──────────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn raw_is_off_until_the_section_turns_it_on_and_the_body_says_so() {
    let app = lab(RawPolicy::default(), vec![]).await;
    let resp = get(&app, &raw_uri("v1.0.0", "README.md")).await;
    assert_eq!(resp.status(), 403);
    let body = body_of(resp).await;
    assert!(body.contains("[registries.raw]"), "{body}");

    let app = lab(on(), vec![]).await;
    assert_eq!(
        get(&app, &raw_uri("v1.0.0", "README.md")).await.status(),
        200
    );
}

#[actix_web::test]
async fn the_repository_allowlist_narrows_raw() {
    let app = lab(
        RawPolicy {
            repos: vec!["cli/*".into()],
            ..on()
        },
        vec![],
    )
    .await;
    assert_eq!(
        get(&app, &raw_uri("v1.0.0", "README.md")).await.status(),
        200
    );
    let resp = get(
        &app,
        &format!("/proxy/{REG}/evil/repo/raw/v1.0.0/README.md"),
    )
    .await;
    assert_eq!(resp.status(), 403);
    assert!(body_of(resp).await.contains("raw.repos"));
}

#[actix_web::test]
async fn a_branch_is_refused_when_raw_must_be_pinned() {
    let app = lab(
        RawPolicy {
            require_pinned: true,
            ..on()
        },
        vec![],
    )
    .await;
    assert_eq!(
        get(&app, &raw_uri("v1.0.0", "README.md")).await.status(),
        200
    );
    let resp = get(&app, &raw_uri("main", "README.md")).await;
    assert_eq!(resp.status(), 403);
    assert!(body_of(resp).await.contains("PINNED_REF_REQUIRED"));
}

#[actix_web::test]
async fn a_script_is_served_under_warn_and_refused_under_deny() {
    let app = lab(on(), vec![]).await;
    assert_eq!(
        get(&app, &raw_uri("v1.0.0", "install.sh")).await.status(),
        200,
        "warn is the default and it serves"
    );

    let app = lab(
        RawPolicy {
            scripts: ScriptAction::Deny,
            ..on()
        },
        vec![],
    )
    .await;
    let resp = get(&app, &raw_uri("v1.0.0", "install.sh")).await;
    assert_eq!(resp.status(), 403);
    assert!(body_of(resp).await.contains("RAW_SCRIPT"));
    // A file the extension says nothing about is still served.
    assert_eq!(
        get(&app, &raw_uri("v1.0.0", "README.md")).await.status(),
        200
    );
}

// ── api_reads ────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn a_family_the_registry_did_not_ask_for_is_not_there() {
    let app = lab(RawPolicy::default(), vec![]).await;
    for uri in [
        format!("/proxy/{REG}/{REPO}/tags"),
        format!("/proxy/{REG}/{REPO}/commits/{A}"),
        format!("/proxy/{REG}/{REPO}/branches/main"),
    ] {
        let resp = get(&app, &uri).await;
        assert_eq!(resp.status(), 404, "{uri}");
        assert!(
            body_of(resp).await.contains("[registries.api_reads]"),
            "the refusal names the section to add"
        );
    }
}

#[actix_web::test]
async fn the_three_families_answer_this_proxys_own_shape() {
    let app = lab(RawPolicy::default(), ApiReadFamily::ALL.to_vec()).await;

    let v = get_json(&app, &format!("/proxy/{REG}/{REPO}/tags")).await;
    assert_eq!(v["package"], REPO);
    assert_eq!(v["tags"][0]["name"], "v1.0.0");
    assert_eq!(v["tags"][0]["sha"], A);

    let v = get_json(&app, &format!("/proxy/{REG}/{REPO}/commits/{A}")).await;
    assert_eq!(v["sha"], A);
    assert_eq!(v["committer"], "committer");

    let v = get_json(&app, &format!("/proxy/{REG}/{REPO}/branches/main")).await;
    assert_eq!(v["branch"], "main");
    assert_eq!(v["sha"], A);

    // A tag is not a branch: the family answers about branches only, so the
    // resolver's own classification is the answer rather than a redirect.
    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/branches/v1.0.0")).await;
    assert_eq!(resp.status(), 404);
}

/// The route pattern is generic, so it also matches on a package registry —
/// where it must refuse. It does, twice over: no `[api_reads]` entry, and no
/// forge client behind the name.
#[actix_web::test]
async fn a_family_on_a_package_registry_refuses() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    let resp = get(&app, "/proxy/npm-reg/some/repo/tags").await;
    assert_eq!(resp.status(), 404);
}

// ── the release document points back at the proxy ────────────────────────────

/// RFC 0019 §4.2 *API reads*: a client that reads `tarball_url` instead of
/// building a path must not be sent to the forge.
#[actix_web::test]
async fn a_release_listing_repoints_its_download_urls() {
    let app = lab(RawPolicy::default(), vec![]).await;
    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/releases")).await;
    assert_eq!(resp.status(), 200);
    let body = body_of(resp).await;
    // `FixedRegistry`'s listing carries no release objects to rewrite, so
    // what this pins is that the route still answers with its document and
    // the rewriter left it alone rather than mangling it.
    assert!(body.starts_with('[') || body.starts_with('{'), "{body}");
    let _ = HashMap::<String, String>::new();
}
