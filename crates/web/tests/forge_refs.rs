//! RFC 0019 phase 1 on the wire: a forge ref is resolved to a commit before
//! the fetch, archives and raw files are cached by that commit, every forge
//! response says which kind of ref answered and which commit, and a branch
//! head is dated by its commit for the age gate.
//!
//! The forge is a fake with a fixed set of tags and branches that the test can
//! move, counting its calls. `FixedRegistry` answers the bytes — with its own
//! coordinate as the body, which is what lets a test see that the client was
//! handed the rewritten, SHA-keyed coordinate rather than the ref.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::test::{call_service, TestRequest};
use async_trait::async_trait;
use batlehub_adapters::in_memory::InMemoryRefResolutionRepository;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{ForgeRefsPolicy, PackageId, PackageMetadata, RefKind},
    error::CoreError,
    ports::{
        DocumentKind, FetchedArtifact, ForgeCommit, ForgeRegistry, RegistryClient, ResolvedTarget,
        VersionDocument,
    },
    rules::{BlockListRule, ReleaseAgeGateRule},
    services::{proxy::proxy_artifact_key, RegistryPolicy},
};
use chrono::{DateTime, Utc};

const REG: &str = "gh";
const REPO: &str = "cli/cli";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A forge with fixed tags and movable branches, over `FixedRegistry`'s bytes.
struct FakeForge {
    inner: Arc<FixedRegistry>,
    tags: HashMap<&'static str, (&'static str, Option<DateTime<Utc>>)>,
    branches: Mutex<HashMap<&'static str, (String, DateTime<Utc>)>>,
    calls: AtomicUsize,
}

impl FakeForge {
    fn new() -> Arc<Self> {
        let old = Utc::now() - chrono::Duration::days(400);
        Arc::new(Self {
            inner: FixedRegistry::new("github"),
            tags: HashMap::from([("v1.0.0", (A, Some(old)))]),
            branches: Mutex::new(HashMap::from([("main", (A.to_owned(), old))])),
            calls: AtomicUsize::new(0),
        })
    }

    fn move_branch(&self, name: &'static str, sha: &str, at: DateTime<Utc>) {
        self.branches
            .lock()
            .unwrap()
            .insert(name, (sha.to_owned(), at));
    }
}

#[async_trait]
impl ForgeRegistry for FakeForge {
    async fn resolve_ref(&self, _: &str, git_ref: &str) -> Result<ResolvedTarget, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((sha, date)) = self.tags.get(git_ref) {
            return Ok(ResolvedTarget {
                kind: RefKind::Tag,
                sha: (*sha).to_owned(),
                object_date: *date,
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
impl RegistryClient for FakeForge {
    fn registry_type(&self) -> &str {
        "github"
    }
    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        Some(self)
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        // Like the real client for an archive: no date of its own, so the
        // resolution's object date is what the gate sees.
        let mut meta = self.inner.resolve_metadata(pkg).await?;
        if pkg
            .artifact
            .as_deref()
            .is_some_and(|a| a.starts_with("tarball/") || a.starts_with("raw/"))
        {
            meta.published_at = None;
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

/// The app, with `min_age` on the registry's chain when given; the forge, to
/// move a branch and count calls; the storage, to see what was cached.
async fn fixture(
    policy: ForgeRefsPolicy,
    min_age: Option<Duration>,
) -> (
    impl TestService,
    Arc<FakeForge>,
    Arc<dyn batlehub_core::ports::StorageBackend>,
) {
    let parts = local_registry_app_parts(REG, "github", RegistryMode::Proxy, None);
    let forge = FakeForge::new();
    let storage = Arc::clone(&parts.proxy_svc.storage);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::clone(&forge) as Arc<dyn RegistryClient>,
        );
        hot.ref_resolutions = Some(InMemoryRefResolutionRepository::new());
        hot.forge_refs.insert(REG.to_owned(), policy);
        if let Some(min_age) = min_age {
            hot.policies.insert(
                REG.to_owned(),
                Arc::new(RegistryPolicy {
                    metadata_ttl: Some(Duration::from_secs(300)),
                    firewall_only: false,
                    serve_stale_metadata: false,
                    artifact_ttl: None,
                    rules: vec![
                        Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo))),
                        Box::new(ReleaseAgeGateRule::new(min_age, vec![])),
                    ],
                }),
            );
        }
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, forge, storage)
}

fn header(resp: &actix_web::dev::ServiceResponse, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_owned())
}

async fn get<S: TestService>(app: &S, uri: &str) -> actix_web::dev::ServiceResponse {
    call_service(app, admin_get(uri)).await
}

// ── the two headers, per ref kind ────────────────────────────────────────────

#[actix_web::test]
async fn a_branch_archive_says_branch_and_names_the_commit() {
    let (app, _forge, _storage) = fixture(Default::default(), None).await;
    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/tarball/main")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header(&resp, "X-BatleHub-Ref-Kind").as_deref(),
        Some("branch")
    );
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(A)
    );
    // The bytes were fetched under the rewritten, commit-keyed coordinate.
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(
        body,
        format!("artifact:github:{REG}/{REPO}/{A}/tarball/{A}").as_bytes()
    );
}

#[actix_web::test]
async fn a_tag_archive_and_a_raw_file_say_tag() {
    let (app, _forge, _storage) = fixture(Default::default(), None).await;
    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/zipball/v1.0.0")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Ref-Kind").as_deref(), Some("tag"));

    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/raw/v1.0.0/install.sh")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Ref-Kind").as_deref(), Some("tag"));
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(A)
    );
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(
        body,
        format!("artifact:github:{REG}/{REPO}/{A}/raw/install.sh").as_bytes()
    );
}

