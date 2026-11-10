//! One check in front of every upstream call made from a caller's coordinate.
//!
//! `server/src/name_injection_probe.rs` aimed every kind's client at a
//! recorder with names, versions and artifacts holding `?` and `#`: 22 of 29
//! interpolated them raw, so the upstream was asked about another coordinate
//! than the one authorization and the rules judged. `validate_coordinate`
//! refuses them at the service's edge; this refuses them at the client's, for
//! every path that reaches a client without that edge (warming, the scan
//! worker, the upstream audit, a listing read). The forges also encode their
//! segments (PR #304); this does not replace that.
//!
//! Every method of [`RegistryClient`] is delegated, not only the guarded ones:
//! a default left in place here would silently change what a kind does.

use std::sync::Arc;

use async_trait::async_trait;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        DocumentEncoding, DocumentKind, DocumentProbe, FetchedArtifact, ForgeRegistry,
        ForgeReleaseSource, RegistryClient, StreamedDocument, UpstreamPackage, VersionDocument,
    },
    services::reject_url_delimiters,
};

pub struct CoordinateGuard {
    inner: Arc<dyn RegistryClient>,
}

impl CoordinateGuard {
    pub fn new(inner: Arc<dyn RegistryClient>) -> Self {
        Self { inner }
    }
}

impl CoordinateGuard {
    /// The rule of [`reject_url_delimiters`], for this client's kind.
    fn check(&self, pkg: &PackageId) -> Result<(), CoreError> {
        reject_url_delimiters(
            self.inner.registry_type(),
            &pkg.name,
            &pkg.version,
            pkg.artifact.as_deref(),
        )
    }

    fn check_name(&self, package: &str) -> Result<(), CoreError> {
        reject_url_delimiters(self.inner.registry_type(), package, "", None)
    }
}

#[async_trait]
impl RegistryClient for CoordinateGuard {
    fn registry_type(&self) -> &str {
        self.inner.registry_type()
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        self.check(pkg)?;
        self.inner.resolve_metadata(pkg).await
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        self.check(pkg)?;
        self.inner.fetch_artifact(pkg).await
    }

    fn resolve_metadata_from_document(
        &self,
        pkg: &PackageId,
        document: &VersionDocument,
    ) -> Option<PackageMetadata> {
        // No upstream call: a pure read of a document already fetched.
        self.inner.resolve_metadata_from_document(pkg, document)
    }

    async fn fetch_artifact_resolved(
        &self,
        pkg: &PackageId,
        resolved: &PackageMetadata,
    ) -> Result<Option<FetchedArtifact>, CoreError> {
        self.check(pkg)?;
        self.inner.fetch_artifact_resolved(pkg, resolved).await
    }

    async fn probe_artifact(&self, pkg: &PackageId) -> Result<(), CoreError> {
        self.check(pkg)?;
        self.inner.probe_artifact(pkg).await
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        self.check_name(package)?;
        self.inner.list_versions(package).await
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        self.check_name(package)?;
        self.inner.fetch_version_document(package, kind).await
    }

    async fn fetch_linked_readme(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Option<String>, CoreError> {
        // The URL comes from the upstream's own document, not the caller.
        self.inner.fetch_linked_readme(url, max_bytes).await
    }

    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        // Branch names may hold `#`; the forge clients encode every segment.
        self.inner.forge()
    }

    async fn fetch_version_document_stream(
        &self,
        package: &str,
        kind: DocumentKind,
        accept: &[DocumentEncoding],
    ) -> Result<Option<StreamedDocument>, CoreError> {
        self.check_name(package)?;
        self.inner
            .fetch_version_document_stream(package, kind, accept)
            .await
    }

