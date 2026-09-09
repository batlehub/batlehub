//! Ref resolution (RFC 0019 §5.2): a ref becomes a commit before anything is
//! fetched, and the answer is remembered.
//!
//! ```text
//! resolve(reg, cli/cli, main)
//!   ├─ full SHA?            → Commit, no call, nothing stored
//!   ├─ stored & within TTL? → the stored commit
//!   └─ ask the forge        → tag, then branch, then 404
//!        └─ upsert, keeping `previous` when the commit changed
//! ```
//!
//! The table is what turns a moved tag into something *detectable*: the
//! first observation is trusted and any later change is recorded as
//! `previous`. Phase 2's `TAG_MOVED` reads that field; this phase writes it.
//!
//! The budget is consulted by the forge client, not here: it is the client
//! that sees `X-RateLimit-*` come back, so it is the client that observes.

use std::sync::Arc;

use chrono::Utc;

use crate::entities::{
    is_commit_sha, ForgeCoordinate, ForgeKind, ForgeRefsPolicy, PackageMetadata, ReasonCode,
    RefAction, RefKind, ResolvedRef, Severity, FORGE_ASSET_DIGEST, FORGE_EXTRA_KEY,
    FORGE_PREVIOUS_COMMIT, FORGE_REF_KIND, FORGE_RESOLVED_COMMIT,
};
use crate::error::CoreError;
use crate::ports::{ForgeRegistry, RefResolutionRepository, StoredRefResolution};

/// One thing the ref says about this request, and what the operator chose to
/// do about it (RFC 0019 §4.2, phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefFinding {
    pub code: ReasonCode,
    pub action: RefAction,
    /// The sentence a refusal shows and a finding records. Names the code so
    /// a client reading only the body still learns which fact refused it.
    pub message: String,
}

impl RefFinding {
    pub fn denies(&self) -> bool {
        self.action == RefAction::Deny
    }

    /// The severity the verdict judges it at: the action, encoded.
    pub fn severity(&self) -> Severity {
        self.action.severity()
    }
}

/// Read `extra.forge` back and say what the ref makes of this request.
///
/// `cached_digest` is the digest of the bytes already stored under this
/// coordinate, bare hex; `None` means nothing is stored yet — a first sight,
/// which is trusted. Both the rule (no `[security]`) and the scanner (with
/// one) call this, so the two paths cannot disagree about what a moved tag
/// is.
pub fn ref_findings(
    policy: ForgeRefsPolicy,
    metadata: &PackageMetadata,
    cached_digest: Option<&str>,
) -> Vec<RefFinding> {
    let Some(coord) = ForgeCoordinate::from_package_id(&metadata.id) else {
        return Vec::new();
    };
    let forge = match metadata.extra.get(FORGE_EXTRA_KEY) {
        Some(serde_json::Value::Object(m)) => m,
        // No resolution rode along: a listing, or a client with no `forge()`.
        // Nothing is asserted about a ref nobody resolved.
        _ => return Vec::new(),
    };
    let str_at = |key: &str| forge.get(key).and_then(|v| v.as_str());
    let kind: Option<RefKind> = str_at(FORGE_REF_KIND).and_then(|k| k.parse().ok());
    let resolved = str_at(FORGE_RESOLVED_COMMIT).unwrap_or("");
    let previous = str_at(FORGE_PREVIOUS_COMMIT);
    // What the *client* asked for, not what the coordinate now says: by the
    // time the rules run, an archive's `PackageId` has been rewritten onto
    // the commit, so `git_ref()` would answer a SHA and the refusal would
    // name something the user never typed.
    let asked = str_at(crate::entities::FORGE_REQUESTED_REF)
        .or_else(|| coord.git_ref())
        .unwrap_or("?");
    let mut out = Vec::new();

    if kind == Some(RefKind::Branch) {
        out.push(RefFinding {
            code: ReasonCode::MutableRef,
            action: policy.mutable_refs,
            message: format!(
                "MUTABLE_REF: '{asked}' is a branch, served at commit {resolved}; what it \
                 names can change under the same coordinate"
            ),
        });
    }

    // A tag that resolves elsewhere than it did. A *branch* that advanced is
    // the same field and is not this finding — it is what a branch does, and
    // `MUTABLE_REF` above already says so.
    if kind == Some(RefKind::Tag) {
        if let Some(prev) = previous.filter(|p| !p.is_empty() && *p != resolved) {
            out.push(RefFinding {
                code: ReasonCode::TagMoved,
                action: policy.tag_moved,
                message: format!(
                    "TAG_MOVED: tag '{asked}' now resolves to {resolved}, previously {prev}"
                ),
            });
        }
    }

    // The forge's own digest for a release asset against the bytes already
    // cached under the coordinate. Only for assets: an archive is generated
    // on demand and is not byte-stable (§4.2 *Identity of the bytes*), so a
    // difference there says nothing about the source.
    if matches!(coord.kind, ForgeKind::Asset { .. }) {
        if let (Some(upstream), Some(cached)) = (str_at(FORGE_ASSET_DIGEST), cached_digest) {
            let upstream_hex = upstream
                .rsplit_once(':')
                .map(|(_, h)| h)
                .unwrap_or(upstream)
                .to_ascii_lowercase();
            if !upstream_hex.is_empty() && upstream_hex != cached.to_ascii_lowercase() {
                out.push(RefFinding {
                    code: ReasonCode::AssetReplaced,
                    action: policy.tag_moved,
                    message: format!(
                        "ASSET_REPLACED: the asset now has digest {upstream_hex}, the bytes \
                         cached under this coordinate hash to {cached}"
                    ),
                });
            }
        }
    }
    out
}

