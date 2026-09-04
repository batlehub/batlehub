//! RFC 0019 phase 2 on the wire: what a forge ref does to a request.
//!
//! Three facts, each with an operator-chosen action — a branch followed
//! (`MUTABLE_REF`, warned by default), a tag that resolves elsewhere than it
//! did (`TAG_MOVED`, denied), a release asset whose digest changed
//! (`ASSET_REPLACED`, denied) — and two ways they reach the client:
//!
//! * on a registry with `[registries.security]` they ride the request's
//!   verdict, so the response carries `X-BatleHub-Verdict: warned` and the
//!   codes beside whatever the verdict already said;
//! * on a forge registry without one, a `deny` is a plain `403` naming the
//!   code and a `warn` is the ref headers alone, which is the degradation
//!   §6.1 names and this file pins.
//!
//! The forge is a fake whose tags *move*, which is the thing no unit test can
//! stage: detection compares what the forge says now with what the resolution
//! table recorded, so it takes two requests either side of a TTL.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::test::call_service;
use async_trait::async_trait;
use batlehub_adapters::in_memory::{
    InMemoryRefResolutionRepository, InMemoryScanQueue, InMemoryVerdictRepository,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{
        ForgeRefsPolicy, PackageId, PackageMetadata, RefAction, RefKind, SecurityMode,
        SecurityPolicy,
    },
    error::CoreError,
    ports::{
        DocumentKind, FetchedArtifact, ForgeCommit, ForgeRegistry, RegistryClient, ResolvedTarget,
        ScanQueue, VerdictRepository, VersionDocument,
    },
    rules::{BlockListRule, ForgeRefRule, VerdictGateRule},
    services::{RegistryPolicy, VerdictService},
};
use chrono::{DateTime, Utc};

const REG: &str = "gh";
const REPO: &str = "cli/cli";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A forge whose tags and branches both move.
struct MovingForge {
    inner: Arc<FixedRegistry>,
    tags: Mutex<HashMap<String, (String, DateTime<Utc>)>>,
    branches: Mutex<HashMap<String, (String, DateTime<Utc>)>>,
    /// The digest the forge advertises for the release asset, if any.
    asset_digest: Mutex<Option<String>>,
}

impl MovingForge {
    fn new() -> Arc<Self> {
        let old = Utc::now() - chrono::Duration::days(400);
        Arc::new(Self {
            inner: FixedRegistry::new("github"),
            tags: Mutex::new(HashMap::from([("v1.0.0".to_owned(), (A.to_owned(), old))])),
            branches: Mutex::new(HashMap::from([("main".to_owned(), (A.to_owned(), old))])),
            asset_digest: Mutex::new(None),
        })
    }

    fn move_tag(&self, name: &str, sha: &str) {
        let at = Utc::now() - chrono::Duration::days(400);
        self.tags
            .lock()
            .unwrap()
            .insert(name.to_owned(), (sha.to_owned(), at));
    }
}

#[async_trait]
impl ForgeRegistry for MovingForge {
    async fn resolve_ref(&self, _: &str, git_ref: &str) -> Result<ResolvedTarget, CoreError> {
        if let Some((sha, date)) = self.tags.lock().unwrap().get(git_ref) {
            return Ok(ResolvedTarget {
                kind: RefKind::Tag,
                sha: sha.clone(),
                object_date: Some(*date),
                publisher: Some("tagger".into()),
            });
        }
        if let Some((sha, date)) = self.branches.lock().unwrap().get(git_ref) {
            return Ok(ResolvedTarget {
                kind: RefKind::Branch,
                sha: sha.clone(),
                object_date: Some(*date),
                publisher: None,
            });
        }
        Err(CoreError::NotFound(format!("no ref {git_ref}")))
    }
    async fn commit(&self, _: &str, sha: &str) -> Result<ForgeCommit, CoreError> {
        Ok(ForgeCommit {
            sha: sha.to_owned(),
            committed_at: None,
            committer: None,
        })
    }
}

