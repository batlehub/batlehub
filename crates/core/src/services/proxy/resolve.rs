use std::sync::Arc;

use chrono::Utc;

use crate::entities::SbomFormat;
use crate::error::CoreError;
use crate::ports::CacheEntry;
use crate::services::cache_control::parse_cache_control;
use crate::services::sbom::SbomProxiedOptions;

use super::{ProxyRequest, ProxyService};

impl ProxyService {
    /// Resolves metadata from cache (hit) or upstream (miss/stale).
    pub(super) async fn resolve_metadata_cached(
        &self,
        client: &Arc<dyn crate::ports::RegistryClient>,
        policy: &Option<Arc<crate::services::hot_config::RegistryPolicy>>,
        req: &ProxyRequest,
        cache_key: &str,
        ttl: Option<std::time::Duration>,
        registry_label: &Arc<str>,
    ) -> Result<crate::entities::PackageMetadata, CoreError> {
        self.resolve_metadata_inner(client, policy, req, cache_key, ttl, registry_label, true)
            .await
    }

    /// The same resolve, without recording anything.
    ///
    /// For the console's per-version README read (RFC 0007 §5.6). That read is
    /// started by a **page view**, and a page view must write nothing: a
    /// `package_readmes` row for a version this instance holds no bytes for
    /// would have nothing that ever deletes it, because deletion keys on a
    /// version being deleted and a version never held here is never deleted.
    ///
    /// It still goes through the *cache*, which is the other half of §4.4: N
    /// readers of the same version during one TTL produce one upstream request,
    /// and a later real download finds the entry already warm.
    pub(super) async fn resolve_metadata_uncaptured(
        &self,
        client: &Arc<dyn crate::ports::RegistryClient>,
        policy: &Option<Arc<crate::services::hot_config::RegistryPolicy>>,
        req: &ProxyRequest,
        cache_key: &str,
        ttl: Option<std::time::Duration>,
        registry_label: &Arc<str>,
    ) -> Result<crate::entities::PackageMetadata, CoreError> {
        self.resolve_metadata_inner(client, policy, req, cache_key, ttl, registry_label, false)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_metadata_inner(
        &self,
        client: &Arc<dyn crate::ports::RegistryClient>,
        policy: &Option<Arc<crate::services::hot_config::RegistryPolicy>>,
        req: &ProxyRequest,
        cache_key: &str,
        ttl: Option<std::time::Duration>,
        registry_label: &Arc<str>,
        capture_readme: bool,
    ) -> Result<crate::entities::PackageMetadata, CoreError> {
        if let Some(entry) = self.cache.get(cache_key).await? {
            tracing::debug!(key = %cache_key, "metadata cache hit");
            metrics::counter!("batlehub_metadata_cache_hits_total", "registry" => Arc::clone(registry_label)).increment(1);
            return Ok(entry.metadata);
        }
        tracing::debug!(key = %cache_key, "metadata cache miss, fetching from upstream");
        metrics::counter!("batlehub_metadata_cache_misses_total", "registry" => Arc::clone(registry_label)).increment(1);
        let meta = match super::time_upstream_call(
            registry_label,
            "resolve_metadata",
            &self.metrics,
            client.resolve_metadata(&req.package_id),
        )
        .await
        {
            Ok(m) => {
                self.metrics.record_upstream_outcome(registry_label, true);
                m
            }
            Err(e) => {
                self.metrics.record_upstream_outcome(registry_label, false);
                let serve_stale = policy
                    .as_ref()
                    .map(|p| p.serve_stale_metadata)
                    .unwrap_or(false);
                if serve_stale && matches!(e, CoreError::Registry(_)) {
                    if let Some(stale) = self.cache.get_stale(cache_key).await? {
                        tracing::warn!(key = %cache_key, error = %e, "upstream unavailable; serving stale metadata");
                        return Ok(stale.metadata);
                    }
                }
                metrics::counter!("batlehub_upstream_errors_total", "registry" => Arc::clone(registry_label)).increment(1);
                super::warn_if_audit_failed(
                    self.repo
                        .record_access(
                            crate::entities::AccessEvent::proxy_error(
                                req.package_id.clone(),
                                req.identity.user_id.clone(),
                                req.identity.role.clone(),
                                e.to_string(),
                            )
                            .with_ip_ua(req.ip_address.clone(), req.user_agent.clone()),
                        )
                        .await,
                    "proxy error",
                );
                return Err(e);
            }
        };
        let skip = meta
            .cache_control
            .as_deref()
            .map(|h| parse_cache_control(h).no_store)
            .unwrap_or(false);
        if !skip {
            self.cache
                .set(
                    cache_key,
                    CacheEntry {
                        metadata: meta.clone(),
                        cached_at: Utc::now(),
                        expires_at: None,
                    },
                    ttl,
                )
                .await?;
        }
        // The document has just been parsed and the README is a field of it, so
        // this is where it is read (RFC 0007 §5.1). Only on the upstream branch:
        // a cache hit returns above, so a re-resolve within the TTL does not
        // re-record. And only when the caller is a *request path* — a page view
        // reads the same document and stores nothing.
        if capture_readme {
            self.maybe_record_readme(&req.package_id.registry, &meta, client)
                .await;
        }
        Ok(meta)
    }

    /// Record the README a just-resolved metadata document carried, in a
    /// detached task.
    ///
    /// Detached because this is a database write — and, for the linked kinds, an
    /// outbound request — on a path a package manager is waiting on. Non-fatal
    /// for the same reason SBOM generation is: a package resolve must not fail
    /// because prose could not be stored.
    async fn maybe_record_readme(
        &self,
        registry_name: &str,
        metadata: &crate::entities::PackageMetadata,
        client: &Arc<dyn crate::ports::RegistryClient>,
    ) {
        let Some(ref readme_svc) = self.readme else {
            return;
        };
        let Some(found) = crate::entities::MetadataReadme::from_extra(&metadata.extra) else {
            return;
        };
        // A registry with no entry is one a test built by hand, not one with the
        // feature off: the builder writes an entry for every configured
        // registry and the default is enabled.
        let cfg = {
            let hot = self.hot.read().await;
            hot.readme.get(registry_name).cloned().unwrap_or_default()
        };
        if !cfg.enabled {
            return;
        }

        let svc = Arc::clone(readme_svc);
        let client = Arc::clone(client);
        let meta = metadata.clone();
        tokio::spawn(async move {
            let outcome = if found.content.is_some() {
                svc.record_from_metadata(&meta, &cfg).await
            } else if let Some(url) = found.url.as_deref() {
                // **One byte past the cap.** `record` decides `truncated` with
                // `truncate_to(text, cfg.max_bytes)`, which reports `false` for
                // anything that fits — and the fetch already guaranteed it fits.
                // So a linked README the read had cut was stored as complete,
                // and the panel showed a document stopping mid-sentence against
                // a `truncated` column whose whole point is that a cut is never
                // silent. Asking for one more byte is what lets the writer tell
                // "exactly the cap" from "longer than the cap"; it still does
                // the real cut, on a character boundary.
                match client
                    .fetch_linked_readme(url, cfg.max_bytes.saturating_add(1))
                    .await
                {
                    Ok(Some(text)) => {
                        svc.record_from_linked(&meta.id, text, found.format, &cfg)
                            .await
                    }
                    // A missing asset, or a kind whose client does not implement
                    // the read, is a fact about that version rather than a
                    // failure: the panel says the README could not be read.
                    Ok(None) => return,
                    Err(e) => {
                        tracing::warn!(
                            package = %meta.id, url = %url, error = %e,
                            "readme: linked fetch failed (non-fatal)"
                        );
                        return;
                    }
                }
            } else {
                return;
            };
            if let Err(e) = outcome {
                tracing::warn!(package = %meta.id, error = %e, "readme: record failed (non-fatal)");
            }
        });
    }

    /// Returns `true` if a cached artifact exists and has not yet exceeded its TTL.
    pub(super) async fn artifact_is_fresh(
        &self,
        artifact_key: &str,
        artifact_ttl: Option<std::time::Duration>,
        registry_name: &str,
    ) -> Result<bool, CoreError> {
        if !self.storage.exists(artifact_key).await? {
            return Ok(false);
        }
        let Some(ttl) = artifact_ttl else {
            return Ok(true);
        };
        match chrono::Duration::from_std(ttl) {
            Ok(d) => {
                let expired = self
                    .artifact_meta
                    .is_artifact_expired(artifact_key, Utc::now() - d)
                    .await?;
                Ok(!expired)
            }
            Err(e) => {
                tracing::warn!(registry = %registry_name, error = %e, "artifact_ttl overflows chrono::Duration; treating artifact as fresh");
                Ok(true)
            }
        }
    }

    /// Reads a freshly cached artifact **once** and fans the result out to
    /// every consumer that wants something out of it (non-blocking, non-fatal).
    ///
    /// Two consumers today: SBOM generation, which needs the dependency manifest
    /// and the licence, and README capture, which needs a file in the same
    /// archive. RFC 0004-bis §13.1 made dependencies and the licence come back
    /// from one decompression because they live in the same file; the README
    /// lives in the same file again, and a second pass for it would repeat the
    /// waste that fixed (RFC 0007 §5.2).
    ///
    /// The bytes are re-read from storage **inside the spawned task** rather than
    /// passed in, so the request hot path never holds the full artifact in
    /// memory — the buffering cost is paid only when a consumer is enabled for
    /// the registry, and off the critical path. Each consumer's failure is its
    /// own and logs its own line: a README that could not be stored must not
    /// cost the SBOM, or the other way round.
    pub(super) async fn maybe_introspect_artifact(
        &self,
        registry_name: &str,
        artifact_key: &str,
        metadata: &crate::entities::PackageMetadata,
        registry_type: &str,
    ) {
        let (sbom_cfg, readme_cfg) = {
            let hot = self.hot.read().await;
            (
                hot.sbom.get(registry_name).cloned(),
                hot.readme.get(registry_name).cloned(),
            )
        };
        let sbom_job = self
            .sbom
            .as_ref()
            .zip(sbom_cfg.filter(|c| c.enabled))
            .map(|(svc, cfg)| (Arc::clone(svc), cfg));
        // Absent config means enabled — the opposite of SBOM — so a registry
        // with no entry still captures, and `from_archive` is what decides
        // whether the archive is opened for it.
        //
        // Two further gates, and both are about the *read* rather than about the
        // capture. `ReadmeConfig::default()` is `enabled: true, from_archive:
        // true`, and `build_readme_map` writes an entry for every registry, so
        // without them this job is `Some` for every registry on the instance and
        // the early return below never fires: every artifact newly written to
        // the cache is pulled straight back out of storage and buffered whole,
        // by a detached task with no concurrency bound, for a consumer that
        // cannot use it. A `generic` registry proxying a 1.5 GB image was paying
        // a full extra copy of it in RAM per download and discarding the lot.
        //
        // - `extractor` is what `capture_readme_from_archive` needs and it comes
        //   off the *SBOM* service, so an instance with README on and SBOM off
        //   read the bytes only to find it had nothing to read them with;
        // - `reads_the_archive()` is the kind's own answer to "is the README in
        //   the artifact at all". For `MetadataLinked` and the kinds with no
        //   README at all, opening the archive can only ever find nothing.
        //
        // A `registry_type` that is not a `RegistryKind` is not a case any
        // configured registry reaches — `validate()` parses every one at load —
        // so the permissive reading there costs nothing real and keeps a test
        // double's made-up type behaving as it did.
        let archive_can_hold_a_readme = registry_type
            .parse::<crate::entities::RegistryKind>()
            .map(|kind| kind.readme_support().reads_the_archive())
            .unwrap_or(true);
        let have_extractor = self.sbom.as_ref().is_some_and(|s| s.extractor.is_some());
        let readme_job = self
            .readme
            .as_ref()
            .filter(|_| archive_can_hold_a_readme && have_extractor)
            .map(|svc| (Arc::clone(svc), readme_cfg.unwrap_or_default()))
            .filter(|(_, cfg)| cfg.enabled && cfg.from_archive);

        // The early return is now "**no** consumer wants the bytes", not "SBOM
        // is off": with SBOM disabled and README-from-archive on, the read still
        // has to happen.
        if sbom_job.is_none() && readme_job.is_none() {
            return;
        }

        let storage = Arc::clone(&self.storage);
        let meta_clone = metadata.clone();
        let key_clone = artifact_key.to_owned();
        let registry_type = registry_type.to_owned();
        let extractor = self.sbom.as_ref().and_then(|s| s.extractor.clone());
        tokio::spawn(async move {
            // Pull the just-stored bytes back from storage. One read, one
            // decompression, however many consumers.
            let Some(data) = read_cached_artifact(&storage, &key_clone).await else {
                return;
            };

            if let Some((readme_svc, cfg)) = readme_job {
                capture_readme_from_archive(ArchiveReadme {
                    svc: &readme_svc,
                    cfg: &cfg,
                    extractor: extractor.as_ref(),
                    data: &data,
                    metadata: &meta_clone,
                    registry_type: &registry_type,
                    key: &key_clone,
                })
                .await;
            }

            if let Some((sbom, cfg)) = sbom_job {
                generate_sbom(&sbom, &cfg, &meta_clone, &key_clone, &data, &registry_type).await;
            }
        });
    }
}

/// Read a just-stored artifact back out of storage and buffer it.
///
/// `None` is never fatal: every consumer of these bytes is best-effort, and each
/// failure mode logs the line that says which one it was. Split out of
/// [`ProxyService::maybe_introspect_artifact`] so the spawned task reads as the
/// three steps it is — read, README, SBOM — rather than a match nested inside a
/// match inside an async block.
async fn read_cached_artifact(
    storage: &Arc<dyn crate::ports::StorageBackend>,
    key: &str,
) -> Option<bytes::Bytes> {
    let artifact = match storage.retrieve(key).await {
        Ok(Some(artifact)) => artifact,
        Ok(None) => {
            tracing::warn!(key = %key, "introspection: cached artifact vanished before it could be read (non-fatal)");
            return None;
        }
        Err(e) => {
            tracing::warn!(key = %key, error = %e, "introspection: storage retrieve failed (non-fatal)");
            return None;
        }
    };
    match crate::ports::collect_byte_stream(artifact.stream).await {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!(key = %key, error = %e, "introspection: failed to read cached artifact (non-fatal)");
            None
        }
    }
}

