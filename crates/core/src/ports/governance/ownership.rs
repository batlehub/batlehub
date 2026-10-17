use async_trait::async_trait;

use crate::{entities::Identity, error::CoreError};

/// An owner entry in the `package_owners` table.
#[derive(Debug, Clone)]
pub struct OwnerEntry {
    pub principal_type: String, // "user" or "group"
    pub principal_id: String,
    pub role: String, // "admin" or "maintainer"
    pub granted_by: Option<String>,
}

/// Port for per-package ownership management.
///
/// On first publish, call `initialize_owner` to make the publisher the package admin.
/// Before subsequent publishes, call `can_publish` to verify the caller is an owner.
#[async_trait]
pub trait OwnershipPort: Send + Sync {
    /// Grant `user_id` the 'admin' role on `package` in `registry`.
    /// Called exactly once: when the first version of a package is published.
    /// Silently succeeds if an owner row for this user already exists (idempotent).
    async fn initialize_owner(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<(), CoreError>;

    /// Make `user_id` the package admin **only if the package has no owner**,
    /// and report whether this call did. Two first publishers racing past
    /// [`Self::can_publish`] (both saw no owner) would otherwise both become
    /// admins; the loser gets `false` and re-checks `can_publish`, which now
    /// sees the winner. Stores override this to make it atomic — the default is
    /// check-then-insert and only fit for test doubles.
    async fn claim_if_unowned(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<bool, CoreError> {
        if !self.list_owners(registry, package).await?.is_empty() {
            return Ok(false);
        }
        self.initialize_owner(registry, package, user_id).await?;
        Ok(true)
    }

    /// Return `true` if `identity` is allowed to publish `package` in `registry`.
    ///
    /// Returns `true` when:
    /// - The package has no owner rows yet (new package — anyone with User role may publish).
    /// - The identity's `user_id` has a row for this package, OR
    /// - Any group in `identity.groups` has a row for this package.
    async fn can_publish(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError>;

    /// Add an owner entry. Returns `CoreError::Conflict` if already present.
    async fn add_owner(
        &self,
        registry: &str,
        package: &str,
        entry: OwnerEntry,
    ) -> Result<(), CoreError>;

    /// Remove an owner entry. Succeeds even if the entry does not exist.
    async fn remove_owner(
        &self,
        registry: &str,
        package: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError>;

    /// List all owners of a package, ordered by `granted_at` ascending.
    async fn list_owners(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<OwnerEntry>, CoreError>;

    /// Drop every owner entry for a package, releasing the name.
    ///
    /// Called when the last version of a package is deleted (RFC 0016 §4.4).
    /// Ownership is keyed by `(registry, package_name)` and nothing else would
    /// remove it, so without this the previous owner keeps publish and
    /// owner-management authority over a name someone else may now take —
    /// authority over a package they have never seen, granted by a decision
    /// nobody remembers making.
    ///
    /// The version tombstones stay, because they are the invariant. The grants
    /// go, because they are a decision about a thing that no longer exists.
    ///
    /// The default loops `list_owners` + `remove_owner`, which is correct for
    /// any store; a backend that can do it in one statement should.
    async fn remove_all_owners(&self, registry: &str, package: &str) -> Result<(), CoreError> {
        for entry in self.list_owners(registry, package).await? {
            self.remove_owner(
                registry,
                package,
                &entry.principal_type,
                &entry.principal_id,
            )
            .await?;
        }
        Ok(())
    }

    /// The reverse of [`list_owners`](OwnershipPort::list_owners): every
    /// `(registry, package)` this identity owns, whether directly or through
    /// one of its groups.
    ///
    /// `list_owners` answers "who owns this package", which is the question the
    /// admin surfaces ask. `GET /api/v1/me/advisories` asks the other one —
    /// "what does this principal own" — and nothing answered it before
    /// RFC 0004 (§6.2, R7).
    ///
    /// Group membership is read from `identity.groups`, so a store cannot
    /// disagree with the request's own view of who the caller is.
    async fn list_owned_by(&self, identity: &Identity) -> Result<Vec<(String, String)>, CoreError> {
        let _ = identity;
        Ok(vec![])
    }
}
