use async_trait::async_trait;

use crate::{
    entities::{NamespacePackage, TeamNamespace, Visibility},
    error::CoreError,
};

/// Port for team namespace claims and per-package visibility.
///
/// A team namespace maps an auth-provider group to a package prefix within a
/// registry (e.g. group `"frontend"` owns prefix `"frontend"`, so only members
/// of that group may publish packages whose name starts with `"frontend/"`).
///
/// Package visibility is stored per package name (not per version). Changing
/// visibility affects all versions of a package simultaneously.
#[async_trait]
pub trait TeamNamespacePort: Send + Sync {
    /// Return the longest-prefix namespace claim that covers `package` in
    /// `registry`.
    ///
    /// Matching rule: a claim with `prefix = P` covers a package with name `N`
    /// when `N == P` **or** `N` starts with `P/`.
    /// When multiple claims match, the one with the longest prefix wins.
    async fn find_namespace(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Option<TeamNamespace>, CoreError>;

    /// Return all namespace claims for `registry`, ordered by prefix ascending.
    async fn list_namespaces(&self, registry: &str) -> Result<Vec<TeamNamespace>, CoreError>;

    /// Create a namespace claim.
    ///
    /// Returns `CoreError::Conflict` when a claim for the same
    /// `(registry, prefix)` pair already exists.
    async fn claim_namespace(&self, ns: TeamNamespace) -> Result<(), CoreError>;

    /// Delete a namespace claim.
    ///
    /// Succeeds silently when no matching claim exists.
    async fn release_namespace(&self, registry: &str, prefix: &str) -> Result<(), CoreError>;

    /// Set the visibility for all versions of `package` in `registry`.
    async fn set_visibility(
        &self,
        registry: &str,
        package: &str,
        vis: Visibility,
    ) -> Result<(), CoreError>;

    /// Return the current visibility of `package` in `registry`.
    ///
    /// Returns `Visibility::Public` when the package has no published rows yet.
    async fn get_visibility(&self, registry: &str, package: &str) -> Result<Visibility, CoreError>;

    /// Warm whatever cache backs [`Self::get_visibility`] for many packages at
    /// once. The default does nothing.
    async fn prefetch_visibility(&self, _packages: &[(String, String)]) {}

    /// Return all namespace claims across all registries where `group_id` is
    /// one of the supplied `groups`, ordered by registry then prefix.
    async fn list_namespaces_for_groups(
        &self,
        groups: &[String],
    ) -> Result<Vec<TeamNamespace>, CoreError>;

    /// Return all published package versions in `registry` whose name matches
    /// `prefix` (exact match or `prefix + '/'` prefix), ordered by name then
    /// version, with pagination.
    async fn list_packages_in_namespace(
        &self,
        registry: &str,
        prefix: &str,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<NamespacePackage>, CoreError>;

    /// Count published package versions matching the same criteria as
    /// [`Self::list_packages_in_namespace`], without `limit`/`offset`. Used
    /// for accurate pagination totals.
    async fn count_packages_in_namespace(
        &self,
        registry: &str,
        prefix: &str,
    ) -> Result<u64, CoreError>;
}
