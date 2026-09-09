use std::time::Duration;

use async_trait::async_trait;

use crate::{
    entities::{CompactionReport, PublishedPackage, Tombstone},
    error::CoreError,
};

/// Authoritative storage for packages published directly to BatleHub.
///
/// Each method is scoped to a `registry` name so one instance serves all
/// local registries. Index metadata is ecosystem-specific opaque JSON stored
/// inside `PublishedPackage::index_metadata`.
///
/// ## Transactional publish protocol
///
/// To survive a hard crash between the index write and the artifact write,
/// callers must use the three-step protocol:
///
/// 1. `publish(pkg)` — reserve the version; implementations may insert in a
///    *pending* state invisible to `get_versions`/`exists`.
/// 2. Write the artifact bytes to `StorageBackend`.
/// 3. `commit_publish(registry, name, version)` — promote the row to the
///    visible *published* state.
///
/// On any failure after step 1, call `remove_version` to clean up the pending
/// row.  Hard-crashed pending rows are recovered by `cleanup_pending`.
///
/// ## Deletion is a tombstone, not a removal
///
/// A version that was ever *published* is never removed from this store. Delete
/// is `tombstone_version`: the row survives with `deleted_at` set, the artifact
/// bytes are dropped by the caller, and `publish` refuses the coordinate for
/// good (RFC 0016 §4.4). `remove_version` remains a hard delete and exists only
/// for the rollback of a publish that never committed — a pending row was never
/// visible to anyone, so it spends no name.
#[async_trait]
pub trait LocalRegistryBackend: Send + Sync {
    /// Reserve a new version. Returns `CoreError::Conflict` if a *published*
    /// version already exists, **or if the coordinate is tombstoned** — a name
    /// that has been published once is spent, and no later publish may occupy it
    /// (RFC 0016 §4.4). Implementations may insert in a *pending* state that is
    /// invisible to `get_versions` and `exists` until `commit_publish` is called.
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError>;

    /// Promote a previously `publish`-ed row to the visible *published* state.
    /// Called after artifact storage succeeds. The default no-op is correct for
    /// backends that insert in published state directly (e.g. in-memory mocks).
    async fn commit_publish(
        &self,
        _registry: &str,
        _name: &str,
        _version: &str,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    /// Mark a version as yanked. Also updates `index_metadata.yanked`.
    async fn yank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError>;

    /// Reverse a yank. Also updates `index_metadata.yanked`.
    async fn unyank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError>;

    /// Flag a version as deprecated with an optional message. The version stays
    /// listed and downloadable. Also mirrors the message into
    /// `index_metadata.deprecated` (npm's native field).
    async fn deprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        message: Option<&str>,
    ) -> Result<(), CoreError>;

    /// Reverse a deprecation. Also removes `index_metadata.deprecated`.
    async fn undeprecate(&self, registry: &str, name: &str, version: &str)
        -> Result<(), CoreError>;

    /// Hide a version from registry-protocol listings. It stays downloadable by
    /// exact coordinate.
    async fn unlist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError>;

    /// Reverse an unlist, making the version visible in listings again.
    async fn relist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError>;

    /// Pin or unpin a version against retention (RFC 0016 §4.1).
    ///
    /// A pinned version is never reclaimed by a retention run, whatever the
    /// registry's policy says. It changes nothing else: the version resolves,
    /// downloads and lists exactly as it did.
    ///
    /// Returns `true` when a published version's pin changed. A no-op returns
    /// `false` rather than erroring, so setting a pin that is already set is
    /// safe to repeat.
    /// Move a published version to a release channel (RFC 0015 §4.2).
    ///
    /// JetBrains Marketplace serves plugin builds on a channel — the empty string
    /// is Stable, anything else (`eap`, `nightly`) is its own feed — and the
    /// channel is read from `index_metadata.channel`, set once at publish. This
    /// is what moves it afterwards.
    ///
    /// # A channel move is not a replacement, and `immutable` does not apply
    ///
    /// The question §4.5 raises is whether repointing a published version at
    /// another channel counts as replacing it. §13.6 already answered the general
    /// form: *"immutability is a question about **bytes**, not about a
    /// coordinate"* — which is why `PublishPolicyRequest::artifact_key` exists and
    /// why a Maven `.jar` is judged on its key rather than on its version row. A
    /// channel move changes no byte: the artifact, its checksum and its signature
    /// are untouched, and a client that already downloaded it is unaffected.
    ///
    /// So this is a narrow, purpose-named method rather than a general
    /// `set_index_metadata`. The generic one would be a way to mutate anything on
    /// a published version — including the checksum — behind a verb that names one
    /// field, which is the shape `immutable` exists to prevent.
    ///
    /// Returns `true` when a row changed, `false` when the version is absent or
    /// already on that channel.
    /// **Required, not defaulted.** A default returning `false` compiled fine and
    /// made every backend that had not implemented it into a silent no-op — which
    /// is how the first version of this shipped, and what the tests caught: the
    /// mock inherited the default and reported "nothing changed" for a move the
    /// caller had been authorized to make. A port method whose default is a lie
    /// about what happened is worse than a compile error.
    async fn set_channel(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        channel: &str,
    ) -> Result<bool, CoreError>;

