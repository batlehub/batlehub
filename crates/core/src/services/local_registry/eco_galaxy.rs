use super::{CoreError, Identity, LocalRegistryService, PublishedPackage};

impl LocalRegistryService {
    /// Every locally published version of one Ansible collection.
    ///
    /// The composed listing and the composed collection document are both built
    /// from these rows (RFC 0031 §4.4): a local version's `created_at` is its
    /// `published_at`, and `requires_ansible` comes out of the `MANIFEST.json`
    /// stored beside it at publish time, so a read is never an archive open.
    pub async fn get_galaxy_versions(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        self.load_visible_versions_or_not_found(registry, name, identity, "collection")
            .await
    }
}