#[async_trait]
impl RegistryClient for MovingForge {
    fn registry_type(&self) -> &str {
        "github"
    }
    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        Some(self)
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let mut meta = self.inner.resolve_metadata(pkg).await?;
        // Like the real client: an asset's metadata carries the digest the
        // forge advertises for it, under `extra.forge`.
        if let Some(digest) = self.asset_digest.lock().unwrap().clone() {
            if pkg
                .artifact
                .as_deref()
                .is_some_and(|a| a.starts_with("filename/"))
            {
                meta.extra = serde_json::json!({
                    batlehub_core::entities::FORGE_EXTRA_KEY: {
                        batlehub_core::entities::FORGE_ASSET_DIGEST: digest,
                    }
                });
            }
        }
        Ok(meta)
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

/// The digests of what is already cached, seeded by the test.
///
/// The shipped in-memory app wires a no-op artifact-meta store, so nothing
/// records a digest there and `ASSET_REPLACED` would have nothing to compare
/// — the comparison, not the recording, is what this file is about.
#[derive(Default)]
struct SeededDigests(Mutex<HashMap<String, String>>);

#[async_trait]
impl batlehub_core::ports::ArtifactCacheMeta for SeededDigests {
    async fn record_artifact(
        &self,
        rec: batlehub_core::ports::ArtifactMetaRecord<'_>,
    ) -> Result<(), CoreError> {
        if let Some(c) = rec.checksum {
            self.0
                .lock()
                .unwrap()
                .insert(rec.key.to_owned(), c.to_owned());
        }
        Ok(())
    }
    async fn get_artifact_checksum(&self, key: &str) -> Result<Option<String>, CoreError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn touch_artifact(&self, _key: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn is_artifact_expired(
        &self,
        _key: &str,
        _older_than: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete_artifact_meta(&self, _key: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

struct Lab {
    forge: Arc<MovingForge>,
    verdicts: Arc<InMemoryVerdictRepository>,
    digests: Arc<SeededDigests>,
}

/// The app: a forge registry whose refs policy is `actions`, with a
/// `[security]` profile when `secure` — where the ref findings ride the
/// request's verdict — and without one otherwise.
async fn lab(actions: (RefAction, RefAction), secure: bool) -> (impl TestService, Lab) {
    let parts = local_registry_app_parts(REG, "github", RegistryMode::Proxy, None);
    let forge = MovingForge::new();
    let verdicts = InMemoryVerdictRepository::new();
    let queue = InMemoryScanQueue::new();
    let digests = Arc::new(SeededDigests::default());
    // Tags are re-asked on every request, which is what lets one test move a
    // tag between two calls; the RFC's own default is an hour.
    let policy = ForgeRefsPolicy {
        branch_ttl: Duration::ZERO,
        tag_ttl: Duration::ZERO,
        mutable_refs: actions.0,
        tag_moved: actions.1,
        frozen: false,
    };
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::clone(&forge) as Arc<dyn RegistryClient>,
        );
        hot.ref_resolutions = Some(InMemoryRefResolutionRepository::new());
        hot.forge_refs.insert(REG.to_owned(), policy);

        let mut rule = ForgeRefRule::new(policy).with_artifact_meta(
            Arc::clone(&digests) as Arc<dyn batlehub_core::ports::ArtifactCacheMeta>
        );
        let mut rules: Vec<Box<dyn batlehub_core::rules::Rule>> = Vec::new();
        if secure {
            let sec = SecurityPolicy {
                mode: SecurityMode::Block,
                min_age: Duration::ZERO,
                ..SecurityPolicy::defaults_for(REG)
            };
            let svc = Arc::new(VerdictService::new(
                Arc::clone(&verdicts) as Arc<dyn VerdictRepository>,
                Arc::clone(&queue) as Arc<dyn ScanQueue>,
            ));
            rules.push(Box::new(VerdictGateRule::new(svc, sec.clone(), None)));
            hot.security.insert(REG.to_owned(), sec);
            hot.verdicts = Some(Arc::clone(&verdicts) as Arc<dyn VerdictRepository>);
            hot.scan_queue = Some(Arc::clone(&queue) as Arc<dyn ScanQueue>);
            rule = rule.carrying_verdict();
        } else {
            rules.push(Box::new(BlockListRule::new(Arc::clone(
                &parts.proxy_svc.repo,
            ))));
        }
        rules.push(Box::new(rule));
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                // Never fresh: a test that moves a tag or a digest between
                // two requests must see the second answer, not the first
                // one cached.
                metadata_ttl: Some(Duration::ZERO),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules,
            }),
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (
        app,
        Lab {
            forge,
            verdicts,
            digests,
        },
    )
}

fn header(resp: &actix_web::dev::ServiceResponse, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_owned())
}

async fn get<S: TestService>(app: &S, uri: &str) -> actix_web::dev::ServiceResponse {
    call_service(app, admin_get(uri)).await
}

fn tarball(git_ref: &str) -> String {
    format!("/proxy/{REG}/{REPO}/tarball/{git_ref}")
}

// ── MUTABLE_REF ──────────────────────────────────────────────────────────────

/// The default: a branch is served, and the response says which commit
/// answered — no verdict on a registry that opted into none.
#[actix_web::test]
async fn a_branch_is_served_and_named_without_a_security_profile() {
    let (app, _) = lab((RefAction::Warn, RefAction::Deny), false).await;
    let resp = get(&app, &tarball("main")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header(&resp, "X-BatleHub-Ref-Kind").as_deref(),
        Some("branch")
    );
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(A)
    );
    assert!(
        header(&resp, "X-BatleHub-Verdict").is_none(),
        "a registry with no [security] invents no verdict"
    );
}