    /// RFC 0020 §13.6: record that a version's signature archive was
    /// **provided** at publish time (an upstream's, kept as-is) rather than
    /// made by the registry's key — `index_metadata.vsixSignature =
    /// "provided"`, or cleared. `Ok(true)` when the row changed.
    ///
    /// Default: not supported, for the test backends that model none of
    /// this; both real backends override it.
    async fn set_vsix_signature_provided(
        &self,
        _registry: &str,
        _name: &str,
        _version: &str,
        _provided: bool,
    ) -> Result<bool, CoreError> {
        Err(CoreError::NotSupported(
            "this backend cannot record a provided VSIX signature".to_owned(),
        ))
    }

    async fn set_retention_keep(
        &self,
        _registry: &str,
        _name: &str,
        _version: &str,
        _keep: bool,
    ) -> Result<bool, CoreError> {
        Ok(false)
    }

    /// Return all versions of `name` in `registry`, sorted by `published_at` ASC.
    /// Returns an empty vec (not an error) when the crate has never been published.
    /// Must only return rows in the *published* state.
    async fn get_versions(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError>;

    /// Return `true` if at least one *published* version of `name` exists in `registry`.
    async fn exists(&self, registry: &str, name: &str) -> Result<bool, CoreError>;

    /// Hard-remove an exact version record from the index regardless of its state.
    ///
    /// **Rollback only.** This is how a publish that failed between `publish` and
    /// `commit_publish` discards its own pending row: that row was never visible
    /// to a reader, so removing it spends no coordinate. A user-facing delete of a
    /// *published* version goes through [`Self::tombstone_version`] instead, and
    /// calling this for one silently frees a name that RFC 0016 §4.4 says is
    /// permanently spent.
    ///
    /// Implementations that cannot support this operation should return `Ok(())`
    /// (best-effort).
    async fn remove_version(
        &self,
        _registry: &str,
        _name: &str,
        _version: &str,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    /// Soft-delete a *published* version: keep the row, set `deleted_at`.
    ///
    /// The coordinate is spent from here on — `publish` refuses it, listings stop
    /// returning it, and the row stays readable to the audit and ownership views.
    /// The caller drops the artifact bytes; this method owns only the row.
    ///
    /// Returns `true` when a published version was tombstoned, `false` when there
    /// was nothing to tombstone (no such version, or it is already a tombstone).
    /// Idempotent: re-deleting an existing tombstone is a `false`, not an error,
    /// and must not overwrite the original `deleted_at`.
    ///
    /// The default implementation refuses rather than silently hard-deleting: a
    /// backend that has not implemented tombstones must not be handed a delete it
    /// would satisfy by freeing the name.
    async fn tombstone_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        _deleted_by: Option<&str>,
    ) -> Result<bool, CoreError> {
        Err(CoreError::Config(format!(
            "this local-registry backend cannot tombstone {name}@{version} in \
             registry '{registry}'; refusing to delete rather than free the coordinate"
        )))
    }

    /// Return the tombstone for an exact coordinate, if the coordinate is spent.
    ///
    /// Consulted by the publish path on every publish. The default `None` is
    /// wrong for any backend that can tombstone, and correct only for one that
    /// cannot — which also cannot delete, so it never creates one.
    async fn find_tombstone(
        &self,
        _registry: &str,
        _name: &str,
        _version: &str,
    ) -> Result<Option<Tombstone>, CoreError> {
        Ok(None)
    }

    /// List tombstones in `registry`, newest deletion first, optionally narrowed
    /// to one package name. For the audit and ownership views, which are the
    /// callers RFC 0016 §4.4 says may still see a deleted version.
    async fn list_tombstones(
        &self,
        _registry: &str,
        _name: Option<&str>,
    ) -> Result<Vec<Tombstone>, CoreError> {
        Ok(vec![])
    }

    /// Strip the detail columns of tombstones in `registry` deleted more than
    /// `older_than` ago, keeping the coordinate claim (RFC 0016 §4.5).
    ///
    /// Only rows with `deleted_at` set are ever touched, and no row is ever
    /// removed — there is deliberately no method here that deletes a tombstone,
    /// because collecting one reopens the hole tombstones exist to close.
    ///
    /// `dry_run` reports what would be stripped and writes nothing.
    async fn compact_tombstone_detail(
        &self,
        _registry: &str,
        _older_than: Duration,
        dry_run: bool,
    ) -> Result<CompactionReport, CoreError> {
        Ok(CompactionReport {
            dry_run,
            ..Default::default()
        })
    }

    /// Delete *pending* rows that were created before `older_than` ago.
    /// These are left by hard crashes between `publish` and `commit_publish`.
    /// Returns the number of rows deleted. The default no-op is correct for
    /// backends that have no pending state.
    async fn cleanup_pending(&self, _older_than: Duration) -> Result<u64, CoreError> {
        Ok(0)
    }

    /// Return the distinct package names published in `registry`.
    /// Used to build registry index files (e.g. Composer `packages.json`).
    /// The default implementation returns an empty vec.
    async fn list_package_names(&self, _registry: &str) -> Result<Vec<String>, CoreError> {
        Ok(vec![])
    }

    /// Yank multiple versions in one call.
    /// The default implementation loops over `yank`. Override for efficiency.
    async fn bulk_yank(
        &self,
        registry: &str,
        items: &[(String, String)],
    ) -> Result<BulkResult, CoreError> {
        let mut result = BulkResult {
            processed: items.len(),
            succeeded: 0,
            failed: vec![],
        };
        for (name, version) in items {
            match self.yank(registry, name, version).await {
                Ok(()) => result.succeeded += 1,
                Err(e) => result
                    .failed
                    .push((name.clone(), version.clone(), e.to_string())),
            }
        }
        Ok(result)
    }

    /// Unyank multiple versions in one call.
    /// The default implementation loops over `unyank`. Override for efficiency.
    async fn bulk_unyank(
        &self,
        registry: &str,
        items: &[(String, String)],
    ) -> Result<BulkResult, CoreError> {
        let mut result = BulkResult {
            processed: items.len(),
            succeeded: 0,
            failed: vec![],
        };
        for (name, version) in items {
            match self.unyank(registry, name, version).await {
                Ok(()) => result.succeeded += 1,
                Err(e) => result
                    .failed
                    .push((name.clone(), version.clone(), e.to_string())),
            }
        }
        Ok(result)
    }

    /// Tombstone multiple versions in one call.
    /// The default implementation loops over `tombstone_version`. Override for efficiency.
    ///
    /// A version that was already a tombstone, or that never existed, counts as
    /// succeeded: the caller asked for the coordinate to be gone and it is. The
    /// distinction the caller does care about — did bytes need dropping — is not
    /// this method's to answer.
    async fn bulk_tombstone_versions(
        &self,
        registry: &str,
        items: &[(String, String)],
        deleted_by: Option<&str>,
    ) -> Result<BulkResult, CoreError> {
        let mut result = BulkResult {
            processed: items.len(),
            succeeded: 0,
            failed: vec![],
        };
        for (name, version) in items {
            match self
                .tombstone_version(registry, name, version, deleted_by)
                .await
            {
                Ok(_) => result.succeeded += 1,
                Err(e) => result
                    .failed
                    .push((name.clone(), version.clone(), e.to_string())),
            }
        }
        Ok(result)
    }
}

/// Result of a bulk yank/unyank/delete operation.
#[derive(Debug)]
pub struct BulkResult {
    /// Total items submitted.
    pub processed: usize,
    /// Items processed without error.
    pub succeeded: usize,
    /// Items that failed: (name, version, error message).
    pub failed: Vec<(String, String, String)>,
}
