use super::{CoreError, Identity, LocalRegistryService};

impl LocalRegistryService {
    /// Whether `registry` holds a published version of chart `name`, whoever
    /// may see it — what keeps a hybrid registry from fetching the public
    /// chart of a name it hosts (`hybrid_merge = "chart"`).
    pub async fn hosts(&self, registry: &str, name: &str) -> Result<bool, CoreError> {
        self.backend.exists(registry, name).await
    }

    /// `index.yaml` for a local or hybrid Helm registry, holding what
    /// `identity` may see (RFC 0029 §4.4).
    ///
    /// Composed from rows, never stored: each entry is the version's
    /// `Chart.yaml` fields read at publish time, plus `digest` (the stored
    /// checksum) and `created` (the publish time). Yanked versions are omitted.
    /// The visibility rule is `get_conda_repodata`'s, for that function's
    /// reason: a document naming every chart must not list a private one to a
    /// caller the archive route would refuse. `urls` is left empty for the
    /// handler's rewrite, as an upstream entry's is overwritten.
    pub async fn get_helm_index(
        &self,
        registry: &str,
        identity: &Identity,
    ) -> Result<HelmIndex, CoreError> {
        use crate::services::helm::{compose_index, composed_entry};

        let listing = self.visible_registry_versions(registry, identity).await?;
        let mut entries = serde_json::Map::new();
        for (name, versions) in listing.visible {
            let mut versions: Vec<_> = versions.into_iter().filter(|p| !p.yanked).collect();
            // Newest first, the order `helm repo index` writes and `helm search
            // repo` displays.
            versions.sort_by(|a, b| {
                crate::services::version_order::newest_first(&a.version, &b.version)
            });
            let list: Vec<serde_json::Value> = versions
                .into_iter()
                .map(|p| {
                    composed_entry(
                        &p.index_metadata,
                        &p.name,
                        &p.version,
                        &p.checksum,
                        &p.published_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    )
                })
                .collect();
            if list.is_empty() {
                continue;
            }
            entries.insert(name, serde_json::Value::Array(list));
        }
        Ok(HelmIndex {
            index: compose_index(
                entries,
                &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
            hosted: listing.hosted,
        })
    }
}

/// [`LocalRegistryService::get_helm_index`]' answer.
pub struct HelmIndex {
    /// The composed `index.yaml`, as this caller may see it.
    pub index: serde_json::Value,
    /// Every chart name this registry holds rows for, including the ones the
    /// caller may not see — what a hybrid merge needs to hide the upstream
    /// chart of a name hosted here (`services::helm::merge`).
    pub hosted: Vec<String>,
}
