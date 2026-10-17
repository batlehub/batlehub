use super::read::DocumentSlot;
use super::{CoreError, Identity, LocalRegistryService};

impl LocalRegistryService {
    /// Return the `/api/v1/gems/{name}.json`-compatible info for the latest gem version.
    pub async fn get_rubygems_gem_info(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<serde_json::Value, CoreError> {
        let versions = self.load_visible_versions(registry, name, identity).await?;
        let latest = Self::latest_stable_or_newest(&versions).ok_or_else(|| {
            CoreError::NotFound(format!(
                "gem '{name}' not found in local registry '{registry}'"
            ))
        })?;
        let meta = &latest.index_metadata;
        Ok(serde_json::json!({
            "name": name,
            "version": latest.version,
            "platform": meta.get("platform").and_then(|v| v.as_str()).unwrap_or("ruby"),
            "summary": meta.get("summary"),
            "authors": meta.get("authors").and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default(),
            "sha": meta.get("sha"),
            "created_at": latest.published_at.to_rfc3339(),
            "yanked": latest.yanked,
        }))
    }

    /// Return the `/api/v1/versions/{name}.json`-compatible array for all gem versions.
    pub async fn get_rubygems_versions(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<Vec<serde_json::Value>, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, name, identity, "gem")
            .await?;
        let result = versions
            .into_iter()
            .rev() // newest-first to match rubygems.org API
            .map(|pkg| {
                let meta = &pkg.index_metadata;
                serde_json::json!({
                    "number": pkg.version,
                    "platform": meta.get("platform").and_then(|v| v.as_str()).unwrap_or("ruby"),
                    "authors": meta.get("authors").and_then(|a| a.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default(),
                    "summary": meta.get("summary"),
                    "sha": meta.get("sha"),
                    "created_at": pkg.published_at.to_rfc3339(),
                    "prerelease": Self::is_prerelease(&pkg.version),
                    "yanked": pkg.yanked,
                })
            })
            .collect();
        Ok(result)
    }

    /// The compact-index `/info/{gem}` document for a locally published gem.
    ///
    /// This is what Bundler resolves from — the JSON APIs above are a fallback
    /// it reaches for only when the compact index is absent. Nothing generated
    /// it, so the handlers proxied upstream in every mode: a gem published to a
    /// local registry was invisible to `bundle install`, and a `local` registry
    /// answered `/versions` with rubygems.org's index (RFC 0009 §12.15).
    ///
    /// Format is one line per version:
    ///
    /// ```text
    /// ---
    /// 1.0.0 rake:~> 13.0,json:>= 2.0|checksum:<sha256>
    /// ```
    ///
    /// Versions come from [`Self::load_visible_versions`], so blocking and
    /// visibility are applied before a line is written rather than stripped
    /// afterwards — the filtering obligation is met by construction here.
    pub async fn get_rubygems_compact_info(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<String, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, name, identity, "gem")
            .await?;
        Ok(Self::render_compact_info(&versions))
    }

