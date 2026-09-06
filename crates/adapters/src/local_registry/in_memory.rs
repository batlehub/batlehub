use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use batlehub_core::{
    entities::{CompactionReport, PublishedPackage, Tombstone},
    error::CoreError,
    ports::LocalRegistryBackend,
};

/// Record status, mirroring the `pending`/`published`/`deleted` lifecycle of
/// [`PostgresLocalRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordStatus {
    Pending,
    Published,
    /// Tombstoned: the coordinate is spent, the bytes are gone, and the row
    /// stays so no later publish can occupy it (RFC 0016 §4.4).
    Deleted,
}

/// The half of a tombstone that outlives compaction, kept beside the record so a
/// deleted row still answers `find_tombstone` after its detail is stripped.
#[derive(Debug, Clone)]
struct DeletionMark {
    deleted_at: DateTime<Utc>,
    deleted_by: Option<String>,
    detail_compacted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct Record {
    pkg: PublishedPackage,
    status: RecordStatus,
    inserted_at: DateTime<Utc>,
    /// `Some` exactly when `status == Deleted`.
    deletion: Option<DeletionMark>,
}

impl Record {
    /// Build the [`Tombstone`] view of a deleted record. Returns `None` for a
    /// record that is not one, so callers cannot accidentally read a live
    /// version as a spent coordinate.
    fn as_tombstone(&self) -> Option<Tombstone> {
        let mark = self.deletion.as_ref()?;
        Some(Tombstone {
            registry: self.pkg.registry.clone(),
            name: self.pkg.name.clone(),
            version: self.pkg.version.clone(),
            deleted_at: mark.deleted_at,
            deleted_by: mark.deleted_by.clone(),
            detail_compacted_at: mark.detail_compacted_at,
            published_at: self.pkg.published_at,
            published_by: self.pkg.published_by.clone(),
            // Mirrors the Postgres column, which compaction nulls. `""` is what
            // a compacted in-memory record carries, and reads back as absent.
            checksum: Some(self.pkg.checksum.clone()).filter(|c| !c.is_empty()),
        })
    }
}

type PackageKey = String; // "{registry}:{name}"
type VersionKey = String; // version string

/// A fully spec-compliant in-memory [`LocalRegistryBackend`].
///
/// Implements the three-step publish protocol (`publish` → artifact write →
/// `commit_publish`), conflict detection on published versions,
/// `cleanup_pending`, and `list_package_names`.
///
/// Intended for integration tests, single-binary demos, and any context that
/// does not need persistence across process restarts. Thread-safe via
/// `tokio::sync::RwLock`.
#[derive(Debug, Default)]
pub struct InMemoryLocalRegistry {
    inner: Arc<RwLock<HashMap<PackageKey, HashMap<VersionKey, Record>>>>,
}

impl InMemoryLocalRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

fn pkg_key(registry: &str, name: &str) -> PackageKey {
    format!("{registry}:{}", name.to_lowercase())
}

/// Looks up `version` under `registry`/`name` and returns it only if it's in
/// `Published` state — the shared lookup + status guard duplicated across
/// yank/unyank/deprecate/undeprecate/unlist/relist below. Returns `None` for a
/// missing package, missing version, a still-`Pending` row, or a `Deleted` one,
/// all of which those methods treat identically (silent no-op) — a tombstone has
/// no bytes to yank or deprecate.
fn published_mut<'a>(
    map: &'a mut HashMap<PackageKey, HashMap<VersionKey, Record>>,
    registry: &str,
    name: &str,
    version: &str,
) -> Option<&'a mut Record> {
    map.get_mut(&pkg_key(registry, name))
        .and_then(|versions| versions.get_mut(version))
        .filter(|r| r.status == RecordStatus::Published)
}

#[async_trait]
impl LocalRegistryBackend for InMemoryLocalRegistry {
    /// Insert the version in *pending* state, invisible to `get_versions` /
    /// `exists` until `commit_publish` is called.
    ///
    /// Returns `CoreError::Conflict` if a *published* version already exists.
    /// Silently overwrites a stale *pending* row (crash recovery for callers
    /// that retry after a partial failure).
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        let versions = map.entry(pkg_key(&pkg.registry, &pkg.name)).or_default();