    async fn probe_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
        accept: &[DocumentEncoding],
    ) -> Result<Option<DocumentProbe>, CoreError> {
        self.check_name(package)?;
        self.inner
            .probe_version_document(package, kind, accept)
            .await
    }

    async fn canonical_coordinate(&self, pkg: &PackageId) -> Result<Option<PackageId>, CoreError> {
        self.check(pkg)?;
        self.inner.canonical_coordinate(pkg).await
    }

    fn releases(&self) -> Option<&dyn ForgeReleaseSource> {
        self.inner.releases()
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        // Free text, sent as an encoded query value; `c#` is a fair search.
        self.inner.search_packages(query, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counting(AtomicUsize);

    #[async_trait]
    impl RegistryClient for Counting {
        fn registry_type(&self) -> &str {
            "npm"
        }
        async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(CoreError::NotFound(pkg.to_string()))
        }
        async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(CoreError::NotFound(pkg.to_string()))
        }
        async fn list_versions(&self, _: &str) -> Result<Vec<String>, CoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["1.0.0".into()])
        }
    }

    #[tokio::test]
    async fn a_delimiter_in_any_coordinate_never_reaches_the_client() {
        let inner = Arc::new(Counting::default());
        let g = CoordinateGuard::new(inner.clone());
        for pkg in [
            PackageId::new("r", "left-pad?x=1", "1.0.0"),
            PackageId::new("r", "left-pad#x", "1.0.0"),
            PackageId::new("r", "left-pad", "1.0.0?x"),
            PackageId {
                artifact: Some("a#b.tgz".into()),
                ..PackageId::new("r", "left-pad", "1.0.0")
            },
        ] {
            assert!(matches!(
                g.resolve_metadata(&pkg).await,
                Err(CoreError::InvalidInput(_))
            ));
            assert!(matches!(
                g.fetch_artifact(&pkg).await,
                Err(CoreError::InvalidInput(_))
            ));
        }
        assert!(matches!(
            g.list_versions("a?b").await,
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            g.fetch_version_document("a#b", DocumentKind::Versions)
                .await,
            Err(CoreError::InvalidInput(_))
        ));
        assert_eq!(inner.0.load(Ordering::SeqCst), 0, "nothing was forwarded");
    }

    #[tokio::test]
    async fn a_clean_coordinate_is_forwarded_and_the_kind_kept() {
        let inner = Arc::new(Counting::default());
        let g = CoordinateGuard::new(inner.clone());
        assert_eq!(g.registry_type(), "npm");
        assert_eq!(
            g.list_versions("@scope/left-pad").await.unwrap(),
            vec!["1.0.0"]
        );
        let _ = g
            .resolve_metadata(&PackageId::new("r", "left-pad", "1.0.0+build.1"))
            .await;
        assert_eq!(inner.0.load(Ordering::SeqCst), 2);
    }

    struct Forge;

    #[async_trait]
    impl RegistryClient for Forge {
        fn registry_type(&self) -> &str {
            "github"
        }
        async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            Err(CoreError::NotFound(pkg.to_string()))
        }
        async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
            Err(CoreError::NotFound(pkg.to_string()))
        }
    }

    /// Git allows `#` in a tag; the forge clients encode it. Refusing it
    /// would make such a release unreachable, so on a forge it passes.
    #[tokio::test]
    async fn a_git_tag_holding_a_hash_reaches_a_forge_and_nothing_else() {
        let tag = PackageId::new("r", "o/r", "release#2");
        let forge = CoordinateGuard::new(Arc::new(Forge));
        assert!(matches!(
            forge.resolve_metadata(&tag).await,
            Err(CoreError::NotFound(_))
        ));
        let npm = CoordinateGuard::new(Arc::new(Counting::default()));
        assert!(matches!(
            npm.resolve_metadata(&tag).await,
            Err(CoreError::InvalidInput(_))
        ));
        // `?` is in no git ref name, so a forge refuses it too.
        let q = PackageId::new("r", "o/r", "release?2");
        assert!(matches!(
            forge.resolve_metadata(&q).await,
            Err(CoreError::InvalidInput(_))
        ));
    }
}