/// What [`capture_readme_from_archive`] needs, as one argument.
///
/// Seven borrows threaded from the spawned task into one call; a struct rather
/// than a seven-parameter signature so the call site names each one.
struct ArchiveReadme<'a> {
    svc: &'a crate::services::readme::ReadmeService,
    cfg: &'a crate::services::hot_config::ReadmeConfig,
    extractor: Option<&'a Arc<dyn crate::ports::SbomExtractor>>,
    data: &'a bytes::Bytes,
    metadata: &'a crate::entities::PackageMetadata,
    registry_type: &'a str,
    key: &'a str,
}

/// Record the README found in the artifact, if there is one. Non-fatal.
///
/// The README comes out of the same `extract` call the SBOM uses, so a registry
/// with SBOM off still gets one — the extractor is a pure function over the
/// bytes and does not need the SBOM service to be enabled, only to exist.
async fn capture_readme_from_archive(job: ArchiveReadme<'_>) {
    let Some(extractor) = job.extractor else {
        return;
    };
    let Some(found) = extractor.extract(job.data, job.registry_type).readme else {
        return;
    };
    let id = &job.metadata.id;
    if let Err(e) = job
        .svc
        .record_from_archive(
            &id.registry,
            &id.name,
            &id.version,
            found.content,
            found.format,
            job.cfg,
        )
        .await
    {
        tracing::warn!(key = %job.key, error = %e, "readme: archive capture failed (non-fatal)");
    }
}

/// Generate and store the SBOM for a just-cached artifact. Non-fatal.
async fn generate_sbom(
    sbom: &crate::services::sbom::SbomService,
    cfg: &crate::services::hot_config::SbomConfig,
    metadata: &crate::entities::PackageMetadata,
    key: &str,
    data: &bytes::Bytes,
    registry_type: &str,
) {
    let formats: Vec<SbomFormat> = cfg
        .formats
        .iter()
        .filter_map(|s| SbomFormat::parse(s))
        .collect();
    if let Err(e) = sbom
        .record_for_proxied(
            metadata,
            key,
            data,
            SbomProxiedOptions {
                registry_type,
                formats: &formats,
                fetch_upstream: cfg.fetch_upstream,
            },
        )
        .await
    {
        tracing::warn!(key = %key, error = %e, "sbom generation failed (non-fatal)");
    }
}