        if let Some(existing) = versions.get(&pkg.version) {
            // A tombstone is checked before the published case: the coordinate is
            // spent, and saying "already published" about bytes that were deleted
            // would send the publisher looking for something that is not there.
            if let Some(ts) = existing.as_tombstone() {
                return Err(CoreError::Conflict(ts.burned_coordinate_message()));
            }
            if existing.status == RecordStatus::Published {
                return Err(CoreError::Conflict(format!(
                    "{}@{} already published in registry '{}'",
                    pkg.name, pkg.version, pkg.registry
                )));
            }
            // Stale pending row: fall through and overwrite below.
        }

        versions.insert(
            pkg.version.clone(),
            Record {
                pkg,
                status: RecordStatus::Pending,
                inserted_at: Utc::now(),
                deletion: None,
            },
        );
        Ok(())
    }

    /// Promote the pending row to *published*. No-op if the row is missing.
    async fn commit_publish(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(versions) = map.get_mut(&pkg_key(registry, name)) {
            if let Some(record) = versions.get_mut(version) {
                record.status = RecordStatus::Published;
            }
        }
        Ok(())
    }

    async fn yank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(r) = published_mut(&mut map, registry, name, version) {
            r.pkg.yanked = true;
            if let Some(obj) = r.pkg.index_metadata.as_object_mut() {
                obj.insert("yanked".to_owned(), serde_json::Value::Bool(true));
            }
        }
        Ok(())
    }

    async fn unyank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(r) = published_mut(&mut map, registry, name, version) {
            r.pkg.yanked = false;
            if let Some(obj) = r.pkg.index_metadata.as_object_mut() {
                obj.insert("yanked".to_owned(), serde_json::Value::Bool(false));
            }
        }
        Ok(())
    }

    async fn deprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        message: Option<&str>,
    ) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(r) = published_mut(&mut map, registry, name, version) {
            r.pkg.deprecated = true;
            r.pkg.deprecation_message = message.map(str::to_owned);
            if let Some(obj) = r.pkg.index_metadata.as_object_mut() {
                obj.insert(
                    "deprecated".to_owned(),
                    serde_json::Value::String(message.unwrap_or("true").to_owned()),
                );
            }
        }
        Ok(())
    }

    async fn undeprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(r) = published_mut(&mut map, registry, name, version) {
            r.pkg.deprecated = false;
            r.pkg.deprecation_message = None;
            if let Some(obj) = r.pkg.index_metadata.as_object_mut() {
                obj.remove("deprecated");
            }
        }
        Ok(())
    }

    async fn unlist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(r) = published_mut(&mut map, registry, name, version) {
            r.pkg.unlisted = true;
        }
        Ok(())
    }

    async fn relist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(r) = published_mut(&mut map, registry, name, version) {
            r.pkg.unlisted = false;
        }
        Ok(())
    }

    /// `published_mut` filters to published rows, so this is a no-op for a
    /// tombstone: a coordinate that is already spent cannot be protected from a
    /// reclamation that can never reach it.
    async fn set_channel(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        channel: &str,
    ) -> Result<bool, CoreError> {
        let mut map = self.inner.write().await;
        match published_mut(&mut map, registry, name, version) {
            Some(r) => {
                let current = r
                    .pkg
                    .index_metadata
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if current == channel {
                    return Ok(false);
                }
                if let Some(obj) = r.pkg.index_metadata.as_object_mut() {
                    obj.insert("channel".to_owned(), serde_json::json!(channel));
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn set_vsix_signature_provided(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        provided: bool,
    ) -> Result<bool, CoreError> {
        let mut map = self.inner.write().await;
        let Some(r) = published_mut(&mut map, registry, name, version) else {
            return Ok(false);
        };
        let current = r
            .pkg
            .index_metadata
            .get("vsixSignature")
            .and_then(|v| v.as_str())
            == Some("provided");
        if current == provided {
            return Ok(false);
        }
        let Some(obj) = r.pkg.index_metadata.as_object_mut() else {
            return Ok(false);
        };
        if provided {
            obj.insert(
                "vsixSignature".to_owned(),
                serde_json::Value::String("provided".to_owned()),
            );
        } else {
            obj.remove("vsixSignature");
        }
        Ok(true)
    }

    async fn set_retention_keep(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        keep: bool,
    ) -> Result<bool, CoreError> {
        let mut map = self.inner.write().await;
        match published_mut(&mut map, registry, name, version) {
            Some(r) if r.pkg.retention_keep != keep => {
                r.pkg.retention_keep = keep;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn get_versions(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        let map = self.inner.read().await;
        let mut result: Vec<PublishedPackage> = map
            .get(&pkg_key(registry, name))
            .map(|vs| {
                vs.values()
                    .filter(|r| r.status == RecordStatus::Published)
                    .map(|r| r.pkg.clone())
                    .collect()
            })
            .unwrap_or_default();
        result.sort_by_key(|p| p.published_at);
        Ok(result)
    }

    async fn exists(&self, registry: &str, name: &str) -> Result<bool, CoreError> {
        let map = self.inner.read().await;
        Ok(map
            .get(&pkg_key(registry, name))
            .map(|vs| vs.values().any(|r| r.status == RecordStatus::Published))
            .unwrap_or(false))
    }

    /// Rollback only, and it will not remove a tombstone: the coordinate a
    /// tombstone holds is permanently spent, and this is the one path left that
    /// removes a row at all.
    async fn remove_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        let mut map = self.inner.write().await;
        if let Some(versions) = map.get_mut(&pkg_key(registry, name)) {
            let is_tombstone = versions
                .get(version)
                .is_some_and(|r| r.status == RecordStatus::Deleted);
            if !is_tombstone {
                versions.remove(version);
            }
        }
        Ok(())
    }

    async fn tombstone_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        deleted_by: Option<&str>,
    ) -> Result<bool, CoreError> {
        let mut map = self.inner.write().await;
        // `published_mut` filters on `Published`, so a second delete finds
        // nothing and returns `false` with the original `deleted_at` intact.
        let Some(record) = published_mut(&mut map, registry, name, version) else {
            return Ok(false);
        };
        record.status = RecordStatus::Deleted;
        record.deletion = Some(DeletionMark {
            deleted_at: Utc::now(),
            deleted_by: deleted_by.map(str::to_owned),
            detail_compacted_at: None,
        });
        Ok(true)
    }

    async fn find_tombstone(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<Tombstone>, CoreError> {
        let map = self.inner.read().await;
        Ok(map
            .get(&pkg_key(registry, name))
            .and_then(|vs| vs.get(version))
            .and_then(Record::as_tombstone))
    }

    async fn list_tombstones(
        &self,
        registry: &str,
        name: Option<&str>,
    ) -> Result<Vec<Tombstone>, CoreError> {
        let prefix = format!("{registry}:");
        let mut out: Vec<Tombstone> = {
            let map = self.inner.read().await;
            map.iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .flat_map(|(_, vs)| vs.values())
                .filter(|r| name.is_none_or(|n| r.pkg.name.eq_ignore_ascii_case(n)))
                .filter_map(Record::as_tombstone)
                .collect()
        }; // read lock dropped here
        out.sort_by(|a, b| {
            b.deleted_at
                .cmp(&a.deleted_at)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.version.cmp(&b.version))
        });
        Ok(out)
    }

    /// Mirrors the Postgres column-by-column strip: `index_metadata` becomes an
    /// empty object rather than disappearing, and the coordinate, `deleted_at`,
    /// `deleted_by` and `published_at` — the claim and its provenance — stay.
    async fn compact_tombstone_detail(
        &self,
        registry: &str,
        older_than: Duration,
        dry_run: bool,
    ) -> Result<CompactionReport, CoreError> {
        let Ok(std_dur) = chrono::Duration::from_std(older_than) else {
            return Ok(CompactionReport {
                dry_run,
                ..Default::default()
            });
        };
        let cutoff = Utc::now() - std_dur;
        let prefix = format!("{registry}:");

        let mut map = self.inner.write().await;
        let mut coordinates = Vec::new();
        let mut total = 0u64;
        for (_, versions) in map.iter_mut().filter(|(k, _)| k.starts_with(&prefix)) {
            for record in versions.values_mut() {
                let Some(mark) = record.deletion.as_mut() else {
                    continue;
                };
                total += 1;
                if mark.detail_compacted_at.is_some() || mark.deleted_at >= cutoff {
                    continue;
                }
                coordinates.push(format!("{}@{}", record.pkg.name, record.pkg.version));
                if dry_run {
                    continue;
                }
                mark.detail_compacted_at = Some(Utc::now());
                record.pkg.index_metadata = serde_json::json!({});
                record.pkg.checksum = String::new();
                record.pkg.published_by = None;
                record.pkg.signature_bytes = None;
                record.pkg.signature_type = None;
                record.pkg.deprecation_message = None;
            }
        }
        coordinates.sort();
        Ok(CompactionReport {
            compacted: coordinates.len() as u64,
            skipped: total.saturating_sub(coordinates.len() as u64),
            dry_run,
            coordinates,
        })
    }

    /// Remove *pending* rows whose `inserted_at` is older than `older_than`.
    /// Published rows are never touched. Returns the number of rows deleted.
    async fn cleanup_pending(&self, older_than: Duration) -> Result<u64, CoreError> {
        // chrono::Duration::from_std fails only on absurd durations (>292 years).
        // Treat that as "nothing qualifies" rather than wiping the entire pending set.
        let Ok(std_dur) = chrono::Duration::from_std(older_than) else {
            return Ok(0);
        };
        let cutoff = Utc::now() - std_dur;
        let mut map = self.inner.write().await;
        let mut removed = 0u64;
        for versions in map.values_mut() {
            let before = versions.len();
            versions.retain(|_, r| !(r.status == RecordStatus::Pending && r.inserted_at < cutoff));
            removed += (before - versions.len()) as u64;
        }
        Ok(removed)
    }

    /// Return distinct package names that have at least one *published* version
    /// in `registry`, sorted alphabetically.
    async fn list_package_names(&self, registry: &str) -> Result<Vec<String>, CoreError> {
        let prefix = format!("{registry}:");
        let mut names: Vec<String> = {
            let map = self.inner.read().await;
            map.iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .filter_map(|(_, vs)| {
                    vs.values()
                        .find(|r| r.status == RecordStatus::Published)
                        .map(|r| r.pkg.name.clone())
                })
                .collect()
        }; // read lock dropped here
        names.sort();
        Ok(names)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;

    use batlehub_core::{
        entities::PublishedPackage, error::CoreError, ports::LocalRegistryBackend,
    };

    use super::InMemoryLocalRegistry;

    fn pkg(registry: &str, name: &str, version: &str) -> PublishedPackage {
        PublishedPackage {
            registry: registry.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            checksum: format!("sha256-{version}"),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: serde_json::json!({"yanked": false}),
            published_at: Utc::now(),
            published_by: Some("test-user".to_owned()),
            signature_bytes: None,
            signature_type: None,
            visibility: Default::default(),
            retention_keep: false,
        }
    }

    /// Publish then commit makes the version visible.
    #[tokio::test]
    async fn commit_promotes_pending_to_published() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();

        // Pending — not visible yet.
        assert!(!store.exists("reg", "foo").await.unwrap());
        assert!(store.get_versions("reg", "foo").await.unwrap().is_empty());

        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();

        assert!(store.exists("reg", "foo").await.unwrap());
        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "1.0.0");
    }

    /// Publishing a duplicate *published* version returns Conflict.
    #[tokio::test]
    async fn duplicate_published_version_is_conflict() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();

        let err = store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );
    }

    /// A stale pending row (from a prior crash) is silently overwritten so the
    /// caller can retry.
    #[tokio::test]
    async fn stale_pending_row_is_overwritten_on_retry() {
        let store = InMemoryLocalRegistry::new();
        // First attempt — crashes before commit.
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        // Retry — must succeed, not return Conflict.
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();
        assert!(store.exists("reg", "foo").await.unwrap());
    }

    /// Yank sets `yanked = true` and updates `index_metadata`.
    #[tokio::test]
    async fn yank_sets_flag_and_metadata() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();
        store.yank("reg", "foo", "1.0.0").await.unwrap();

        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert!(versions[0].yanked);
        assert_eq!(
            versions[0].index_metadata["yanked"],
            serde_json::Value::Bool(true)
        );
    }

    /// Unyank reverses a yank.
    #[tokio::test]
    async fn unyank_clears_flag_and_metadata() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();
        store.yank("reg", "foo", "1.0.0").await.unwrap();
        store.unyank("reg", "foo", "1.0.0").await.unwrap();

        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert!(!versions[0].yanked);
        assert_eq!(
            versions[0].index_metadata["yanked"],
            serde_json::Value::Bool(false)
        );
    }

    /// Deprecate sets the flag + message and mirrors into `index_metadata`.
    #[tokio::test]
    async fn deprecate_sets_flag_message_and_metadata() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();
        store
            .deprecate("reg", "foo", "1.0.0", Some("use bar instead"))
            .await
            .unwrap();

        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert!(versions[0].deprecated);
        assert_eq!(
            versions[0].deprecation_message.as_deref(),
            Some("use bar instead")
        );
        assert_eq!(versions[0].index_metadata["deprecated"], "use bar instead");

        store.undeprecate("reg", "foo", "1.0.0").await.unwrap();
        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert!(!versions[0].deprecated);
        assert!(versions[0].deprecation_message.is_none());
        assert!(versions[0].index_metadata.get("deprecated").is_none());
    }

    /// Unlist sets the flag; relist clears it.
    #[tokio::test]
    async fn unlist_and_relist_toggle_flag() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();

        store.unlist("reg", "foo", "1.0.0").await.unwrap();
        assert!(store.get_versions("reg", "foo").await.unwrap()[0].unlisted);

        store.relist("reg", "foo", "1.0.0").await.unwrap();
        assert!(!store.get_versions("reg", "foo").await.unwrap()[0].unlisted);
    }

    /// `remove_version` deletes a record regardless of its status.
    #[tokio::test]
    async fn remove_version_deletes_published_record() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();
        store.remove_version("reg", "foo", "1.0.0").await.unwrap();

        assert!(!store.exists("reg", "foo").await.unwrap());
        assert!(store.get_versions("reg", "foo").await.unwrap().is_empty());
    }

    /// `remove_version` on a pending row (rollback scenario).
    #[tokio::test]
    async fn remove_version_deletes_pending_record() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        // Do NOT commit — simulate rollback.
        store.remove_version("reg", "foo", "1.0.0").await.unwrap();

        // A fresh publish of the same version must now succeed.
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "1.0.0").await.unwrap();
        assert!(store.exists("reg", "foo").await.unwrap());
    }

    /// `cleanup_pending` removes old pending rows but never published ones.
    #[tokio::test]
    async fn cleanup_pending_removes_old_pending_only() {
        use std::ops::Sub;

        let store = InMemoryLocalRegistry::new();

        // Insert a published version — must survive cleanup.
        store.publish(pkg("reg", "bar", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "bar", "1.0.0").await.unwrap();

        // Insert a fresh pending version — too new to be cleaned up.
        store.publish(pkg("reg", "bar", "2.0.0")).await.unwrap();

        // Manually backdate the pending row so it looks old.
        {
            let mut map = store.inner.write().await;
            let key = super::pkg_key("reg", "bar");
            if let Some(r) = map.get_mut(&key).and_then(|vs| vs.get_mut("2.0.0")) {
                r.inserted_at = Utc::now().sub(chrono::Duration::hours(2));
            }
        }

        let removed = store
            .cleanup_pending(Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(removed, 1, "expected 1 pending row removed");

        // Published 1.0.0 must still be visible.
        assert!(store.exists("reg", "bar").await.unwrap());
        let versions = store.get_versions("reg", "bar").await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "1.0.0");
    }

    /// `cleanup_pending` with zero duration removes nothing if all pending rows
    /// are brand-new.
    #[tokio::test]
    async fn cleanup_pending_leaves_fresh_pending_intact() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        let removed = store
            .cleanup_pending(Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(removed, 0);
    }

    /// `list_package_names` returns alphabetically sorted names of packages
    /// with at least one published version, excluding packages with only
    /// pending versions.
    #[tokio::test]
    async fn list_package_names_published_only_sorted() {
        let store = InMemoryLocalRegistry::new();

        for name in ["charlie", "alpha", "beta"] {
            store.publish(pkg("reg", name, "1.0.0")).await.unwrap();
            store.commit_publish("reg", name, "1.0.0").await.unwrap();
        }

        // Pending-only package — must not appear.
        store.publish(pkg("reg", "delta", "1.0.0")).await.unwrap();

        // Different registry — must not appear.
        store
            .publish(pkg("other-reg", "zeta", "1.0.0"))
            .await
            .unwrap();
        store
            .commit_publish("other-reg", "zeta", "1.0.0")
            .await
            .unwrap();

        let names = store.list_package_names("reg").await.unwrap();
        assert_eq!(names, vec!["alpha", "beta", "charlie"]);
    }

    /// `get_versions` returns results sorted by `published_at` ASC.
    #[tokio::test]
    async fn get_versions_sorted_by_published_at() {
        let store = InMemoryLocalRegistry::new();

        let t0 = Utc::now();
        let mut v1 = pkg("reg", "foo", "1.0.0");
        let mut v2 = pkg("reg", "foo", "2.0.0");
        let mut v3 = pkg("reg", "foo", "3.0.0");
        v1.published_at = t0;
        v2.published_at = t0 + chrono::Duration::seconds(1);
        v3.published_at = t0 + chrono::Duration::seconds(2);

        // Publish in reverse order to verify sort.
        for v in [v3.clone(), v1.clone(), v2.clone()] {
            let ver = v.version.clone();
            store.publish(v).await.unwrap();
            store.commit_publish("reg", "foo", &ver).await.unwrap();
        }

        let versions = store.get_versions("reg", "foo").await.unwrap();
        let got: Vec<&str> = versions.iter().map(|p| p.version.as_str()).collect();
        assert_eq!(got, vec!["1.0.0", "2.0.0", "3.0.0"]);
    }

    /// `exists` returns false for an unknown package.
    #[tokio::test]
    async fn exists_false_for_unknown_package() {
        let store = InMemoryLocalRegistry::new();
        assert!(!store.exists("reg", "unknown").await.unwrap());
    }

    /// The default `bulk_yank` implementation yanks multiple versions in one call.
    #[tokio::test]
    async fn bulk_yank_yanks_multiple_versions() {
        let store = InMemoryLocalRegistry::new();
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            store.publish(pkg("reg", "foo", v)).await.unwrap();
            store.commit_publish("reg", "foo", v).await.unwrap();
        }

        let result = store
            .bulk_yank(
                "reg",
                &[
                    ("foo".to_owned(), "1.0.0".to_owned()),
                    ("foo".to_owned(), "2.0.0".to_owned()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(result.succeeded, 2);
        assert!(result.failed.is_empty());

        let versions = store.get_versions("reg", "foo").await.unwrap();
        let yanked: Vec<&str> = versions
            .iter()
            .filter(|p| p.yanked)
            .map(|p| p.version.as_str())
            .collect();
        assert_eq!(yanked, vec!["1.0.0", "2.0.0"]);
        assert!(
            !versions
                .iter()
                .find(|p| p.version == "3.0.0")
                .unwrap()
                .yanked
        );
    }

    /// `bulk_unyank` reverses a bulk yank.
    #[tokio::test]
    async fn bulk_unyank_unyanks_multiple_versions() {
        let store = InMemoryLocalRegistry::new();
        for v in ["1.0.0", "2.0.0"] {
            store.publish(pkg("reg", "foo", v)).await.unwrap();
            store.commit_publish("reg", "foo", v).await.unwrap();
            store.yank("reg", "foo", v).await.unwrap();
        }

        let result = store
            .bulk_unyank(
                "reg",
                &[
                    ("foo".to_owned(), "1.0.0".to_owned()),
                    ("foo".to_owned(), "2.0.0".to_owned()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(result.succeeded, 2);

        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert!(versions.iter().all(|p| !p.yanked));
    }

    /// `bulk_tombstone_versions` takes multiple versions out of every listing —
    /// and, being a tombstone rather than a removal, keeps their coordinates.
    #[tokio::test]
    async fn bulk_tombstone_removes_multiple_versions_from_listings() {
        let store = InMemoryLocalRegistry::new();
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            store.publish(pkg("reg", "foo", v)).await.unwrap();
            store.commit_publish("reg", "foo", v).await.unwrap();
        }

        let result = store
            .bulk_tombstone_versions(
                "reg",
                &[
                    ("foo".to_owned(), "1.0.0".to_owned()),
                    ("foo".to_owned(), "3.0.0".to_owned()),
                ],
                Some("alice"),
            )
            .await
            .unwrap();
        assert_eq!(result.succeeded, 2);

        let versions = store.get_versions("reg", "foo").await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "2.0.0");

        // The coordinates are spent, not freed.
        for v in ["1.0.0", "3.0.0"] {
            let ts = store.find_tombstone("reg", "foo", v).await.unwrap();
            assert_eq!(ts.expect("tombstone").deleted_by.as_deref(), Some("alice"));
            let err = store.publish(pkg("reg", "foo", v)).await.unwrap_err();
            assert!(
                matches!(err, CoreError::Conflict(ref m) if m.contains("never reused")),
                "re-publishing a deleted version must be refused, got {err:?}"
            );
        }
    }

    /// Operations on a different registry name are fully isolated.
    #[tokio::test]
    async fn registry_namespaces_are_isolated() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg-a", "foo", "1.0.0")).await.unwrap();
        store.commit_publish("reg-a", "foo", "1.0.0").await.unwrap();

        assert!(!store.exists("reg-b", "foo").await.unwrap());
        assert!(store.get_versions("reg-b", "foo").await.unwrap().is_empty());
    }

    // ── Tombstones (RFC 0016) ─────────────────────────────────────────────────

    /// Publish and tombstone in one step, for the tests that are about what
    /// happens afterwards.
    async fn published_then_deleted(store: &InMemoryLocalRegistry, version: &str) {
        store.publish(pkg("reg", "foo", version)).await.unwrap();
        store.commit_publish("reg", "foo", version).await.unwrap();
        assert!(store
            .tombstone_version("reg", "foo", version, Some("alice"))
            .await
            .unwrap());
    }

    /// A tombstone leaves every listing at once: the version list, the existence
    /// check, and the name catalogue. The last is a separate query and would not
    /// be fixed by the same edit as the first two.
    #[tokio::test]
    async fn a_tombstone_leaves_every_listing() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;

        assert!(store.get_versions("reg", "foo").await.unwrap().is_empty());
        assert!(!store.exists("reg", "foo").await.unwrap());
        assert!(store.list_package_names("reg").await.unwrap().is_empty());
    }

    /// A tombstone has nothing to yank, deprecate or unlist — those mutators
    /// take the published path and must find nothing.
    #[tokio::test]
    async fn lifecycle_mutations_do_not_reach_a_tombstone() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;

        store.yank("reg", "foo", "1.0.0").await.unwrap();
        store.unlist("reg", "foo", "1.0.0").await.unwrap();
        store
            .deprecate("reg", "foo", "1.0.0", Some("gone"))
            .await
            .unwrap();

        // Still a tombstone, still absent, still not resurrected by any of them.
        assert!(store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .is_some());
        assert!(store.get_versions("reg", "foo").await.unwrap().is_empty());
    }

    /// Deleting twice returns `false` and keeps the first `deleted_at` — the
    /// timestamp compaction ages against.
    #[tokio::test]
    async fn tombstoning_is_idempotent_and_keeps_the_first_timestamp() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;
        let first = store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .unwrap();

        assert!(!store
            .tombstone_version("reg", "foo", "1.0.0", Some("bob"))
            .await
            .unwrap());
        let second = store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.deleted_at, second.deleted_at);
        assert_eq!(second.deleted_by.as_deref(), Some("alice"));
    }

    /// Tombstoning something that was never published, or is still pending, does
    /// nothing and says so.
    #[tokio::test]
    async fn tombstoning_a_missing_or_pending_version_is_false() {
        let store = InMemoryLocalRegistry::new();
        assert!(!store
            .tombstone_version("reg", "ghost", "1.0.0", None)
            .await
            .unwrap());

        // Reserved but never committed: it was never visible, so it spends nothing.
        store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap();
        assert!(!store
            .tombstone_version("reg", "foo", "1.0.0", None)
            .await
            .unwrap());
        assert!(store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .is_none());
    }

    /// `remove_version` is the publish rollback: it clears a pending row and
    /// refuses to touch a tombstone, which is the only row whose disappearance
    /// would free a spent coordinate.
    #[tokio::test]
    async fn remove_version_clears_a_pending_row_but_never_a_tombstone() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "rolled", "1.0.0")).await.unwrap();
        store
            .remove_version("reg", "rolled", "1.0.0")
            .await
            .unwrap();
        assert!(store.publish(pkg("reg", "rolled", "1.0.0")).await.is_ok());

        published_then_deleted(&store, "1.0.0").await;
        store.remove_version("reg", "foo", "1.0.0").await.unwrap();
        assert!(store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .is_some());
    }

    /// `list_tombstones` is the audit view: newest first, optionally narrowed to
    /// one package, and it never returns a live version.
    #[tokio::test]
    async fn list_tombstones_is_newest_first_and_filters_by_name() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;
        published_then_deleted(&store, "2.0.0").await;
        store.publish(pkg("reg", "bar", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "bar", "1.0.0").await.unwrap();
        store
            .tombstone_version("reg", "bar", "1.0.0", None)
            .await
            .unwrap();
        // A live version, which must not appear.
        store.publish(pkg("reg", "foo", "3.0.0")).await.unwrap();
        store.commit_publish("reg", "foo", "3.0.0").await.unwrap();

        let all = store.list_tombstones("reg", None).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(
            all.windows(2).all(|w| w[0].deleted_at >= w[1].deleted_at),
            "newest deletion first"
        );

        let foo = store.list_tombstones("reg", Some("foo")).await.unwrap();
        assert_eq!(foo.len(), 2);
        assert!(foo.iter().all(|t| t.name == "foo"));
        assert!(
            !foo.iter().any(|t| t.version == "3.0.0"),
            "a live version is not a tombstone"
        );
    }

    /// Compaction strips detail, keeps the claim, and leaves the refusal intact.
    #[tokio::test]
    async fn compaction_strips_detail_and_keeps_the_claim() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;

        let report = store
            .compact_tombstone_detail("reg", Duration::from_secs(0), false)
            .await
            .unwrap();
        assert_eq!(report.compacted, 1);
        assert_eq!(report.coordinates, vec!["foo@1.0.0".to_owned()]);
        assert!(!report.dry_run);

        let ts = store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .expect("the row survives — that is the whole design");
        assert!(
            ts.detail_compacted_at.is_some(),
            "compaction stamps the row it stripped"
        );
        assert!(ts.checksum.is_none());
        assert!(ts.published_by.is_none());
        assert_eq!(ts.deleted_by.as_deref(), Some("alice"));

        let err = store.publish(pkg("reg", "foo", "1.0.0")).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Conflict(ref m) if m.contains("never reused")),
            "a compacted tombstone still spends its coordinate, got {err:?}"
        );
    }

    /// A dry run writes nothing and reports what the live run then does.
    #[tokio::test]
    async fn compaction_dry_run_writes_nothing() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;

        let preview = store
            .compact_tombstone_detail("reg", Duration::from_secs(0), true)
            .await
            .unwrap();
        assert!(preview.dry_run);
        assert_eq!(preview.compacted, 1);
        assert!(store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .unwrap()
            .checksum
            .is_some());

        let live = store
            .compact_tombstone_detail("reg", Duration::from_secs(0), false)
            .await
            .unwrap();
        assert_eq!(live.coordinates, preview.coordinates);
    }

    /// A tombstone inside the window keeps its detail, and a second run over an
    /// already-compacted one is a no-op rather than a re-stamp.
    #[tokio::test]
    async fn compaction_respects_the_window_and_does_not_restamp() {
        let store = InMemoryLocalRegistry::new();
        published_then_deleted(&store, "1.0.0").await;

        let inside = store
            .compact_tombstone_detail("reg", Duration::from_secs(3600), false)
            .await
            .unwrap();
        assert_eq!(inside.compacted, 0);
        assert_eq!(inside.skipped, 1);

        store
            .compact_tombstone_detail("reg", Duration::from_secs(0), false)
            .await
            .unwrap();
        let stamp = store
            .find_tombstone("reg", "foo", "1.0.0")
            .await
            .unwrap()
            .unwrap()
            .detail_compacted_at;

        let again = store
            .compact_tombstone_detail("reg", Duration::from_secs(0), false)
            .await
            .unwrap();
        assert_eq!(again.compacted, 0);
        assert_eq!(again.skipped, 1);
        assert_eq!(
            store
                .find_tombstone("reg", "foo", "1.0.0")
                .await
                .unwrap()
                .unwrap()
                .detail_compacted_at,
            stamp,
        );
    }

    /// Compaction never touches a live row.
    #[tokio::test]
    async fn compaction_never_touches_a_live_row() {
        let store = InMemoryLocalRegistry::new();
        store.publish(pkg("reg", "alive", "1.0.0")).await.unwrap();
        store.commit_publish("reg", "alive", "1.0.0").await.unwrap();
        let before = store.get_versions("reg", "alive").await.unwrap();

        let report = store
            .compact_tombstone_detail("reg", Duration::from_secs(0), false)
            .await
            .unwrap();
        assert_eq!(report.compacted, 0);

        let after = store.get_versions("reg", "alive").await.unwrap();
        assert_eq!(after[0].checksum, before[0].checksum);
        assert_eq!(after[0].index_metadata, before[0].index_metadata);
        assert_eq!(after[0].published_by, before[0].published_by);
    }

    /// Compaction is scoped to the registry it names — the store is shared by
    /// every registry on the instance.
    #[tokio::test]
    async fn compaction_is_scoped_to_one_registry() {
        let store = InMemoryLocalRegistry::new();
        for reg in ["reg-a", "reg-b"] {
            store.publish(pkg(reg, "foo", "1.0.0")).await.unwrap();
            store.commit_publish(reg, "foo", "1.0.0").await.unwrap();
            store
                .tombstone_version(reg, "foo", "1.0.0", None)
                .await
                .unwrap();
        }

        let report = store
            .compact_tombstone_detail("reg-a", Duration::from_secs(0), false)
            .await
            .unwrap();
        assert_eq!(report.compacted, 1);
        assert!(
            store
                .find_tombstone("reg-b", "foo", "1.0.0")
                .await
                .unwrap()
                .unwrap()
                .checksum
                .is_some(),
            "the other registry's tombstone detail must be untouched"
        );
    }
}