/// With `mutable_refs = "deny"` the same request is refused, and the body
/// names the code so a client that reads only the body still learns why.
#[actix_web::test]
async fn a_branch_is_refused_when_the_registry_must_be_reproducible() {
    let (app, _) = lab((RefAction::Deny, RefAction::Deny), false).await;
    let resp = get(&app, &tarball("main")).await;
    assert_eq!(resp.status(), 403);
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("MUTABLE_REF"), "{body}");
    assert!(body.contains("main"), "{body}");
}

/// With `[security]`, a warned ref reaches the client as a verdict: the same
/// headers RFC 0018 uses for everything else it wants a client to log.
#[actix_web::test]
async fn a_branch_is_warned_on_the_wire_under_a_security_profile() {
    let (app, lab) = lab((RefAction::Warn, RefAction::Deny), true).await;
    let resp = get(&app, &tarball("main")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header(&resp, "X-BatleHub-Verdict").as_deref(),
        Some("warned")
    );
    let reason = header(&resp, "X-BatleHub-Reason").unwrap_or_default();
    assert!(reason.contains("MUTABLE_REF"), "{reason}");

    // And it is not written to the store: the ref is a fact about the
    // request, and the same commit reached by a tag is not mutable.
    let stored = lab
        .verdicts
        .get(&PackageId::new(REG, REPO, A))
        .await
        .unwrap();
    assert!(
        stored.is_none_or(|v| !v
            .reason_codes
            .contains(&batlehub_core::entities::ReasonCode::MutableRef)),
        "MUTABLE_REF must not be stored against the commit"
    );
}

// ── TAG_MOVED ────────────────────────────────────────────────────────────────

/// A tag is trusted the first time it is seen and refused once it points
/// somewhere else — the detection the resolution table exists for.
#[actix_web::test]
async fn a_tag_that_moves_is_refused_by_default_and_names_both_commits() {
    let (app, lab) = lab((RefAction::Warn, RefAction::Deny), false).await;
    let resp = get(&app, &tarball("v1.0.0")).await;
    assert_eq!(resp.status(), 200, "first sight is trusted");

    lab.forge.move_tag("v1.0.0", B);
    let resp = get(&app, &tarball("v1.0.0")).await;
    assert_eq!(resp.status(), 403);
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("TAG_MOVED"), "{body}");
    assert!(body.contains(A) && body.contains(B), "{body}");
}

