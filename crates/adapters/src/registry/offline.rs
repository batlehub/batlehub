//! The client an air-gapped instance uses instead of a real one (RFC 0008
//! §13, decision 1).
//!
//! # Why the refusal is here and not at the fetch site
//!
//! §5.3 put the decision in `ProxyService::handle`, at the upstream-fetch
//! branch. There is no such branch: there are two artifact fetch sites and
//! five other dial-outs the read path never sees — the passthrough rungs
//! (npm audit, the Go checksum database), metadata resolution, the upstream
//! detail lookup, warming and the README fetch. Enforcing "never dials" at
//! any one of them would leave the others.
//!
//! So it is enforced where clients are *built*: on an instance with
//! `air_gap.enabled`, every `RegistryClient` is wrapped in this one, and
//! every path inherits the refusal because every path goes through a client.
//! There is nothing to forget and nothing to add when a new caller appears.
//!
//! The seeding objection §6.7 raises does not apply: seeding runs on the
//! *connected* instance, which has no `[air_gap]` section and therefore no
//! wrapper.

use std::sync::Arc;

use async_trait::async_trait;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        DocumentKind, FetchedArtifact, ForgeRegistry, RegistryClient, UpstreamPackage,
        VersionDocument,
    },
};

/// Wraps a real client and answers [`CoreError::ContentUnavailable`] to every
/// call that would reach the network.
///
/// The inner client is kept rather than dropped for one reason:
/// [`RegistryClient::registry_type`] is what every kind-dependent decision in
/// the tree reads — the blocking filter, the storage key, the scanners'
/// `supports` — and answering it wrongly would change behaviour that has
/// nothing to do with the network.
pub struct OfflineRegistryClient {
    inner: Arc<dyn RegistryClient>,
    registry: String,
}

impl OfflineRegistryClient {
    pub fn new(inner: Arc<dyn RegistryClient>, registry: impl Into<String>) -> Self {
        Self {
            inner,
            registry: registry.into(),
        }
    }

    fn refuse(&self, key: impl Into<String>) -> CoreError {
        CoreError::ContentUnavailable {
            registry: self.registry.clone(),
            key: key.into(),
        }
    }
}

#[async_trait]
impl RegistryClient for OfflineRegistryClient {
    fn registry_type(&self) -> &str {
        self.inner.registry_type()
    }

    /// **Not** forwarded. A forge client answers `Some(self)`, and the ref
    /// resolver would then dial the forge — the one caller that reaches the
    /// network without going through a method on this trait. Answering
    /// `None` makes an air-gapped forge registry behave as one whose refs
    /// come from the bundle, which is what RFC 0008 §13 decision 3 describes.
    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        None
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Err(self.refuse(pkg.cache_key()))
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(self.refuse(pkg.cache_key()))
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        Err(self.refuse(format!("{package} ({})", kind.as_str())))
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        Err(self.refuse(package))
    }

    async fn search_packages(
        &self,
        query: &str,
        _limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        // Search is a live question about an upstream catalogue. There is no
        // cached answer to fall back on, and an empty list would read as
        // "nothing matches" — which is a claim about the upstream this
        // instance is in no position to make.
        Err(self.refuse(format!("search: {query}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dialling;

    #[async_trait]
    impl RegistryClient for Dialling {
        fn registry_type(&self) -> &str {
            "npm"
        }
        async fn resolve_metadata(&self, _: &PackageId) -> Result<PackageMetadata, CoreError> {
            panic!("an air-gapped instance dialled out");
        }
        async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
            panic!("an air-gapped instance dialled out");
        }
    }

    fn offline() -> OfflineRegistryClient {
        OfflineRegistryClient::new(Arc::new(Dialling), "npm-mirror")
    }

    #[tokio::test]
    async fn every_call_that_would_reach_the_network_is_refused() {
        let c = offline();
        let pkg = PackageId::new("npm-mirror", "left-pad", "1.3.1");
        // `FetchedArtifact` holds a stream and is not `Debug`, so its arm
        // takes the error out by hand rather than through `unwrap_err`.
        let artifact_err = match c.fetch_artifact(&pkg).await {
            Ok(_) => panic!("an air-gapped client returned an artifact"),
            Err(e) => e,
        };
        for err in [
            c.resolve_metadata(&pkg).await.unwrap_err(),
            artifact_err,
            c.fetch_version_document("left-pad", DocumentKind::Versions)
                .await
                .unwrap_err(),
            c.list_versions("left-pad").await.unwrap_err(),
            c.search_packages("pad", 10).await.unwrap_err(),
        ] {
            let CoreError::ContentUnavailable { registry, .. } = err else {
                panic!("expected ContentUnavailable, got {err:?}");
            };
            assert_eq!(registry, "npm-mirror");
        }
    }

    #[tokio::test]
    async fn the_kind_is_still_the_real_one_and_the_forge_hook_is_not() {
        let c = offline();
        assert_eq!(
            c.registry_type(),
            "npm",
            "the kind decides more than the network"
        );
        assert!(
            c.forge().is_none(),
            "the ref resolver reaches the forge without going through this trait"
        );
    }
}
