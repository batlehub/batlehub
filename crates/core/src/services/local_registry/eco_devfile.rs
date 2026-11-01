use std::collections::BTreeSet;

use super::{CoreError, Identity, LocalRegistryService};
use crate::entities::Action;
use crate::services::devfile::{compose_index, HeldStackVersion, Source};

impl LocalRegistryService {
    /// One index document of a local or hybrid devfile registry, holding what
    /// `identity` may see (RFC 0039 §4.4).
    ///
    /// Composed from rows, never stored, as helm's `index.yaml` is: a yank, a
    /// visibility change or a block shows in the next read with nothing to
    /// invalidate. Each version's facts are its `index_metadata.devfile`,
    /// written at publish, plus the publish time as `lastModified`. Yanked
    /// versions are left out.
    pub async fn get_devfile_index(
        &self,
        registry: &str,
        address: &str,
        legacy: bool,
        identity: &Identity,
    ) -> Result<DevfileIndex, CoreError> {
        let listing = self.visible_registry_versions(registry, identity).await?;
        let mut held = Vec::new();
        for (name, versions) in listing.visible {
            for p in versions.into_iter().filter(|p| !p.yanked) {
                let mut facts = p
                    .index_metadata
                    .get("devfile")
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                facts.insert(
                    "lastModified".into(),
                    serde_json::Value::String(
                        p.published_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    ),
                );
                held.push((
                    name.clone(),
                    HeldStackVersion {
                        version: p.version,
                        facts,
                    },
                ));
            }
        }
        Ok(DevfileIndex {
            index: compose_index(address, legacy, &held, Source::Published),
            hosted: listing.hosted.into_iter().collect(),
        })
    }

    /// Whether `identity` may take a stack name upstream lists
    /// (`devfile:stack:claim`, RFC 0039 §4.5). The coordinate is the stack, so
    /// a `[[registries.namespaces]]` grant on a name prefix reaches it.
    pub async fn holds_devfile_claim(
        &self,
        registry: &str,
        stack: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        match self
            .authorize_write(registry, stack, "", identity, Action::DevfileStackClaim)
            .await
        {
            Ok(()) => Ok(true),
            Err(CoreError::AccessDenied(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// [`LocalRegistryService::get_devfile_index`]' answer.
pub struct DevfileIndex {
    /// The composed document, as this caller may see it.
    pub index: serde_json::Value,
    /// Every stack name this registry holds rows for, including the ones the
    /// caller may not see — what a hybrid merge drops from upstream's half
    /// (`services::devfile::merge_hybrid`).
    pub hosted: BTreeSet<String>,
}