/// `tag_moved = "warn"`: served, and the wire says what happened — the
/// previous commit in a header on a plain registry.
#[actix_web::test]
async fn a_moved_tag_can_be_warned_instead_and_the_header_names_the_old_commit() {
    let (app, lab) = lab((RefAction::Warn, RefAction::Warn), false).await;
    get(&app, &tarball("v1.0.0")).await;
    lab.forge.move_tag("v1.0.0", B);
    let resp = get(&app, &tarball("v1.0.0")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(B)
    );
    assert_eq!(
        header(&resp, "X-BatleHub-Ref-Previous-Commit").as_deref(),
        Some(A)
    );
}

/// Under `[security]` a moved tag is a verdict denial, which is what makes
/// `batlehub why` able to answer it.
#[actix_web::test]
async fn a_moved_tag_denies_through_the_verdict_when_one_exists() {
    let (app, lab) = lab((RefAction::Warn, RefAction::Deny), true).await;
    assert_eq!(get(&app, &tarball("v1.0.0")).await.status(), 200);
    lab.forge.move_tag("v1.0.0", B);
    let resp = get(&app, &tarball("v1.0.0")).await;
    assert_eq!(resp.status(), 403);
    let reason = header(&resp, "X-BatleHub-Reason").unwrap_or_default();
    assert!(reason.contains("TAG_MOVED"), "{reason}");
    assert_eq!(
        header(&resp, "X-BatleHub-Verdict").as_deref(),
        Some("denied")
    );
}

// ── ASSET_REPLACED ───────────────────────────────────────────────────────────

/// The bytes under a release asset changed: the digest the forge advertises
/// no longer matches what is cached under the coordinate.
#[actix_web::test]
async fn an_asset_whose_digest_changed_is_refused() {
    let (app, lab) = lab((RefAction::Warn, RefAction::Deny), false).await;
    let uri = format!("/proxy/{REG}/{REPO}/releases/download/v1.0.0/gh.tar.gz");

    // Nothing cached yet: a first sight has nothing to compare and is
    // trusted, whatever the forge advertises.
    *lab.forge.asset_digest.lock().unwrap() = Some(format!("sha256:{}", "e".repeat(64)));
    assert_eq!(get(&app, &uri).await.status(), 200);

    // What the proxy holds under the coordinate, as the streaming store
    // records it: bare hex, no prefix.
    lab.digests.0.lock().unwrap().insert(
        format!("artifact:{REG}/{REPO}/v1.0.0/filename/gh.tar.gz"),
        "e".repeat(64),
    );
    // The same digest is not a replacement.
    assert_eq!(get(&app, &uri).await.status(), 200);

    // A different one is.
    *lab.forge.asset_digest.lock().unwrap() = Some(format!("sha256:{}", "f".repeat(64)));
    let resp = get(&app, &uri).await;
    assert_eq!(resp.status(), 403);
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("ASSET_REPLACED"), "{body}");
}

/// An archive is *not* judged this way: a forge regenerates it on demand and
/// the bytes are not stable, so a difference says nothing about the source
/// (§4.2 *Identity of the bytes*).
#[actix_web::test]
async fn an_archive_digest_is_never_asset_replaced() {
    let (app, lab) = lab((RefAction::Warn, RefAction::Deny), false).await;
    lab.digests.0.lock().unwrap().insert(
        format!("artifact:{REG}/{REPO}/{A}/tarball/{A}"),
        "e".repeat(64),
    );
    *lab.forge.asset_digest.lock().unwrap() = Some(format!("sha256:{}", "f".repeat(64)));
    assert_eq!(get(&app, &tarball("v1.0.0")).await.status(), 200);
}
