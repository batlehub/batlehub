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

use crate::entities::{is_commit_sha, ForgeRefsPolicy, RefKind, ResolvedRef};
use crate::error::CoreError;
use crate::ports::{ForgeRegistry, RefResolutionRepository, StoredRefResolution};

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
        let fresh = policy.ttl_for(prev.kind).is_some_and(|ttl| {
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

    #[tokio::test]
    async fn an_unknown_ref_is_not_found() {
        let f = forge();
        let err = resolve_ref("gh", &f, None, Default::default(), "o/r", "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }
}