/// Resolve `git_ref` in `owner_repo` on `registry`.
///
/// `store` is optional so a deployment with no database — and a test that
/// wired none — still resolves; it just does not remember, and so cannot
/// detect a move.
pub async fn resolve_ref(
    registry: &str,
    forge: &dyn ForgeRegistry,
    store: Option<&Arc<dyn RefResolutionRepository>>,
    policy: ForgeRefsPolicy,
    owner_repo: &str,
    git_ref: &str,
) -> Result<ResolvedRef, CoreError> {
    if is_commit_sha(git_ref) {
        return Ok(ResolvedRef {
            requested: git_ref.to_owned(),
            kind: RefKind::Commit,
            sha: git_ref.to_ascii_lowercase(),
            resolved_at: Utc::now(),
            previous: None,
            object_date: None,
            publisher: None,
        });
    }

    let stored = match store {
        Some(s) => s
            .get(registry, owner_repo, git_ref)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    registry,
                    owner_repo,
                    git_ref,
                    error = %e,
                    "could not read the ref resolution table; asking the forge"
                );
                None
            }),
        None => None,
    };

    if let Some(prev) = &stored {
        // `frozen` short-circuits the TTL on an air-gapped instance: there is
        // no upstream to re-ask, so an expired row would fall through to a
        // refusal and take every cached artifact of that ref with it.
        let fresh = policy.frozen
            || policy.ttl_for(prev.kind).is_some_and(|ttl| {
                Utc::now() - prev.resolved_at < chrono::Duration::from_std(ttl).unwrap_or_default()
            });
        if fresh {
            return Ok(ResolvedRef {
                requested: git_ref.to_owned(),
                kind: prev.kind,
                sha: prev.sha.clone(),
                resolved_at: prev.resolved_at,
                previous: prev.previous.clone(),
                object_date: None,
                publisher: None,
            });
        }
    }

    let target = forge.resolve_ref(owner_repo, git_ref).await?;
    let now = Utc::now();
    // A change since the last resolution is remembered; an unchanged answer
    // keeps whatever `previous` was already recorded, so a move is not
    // forgotten by the next unchanged re-resolution within the same window.
    let previous = match &stored {
        Some(prev) if prev.sha != target.sha => Some(prev.sha.clone()),
        Some(prev) => prev.previous.clone(),
        None => None,
    };
    let resolution = StoredRefResolution {
        kind: target.kind,
        sha: target.sha.clone(),
        resolved_at: now,
        previous: previous.clone(),
    };
    if let Some(s) = store {
        if let Err(e) = s.upsert(registry, owner_repo, git_ref, &resolution).await {
            tracing::warn!(
                registry,
                owner_repo,
                git_ref,
                error = %e,
                "could not record the ref resolution; a move will not be detectable until it is"
            );
        }
    }

    Ok(ResolvedRef {
        requested: git_ref.to_owned(),
        kind: target.kind,
        sha: target.sha,
        resolved_at: now,
        previous,
        object_date: target.object_date,
        publisher: target.publisher,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ForgeCommit, ResolvedTarget};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A forge with a fixed set of tags and branches, counting its calls.
    struct FakeForge {
        tags: HashMap<&'static str, &'static str>,
        branches: Mutex<HashMap<&'static str, String>>,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl ForgeRegistry for FakeForge {
        async fn resolve_ref(&self, _: &str, git_ref: &str) -> Result<ResolvedTarget, CoreError> {
            *self.calls.lock().unwrap() += 1;
            if let Some(sha) = self.tags.get(git_ref) {
                return Ok(ResolvedTarget {
                    kind: RefKind::Tag,
                    sha: (*sha).to_owned(),
                    object_date: None,
                    publisher: Some("tagger".into()),
                });
            }
            if let Some(sha) = self.branches.lock().unwrap().get(git_ref) {
                return Ok(ResolvedTarget {
                    kind: RefKind::Branch,
                    sha: sha.clone(),
                    object_date: Some(Utc::now()),
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

    #[derive(Default)]
    struct MemStore(Mutex<HashMap<String, StoredRefResolution>>);

    #[async_trait]
    impl RefResolutionRepository for MemStore {
        async fn get(
            &self,
            registry: &str,
            owner_repo: &str,
            git_ref: &str,
        ) -> Result<Option<StoredRefResolution>, CoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&format!("{registry}/{owner_repo}/{git_ref}"))
                .cloned())
        }
        async fn upsert(
            &self,
            registry: &str,
            owner_repo: &str,
            git_ref: &str,
            resolution: &StoredRefResolution,
        ) -> Result<(), CoreError> {
            self.0.lock().unwrap().insert(
                format!("{registry}/{owner_repo}/{git_ref}"),
                resolution.clone(),
            );
            Ok(())
        }
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn forge() -> FakeForge {
        FakeForge {
            // `v1` is both a tag and a branch: the tag must win.
            tags: HashMap::from([("v1", A)]),
            branches: Mutex::new(HashMap::from([
                ("main", A.to_owned()),
                ("v1", B.to_owned()),
            ])),
            calls: Mutex::new(0),
        }
    }

    fn store() -> Arc<dyn RefResolutionRepository> {
        Arc::new(MemStore::default())
    }

    #[tokio::test]
    async fn a_full_sha_resolves_without_a_call_or_a_row() {
        let f = forge();
        let s = store();
        let r = resolve_ref("gh", &f, Some(&s), Default::default(), "o/r", A)
            .await
            .unwrap();
        assert_eq!(r.kind, RefKind::Commit);
        assert_eq!(r.sha, A);
        assert_eq!(*f.calls.lock().unwrap(), 0);
        assert!(s.get("gh", "o/r", A).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_ref_that_is_both_a_tag_and_a_branch_resolves_as_the_tag() {
        let f = forge();
        let r = resolve_ref("gh", &f, None, Default::default(), "o/r", "v1")
            .await
            .unwrap();
        assert_eq!(r.kind, RefKind::Tag);
        assert_eq!(r.sha, A);
        assert_eq!(r.publisher.as_deref(), Some("tagger"));
    }

    #[tokio::test]
    async fn a_fresh_resolution_is_served_from_the_table_without_a_call() {
        let f = forge();
        let s = store();
        resolve_ref("gh", &f, Some(&s), Default::default(), "o/r", "main")
            .await
            .unwrap();
        let again = resolve_ref("gh", &f, Some(&s), Default::default(), "o/r", "main")
            .await
            .unwrap();
        assert_eq!(again.kind, RefKind::Branch);
        assert_eq!(again.sha, A);
        assert_eq!(
            *f.calls.lock().unwrap(),
            1,
            "the second lookup hit the table"
        );
    }

    #[tokio::test]
    async fn an_expired_resolution_is_re_asked_and_a_move_is_recorded() {
        let f = forge();
        let s = store();
        let expire_at_once = ForgeRefsPolicy {
            branch_ttl: Duration::ZERO,
            tag_ttl: Duration::ZERO,
            ..ForgeRefsPolicy::default()
        };
        let first = resolve_ref("gh", &f, Some(&s), expire_at_once, "o/r", "main")
            .await
            .unwrap();
        assert!(!first.moved());

        f.branches.lock().unwrap().insert("main", B.to_owned());
        let second = resolve_ref("gh", &f, Some(&s), expire_at_once, "o/r", "main")
            .await
            .unwrap();
        assert_eq!(second.sha, B);
        assert_eq!(second.previous.as_deref(), Some(A));
        assert!(second.moved());

        // Unchanged afterwards: the move is still on record.
        let third = resolve_ref("gh", &f, Some(&s), expire_at_once, "o/r", "main")
            .await
            .unwrap();
        assert_eq!(third.previous.as_deref(), Some(A));
        assert_eq!(*f.calls.lock().unwrap(), 3);
    }

    /// RFC 0008 §13.3. A TTL is a promise to go and ask again, and an
    /// air-gapped instance has nobody to ask. Frozen, an expired row is
    /// simply what this instance knows — and it has to be, because a ref is
    /// resolved *before* anything is fetched: falling through to the forge
    /// would refuse every cached artifact of that ref along with it.
    #[tokio::test]
    async fn a_frozen_policy_never_re_asks_however_old_the_row_is() {
        let f = forge();
        let s = store();
        let expired = ForgeRefsPolicy {
            branch_ttl: Duration::ZERO,
            tag_ttl: Duration::ZERO,
            ..ForgeRefsPolicy::default()
        };
        // One resolution, made while there was still an upstream.
        let first = resolve_ref("gh", &f, Some(&s), expired, "o/r", "main")
            .await
            .unwrap();
        assert_eq!(first.sha, A);
        assert_eq!(*f.calls.lock().unwrap(), 1);

        let frozen = ForgeRefsPolicy {
            frozen: true,
            ..expired
        };
        // The branch moves upstream. The frozen instance neither knows nor
        // asks: it answers what it holds.
        f.branches.lock().unwrap().insert("main", B.to_owned());
        for _ in 0..3 {
            let again = resolve_ref("gh", &f, Some(&s), frozen, "o/r", "main")
                .await
                .unwrap();
            assert_eq!(again.sha, A, "a frozen resolution is the one it holds");
        }
        assert_eq!(
            *f.calls.lock().unwrap(),
            1,
            "not one of the three reached the forge"
        );

        // And a ref it has never resolved is still a refusal rather than an
        // invention: frozen means "do not re-ask", not "answer anyway".
        assert!(resolve_ref("gh", &f, Some(&s), frozen, "o/r", "never-seen")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_unknown_ref_is_not_found() {
        let f = forge();
        let err = resolve_ref("gh", &f, None, Default::default(), "o/r", "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }
}

#[cfg(test)]
mod ref_finding_tests {
    use super::*;
    use crate::entities::{PackageId, RefAction};

    fn meta(artifact: &str, forge: serde_json::Value) -> PackageMetadata {
        let mut m = PackageMetadata::minimal(
            PackageId::new("gh", "cli/cli", "v1.0.0").with_artifact(artifact),
            serde_json::Value::Null,
        );
        m.extra = serde_json::json!({ FORGE_EXTRA_KEY: forge });
        m
    }

    #[test]
    fn an_asset_digest_that_differs_from_the_cached_bytes_is_a_finding() {
        let p = ForgeRefsPolicy::default();
        let m = meta(
            "filename/gh.tar.gz",
            serde_json::json!({
                "ref_kind": "tag",
                "resolved_commit": "a".repeat(40),
                "asset_digest": format!("sha256:{}", "f".repeat(64)),
            }),
        );
        let found = ref_findings(p, &m, Some(&"e".repeat(64)));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].code, ReasonCode::AssetReplaced);
        assert_eq!(found[0].action, RefAction::Deny);

        // The same digest, and a first sight, are both trusted.
        assert!(ref_findings(p, &m, Some(&"f".repeat(64))).is_empty());
        assert!(ref_findings(p, &m, None).is_empty());
    }
}