    /// The pure rendering behind [`Self::get_rubygems_compact_info`]: one
    /// `{version} {deps}|checksum:{sha}` line per non-yanked version.
    ///
    /// It writes stored fields verbatim, so it is only as safe as the publish
    /// that stored them — `parse_gem_bytes` refuses any field carrying a
    /// delimiter. Public for `fuzz_gem_compact_index`, which re-reads this
    /// output with an independent reader.
    pub fn render_compact_info(versions: &[crate::entities::PublishedPackage]) -> String {
        let mut out = String::from("---\n");
        for pkg in versions.iter().filter(|p| !p.yanked) {
            let meta = &pkg.index_metadata;
            let deps = meta
                .get("dependencies")
                .and_then(|d| d.as_array())
                .map(|deps| {
                    deps.iter()
                        .filter_map(|d| {
                            let name = d.get("name")?.as_str()?;
                            let req = d.get("requirement").and_then(|r| r.as_str()).unwrap_or("");
                            Some(format!("{name}:{req}"))
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let checksum = meta.get("sha").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!("{} {deps}|checksum:{checksum}\n", pkg.version));
        }
        out
    }

    /// The compact-index `/versions` document: every gem in the registry.
    ///
    /// Each line ends with the MD5 of that gem's `/info` document, which is how
    /// Bundler decides whether its cached copy is current — so it is computed
    /// from the same bytes [`Self::get_rubygems_compact_info`] would return,
    /// not from the gem's own contents.
    pub async fn get_rubygems_compact_versions(
        &self,
        registry: &str,
        identity: &Identity,
    ) -> Result<String, CoreError> {
        use md5::{Digest as _, Md5};

        // §11.7 arm 3: one entry per *audience*, not per caller. Phase 0b found
        // this key load-bearing rather than optional — and §4.4 rule 3 decides
        // what the audience has to include. The slot hands back the read set it
        // keyed on, so this document is filtered with the same resolution the
        // key describes.
        let (cache_key, generation, readable, grant_rows) =
            match self.cached_document(registry, "versions", identity).await? {
                DocumentSlot::Hit(body) => return Ok(body.to_string()),
                DocumentSlot::Miss {
                    key,
                    generation,
                    readable,
                    grants,
                } => (key, generation, readable, grants),
            };

        let names = self.backend.list_package_names(registry).await?;
        // A fixed epoch rather than "now": Bundler treats this header as the
        // point its incremental fetches start from, and a timestamp that moves
        // on every request invalidates every cache on every request.
        let mut out = String::from("created_at: 1970-01-01T00:00:00Z\n---\n");
        for name in &names {
            // RFC 0015 §4.4 — the document lists what this caller may see. The
            // check is free for a caller whose broad tiers grant the read, which
            // is almost all of them; see `Readable`.
            if !readable.contains(name) {
                continue;
            }
            let Ok(versions) = self
                .load_visible_versions_in(registry, name, identity, Some(&grant_rows))
                .await
            else {
                // Not visible to this identity is not an error here — it is a
                // gem this caller does not get to see, like every other listing.
                continue;
            };
            let live: Vec<String> = versions
                .iter()
                .filter(|p| !p.yanked)
                .map(|p| p.version.clone())
                .collect();
            if live.is_empty() {
                continue;
            }
            let info = Self::render_compact_info(&versions);
            let digest = Md5::digest(info.as_bytes());
            out.push_str(&format!(
                "{name} {} {}\n",
                live.join(","),
                hex::encode(digest)
            ));
        }
        self.store_document(cache_key, &out, generation).await;
        Ok(out)
    }

    /// The compact-index `/names` document.
    ///
    /// Unfiltered by version on purpose, like the handler that serves it: a gem
    /// with one blocked version is still a gem that exists. A gem whose every
    /// version is invisible to this caller is not listed.
    pub async fn get_rubygems_compact_names(
        &self,
        registry: &str,
        identity: &Identity,
    ) -> Result<String, CoreError> {
        // §11.7 arm 3, as `/versions` above.
        let (cache_key, generation, readable, grant_rows) =
            match self.cached_document(registry, "names", identity).await? {
                DocumentSlot::Hit(body) => return Ok(body.to_string()),
                DocumentSlot::Miss {
                    key,
                    generation,
                    readable,
                    grants,
                } => (key, generation, readable, grants),
            };

        let names = self.backend.list_package_names(registry).await?;
        let mut out = String::from("---\n");
        for name in &names {
            // §4.4, same as `/versions`: this document names every gem in the
            // registry, so a caller who may not read one must not see it here.
            if !readable.contains(name) {
                continue;
            }
            let Ok(versions) = self
                .load_visible_versions_in(registry, name, identity, Some(&grant_rows))
                .await
            else {
                continue;
            };
            if versions.iter().any(|p| !p.yanked) {
                out.push_str(name);
                out.push('\n');
            }
        }
        self.store_document(cache_key, &out, generation).await;
        Ok(out)
    }
}
