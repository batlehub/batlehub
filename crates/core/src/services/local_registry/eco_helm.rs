use super::{CoreError, Identity, LocalRegistryService};

impl LocalRegistryService {
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
    ) -> Result<serde_json::Value, CoreError> {
        use crate::services::helm::{compose_index, composed_entry};

        let names = self.backend.list_package_names(registry).await?;
        let (readable, grant_rows) = self.readable_packages(registry, identity).await?;
        let mut entries = serde_json::Map::new();
        for name in &names {
            if !readable.contains(name) {
                continue;
            }
            let versions = match self
                .load_visible_versions_in(registry, name, identity, Some(&grant_rows))
                .await
            {
                Ok(v) => v,
                Err(CoreError::AccessDenied(_)) => continue,
                Err(e) => return Err(e),
            };
            let mut versions: Vec<_> = versions
                .into_iter()
                .filter(|p| !p.yanked && !p.unlisted)
                .collect();
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
            entries.insert(name.clone(), serde_json::Value::Array(list));
        }
        Ok(compose_index(
            entries,
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ))
    }
}