/// A full SHA is a commit: resolved without asking the forge at all.
#[actix_web::test]
async fn a_commit_sha_resolves_without_a_forge_call() {
    let (app, forge, _storage) = fixture(Default::default(), None).await;
    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/tarball/{B}")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header(&resp, "X-BatleHub-Ref-Kind").as_deref(),
        Some("commit")
    );
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(B)
    );
    assert_eq!(forge.calls.load(Ordering::SeqCst), 0);
}

/// Release assets are resolved (so a moved tag is recorded) but keep the tag
/// as their key: the uploaded bytes, not the commit, are the identity.
#[actix_web::test]
async fn a_release_asset_is_resolved_but_keeps_its_tag_key() {
    let (app, forge, _storage) = fixture(Default::default(), None).await;
    let resp = get(
        &app,
        &format!("/proxy/{REG}/{REPO}/releases/download/v1.0.0/gh.tar.gz"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Ref-Kind").as_deref(), Some("tag"));
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(A)
    );
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(
        body,
        format!("artifact:github:{REG}/{REPO}/v1.0.0/filename/gh.tar.gz").as_bytes()
    );
    assert_eq!(forge.calls.load(Ordering::SeqCst), 1);
}

#[actix_web::test]
async fn an_unknown_ref_is_not_found() {
    let (app, _forge, _storage) = fixture(Default::default(), None).await;
    let resp = get(&app, &format!("/proxy/{REG}/{REPO}/tarball/nope")).await;
    assert_eq!(resp.status(), 404);
}

/// A package registry is untouched by any of this: no headers, no resolver.
#[actix_web::test]
async fn a_package_registry_gets_no_forge_headers() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    let resp = get(&app, "/proxy/npm-reg/lodash/1.1.0/tarball").await;
    assert_eq!(resp.status(), 200);
    assert!(header(&resp, "X-BatleHub-Ref-Kind").is_none());
}

// ── the cache is keyed by the commit ─────────────────────────────────────────

/// `main` today and `main` tomorrow are two entries, keyed by what they
/// resolved to — and a resolution within its TTL costs no forge call.
#[actix_web::test]
async fn archives_are_cached_by_commit_and_a_moved_branch_is_a_new_entry() {
    let (app, forge, storage) = fixture(
        ForgeRefsPolicy {
            branch_ttl: Duration::ZERO,
            tag_ttl: Duration::from_secs(3600),
        },
        None,
    )
    .await;
    let url = format!("/proxy/{REG}/{REPO}/tarball/main");
    let key_a =
        proxy_artifact_key(&PackageId::new(REG, REPO, A).with_artifact(format!("tarball/{A}")));
    let key_b =
        proxy_artifact_key(&PackageId::new(REG, REPO, B).with_artifact(format!("tarball/{B}")));

    assert_eq!(get(&app, &url).await.status(), 200);
    assert!(
        storage.exists(&key_a).await.unwrap(),
        "cached under {key_a}"
    );
    assert!(!storage.exists(&key_b).await.unwrap());

    forge.move_branch("main", B, Utc::now() - chrono::Duration::days(1));
    let resp = get(&app, &url).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header(&resp, "X-BatleHub-Resolved-Commit").as_deref(),
        Some(B)
    );
    assert!(
        storage.exists(&key_b).await.unwrap(),
        "the moved branch is a second entry"
    );
    assert!(
        storage.exists(&key_a).await.unwrap(),
        "and the old one is untouched"
    );
}

#[actix_web::test]
async fn a_resolution_within_its_ttl_costs_no_forge_call() {
    let (app, forge, _storage) = fixture(Default::default(), None).await;
    let url = format!("/proxy/{REG}/{REPO}/tarball/main");
    get(&app, &url).await;
    get(&app, &url).await;
    get(&app, &format!("/proxy/{REG}/{REPO}/raw/main/README.md")).await;
    assert_eq!(
        forge.calls.load(Ordering::SeqCst),
        1,
        "three reads of `main`, one resolution"
    );
}

// ── the metadata contract: a branch head is dated by its commit ──────────────

/// RFC 0019 §4.2: a branch coordinate is never held for being mutable, only
/// for a commit younger than the floor. Yesterday's head is served; a commit
/// pushed a minute ago is held, with the same `403` any young version gets.
#[actix_web::test]
async fn a_fresh_commit_on_a_branch_is_held_by_min_age_and_an_old_one_is_served() {
    let (app, forge, _storage) = fixture(
        ForgeRefsPolicy {
            branch_ttl: Duration::ZERO,
            tag_ttl: Duration::from_secs(3600),
        },
        Some(Duration::from_secs(3600)),
    )
    .await;
    let url = format!("/proxy/{REG}/{REPO}/tarball/main");
    assert_eq!(
        get(&app, &url).await.status(),
        200,
        "a year-old head passes"
    );

    forge.move_branch("main", B, Utc::now() - chrono::Duration::seconds(60));
    let resp = get(&app, &url).await;
    assert_eq!(
        resp.status(),
        403,
        "a minute-old head is held for the floor"
    );
}

/// The tag route and the age gate agree on the tagger date.
#[actix_web::test]
async fn a_tag_archive_is_dated_by_its_tag() {
    let (app, _forge, _storage) =
        fixture(Default::default(), Some(Duration::from_secs(3600))).await;
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/{REPO}/tarball/v1.0.0"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}
