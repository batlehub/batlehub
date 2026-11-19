use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::entities::{ArtifactSbom, PackageMetadata, SbomFormat, SbomSource};
use crate::error::CoreError;
use crate::ports::{
    ExtractedManifest, SbomDependency, SbomExtractor, SbomRepository, UpstreamSbomFetcher,
};

mod fetch;
mod generate;

pub struct SbomService {
    pub repo: Arc<dyn SbomRepository>,
    pub extractor: Option<Arc<dyn SbomExtractor>>,
    pub fetcher: Option<Arc<dyn UpstreamSbomFetcher>>,
}

/// Options for recording an SBOM when a package is published locally.
pub struct SbomPublishOptions<'a> {
    pub registry_type: &'a str,
    pub formats: &'a [SbomFormat],
    pub required: bool,
}

/// Options for recording an SBOM for a proxied (cached) artifact. Mirrors
/// [`SbomPublishOptions`]'s grouping of the non-identity parameters.
pub struct SbomProxiedOptions<'a> {
    pub registry_type: &'a str,
    pub formats: &'a [SbomFormat],
    pub fetch_upstream: bool,
}

/// Extraction decompresses a whole package: CPU-bound for seconds on a large
/// archive. Run it on the blocking pool, at most half the cores at once, so a
/// burst of cold misses cannot stall every request on the async runtime.
static EXTRACT_PERMITS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        tokio::sync::Semaphore::new((cores / 2).max(1))
    });

/// [`SbomExtractor::extract`], off the async runtime and bounded. Best-effort
/// like every caller: a panicking extractor reads as "no manifest".
pub async fn extract_off_runtime(
    extractor: &Arc<dyn SbomExtractor>,
    data: &Bytes,
    registry_type: &str,
) -> ExtractedManifest {
    let Ok(_permit) = EXTRACT_PERMITS.acquire().await else {
        return ExtractedManifest::default();
    };
    let (extractor, data, registry_type) = (
        Arc::clone(extractor),
        data.clone(),
        registry_type.to_owned(),
    );
    tokio::task::spawn_blocking(move || extractor.extract(&data, &registry_type))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "sbom: archive extraction task failed");
            ExtractedManifest::default()
        })
}

// ── SbomService implementation ────────────────────────────────────────────────

impl SbomService {
    pub fn new(
        repo: Arc<dyn SbomRepository>,
        extractor: Option<Arc<dyn SbomExtractor>>,
        fetcher: Option<Arc<dyn UpstreamSbomFetcher>>,
    ) -> Self {
        Self {
            repo,
            extractor,
            fetcher,
        }
    }

    /// Store both SPDX and CycloneDX SBOMs for a proxied artifact.
    ///
    /// `registry_type` is the adapter type string (e.g. "cargo", "npm") used for
    /// archive extraction and upstream SBOM fetching — distinct from the user-defined
    /// registry name stored in `meta.id.registry`.
    ///
    /// Priority for SPDX: upstream-fetched > archive-extracted > minimal generated.
    /// CycloneDX is always generated (minimal if no deps extracted).
    pub async fn record_for_proxied(
        &self,
        meta: &PackageMetadata,
        artifact_key: &str,
        data: &Bytes,
        opts: SbomProxiedOptions<'_>,
    ) -> Result<(), CoreError> {
        let SbomProxiedOptions {
            registry_type,
            formats,
            fetch_upstream,
        } = opts;
        // Try upstream SBOM (SPDX only — GitHub returns SPDX)
        let upstream_spdx = if fetch_upstream {
            if let Some(ref fetcher) = self.fetcher {
                fetcher
                    .fetch(registry_type, &meta.id.name, &meta.id.version)
                    .await
                    .unwrap_or(None)
            } else {
                None
            }
        } else {
            None
        };

        // Extract deps and the declared licence from the archive in one pass
        // (fallback to empty if no extractor).
        let manifest = match &self.extractor {
            Some(e) => extract_off_runtime(e, data, registry_type).await,
            None => ExtractedManifest::default(),
        };
        let deps: Vec<SbomDependency> = manifest.dependencies;

        let source = if upstream_spdx.is_some() {
            SbomSource::Upstream
        } else if !deps.is_empty() {
            SbomSource::Extracted
        } else {
            SbomSource::Generated
        };

        for format in formats {
            let document = match format {
                SbomFormat::Spdx => upstream_spdx.clone().unwrap_or_else(|| {
                    generate::generate_spdx(meta, artifact_key, &deps, manifest.license.as_deref())
                }),
                SbomFormat::CycloneDx => generate::generate_cyclonedx(
                    meta,
                    artifact_key,
                    &deps,
                    registry_type,
                    manifest.license.as_deref(),
                ),
            };

            self.repo
                .upsert_sbom(ArtifactSbom {
                    id: Uuid::new_v4(),
                    artifact_key: artifact_key.to_owned(),
                    registry: meta.id.registry.clone(),
                    package_name: meta.id.name.clone(),
                    version: meta.id.version.clone(),
                    spec_version: format.spec_version().to_owned(),
                    format: format.clone(),
                    document,
                    source: source.clone(),
                    created_at: Utc::now(),
                    license: manifest.license.clone(),
                })
                .await?;
        }

        Ok(())
    }

    /// Store SBOMs for a privately published artifact.
    ///
    /// Extracts deps from the archive. When `required` is `true` and no manifest
    /// can be found, returns `CoreError::AccessDenied` so the publish can be rolled back.
    pub async fn record_for_published(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        artifact_key: &str,
        data: &Bytes,
        opts: SbomPublishOptions<'_>,
    ) -> Result<(), CoreError> {
        let SbomPublishOptions {
            registry_type,
            formats,
            required,
        } = opts;
        let manifest = match &self.extractor {
            Some(e) => extract_off_runtime(e, data, registry_type).await,
            None => ExtractedManifest::default(),
        };
        let deps: Vec<SbomDependency> = manifest.dependencies;

        if required && deps.is_empty() {
            return Err(CoreError::AccessDenied(
                "SBOM required: no dependency manifest found in the uploaded archive".into(),
            ));
        }

        let source = if deps.is_empty() {
            SbomSource::Generated
        } else {
            SbomSource::Extracted
        };

        let fake_meta = PackageMetadata {
            id: crate::entities::PackageId::new(registry, name, version),
            published_at: Some(Utc::now()),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        };

        for format in formats {
            let document = match format {
                SbomFormat::Spdx => generate::generate_spdx(
                    &fake_meta,
                    artifact_key,
                    &deps,
                    manifest.license.as_deref(),
                ),
                SbomFormat::CycloneDx => generate::generate_cyclonedx(
                    &fake_meta,
                    artifact_key,
                    &deps,
                    registry_type,
                    manifest.license.as_deref(),
                ),
            };

            self.repo
                .upsert_sbom(ArtifactSbom {
                    id: Uuid::new_v4(),
                    artifact_key: artifact_key.to_owned(),
                    registry: registry.to_owned(),
                    package_name: name.to_owned(),
                    version: version.to_owned(),
                    spec_version: format.spec_version().to_owned(),
                    format: format.clone(),
                    document,
                    source: source.clone(),
                    created_at: Utc::now(),
                    license: manifest.license.clone(),
                })
                .await?;
        }

        Ok(())
    }

    pub async fn get_artifact_sbom(
        &self,
        artifact_key: &str,
        format: &SbomFormat,
    ) -> Result<Option<ArtifactSbom>, CoreError> {
        self.repo.get_sbom(artifact_key, format).await
    }

    /// Fetch the SBOM for a registry/package/version regardless of the
    /// per-registry artifact suffix (`/tarball`, `/dl`, …) baked into the
    /// stored `artifact_key`.
    pub async fn get_artifact_sbom_by_coordinates(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
        format: &SbomFormat,
    ) -> Result<Option<ArtifactSbom>, CoreError> {
        self.repo
            .get_sbom_by_coordinates(registry, package_name, version, format)
            .await
    }

    /// Which SBOM formats are actually available for a coordinate.
    ///
    /// The question a page asks before it offers a download, rather than the
    /// one it asks by trying: see
    /// [`SbomRepository::sbom_formats_for_coordinate`].
    pub async fn formats_for_coordinate(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
    ) -> Result<Vec<SbomFormat>, CoreError> {
        self.repo
            .sbom_formats_for_coordinate(registry, package_name, version)
            .await
    }

    /// Merge all SBOMs in the given time range into a single org-level document.
    pub async fn export_org_sbom(
        &self,
        registry: Option<&str>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        format: &SbomFormat,
    ) -> Result<serde_json::Value, CoreError> {
        let sboms = fetch::collect_all_sbom_pages(&*self.repo, registry, from, to).await?;
        match format {
            SbomFormat::Spdx => {
                let (packages, relationships) = fetch::collect_spdx_entries(&sboms);
                Ok(generate::build_spdx_document(packages, relationships))
            }
            SbomFormat::CycloneDx => {
                let components = fetch::collect_cyclonedx_components(&sboms);
                Ok(generate::build_cyclonedx_document(components))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::entities::PackageId;
    use crate::ports::ExtractedManifest;

    struct PanickingExtractor;
    impl SbomExtractor for PanickingExtractor {
        fn extract(&self, _: &Bytes, _: &str) -> ExtractedManifest {
            panic!("hostile archive")
        }
    }

    #[tokio::test]
    async fn extraction_runs_off_the_runtime_and_a_panic_reads_as_no_manifest() {
        let e: Arc<dyn SbomExtractor> = Arc::new(PanickingExtractor);
        let got = extract_off_runtime(&e, &Bytes::from_static(b"x"), "npm").await;
        assert_eq!(got, ExtractedManifest::default());
        // The permit came back: a second call does not hang.
        let again = extract_off_runtime(&e, &Bytes::from_static(b"x"), "npm").await;
        assert_eq!(again, ExtractedManifest::default());
    }

    struct InMemorySbomRepo {
        items: Mutex<Vec<ArtifactSbom>>,
    }

    impl InMemorySbomRepo {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                items: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl SbomRepository for InMemorySbomRepo {
        async fn upsert_sbom(&self, sbom: ArtifactSbom) -> Result<(), CoreError> {
            self.items.lock().unwrap().push(sbom);
            Ok(())
        }

        async fn get_sbom(
            &self,
            artifact_key: &str,
            format: &SbomFormat,
        ) -> Result<Option<ArtifactSbom>, CoreError> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.artifact_key == artifact_key && &s.format == format)
                .cloned())
        }

        async fn get_sbom_by_coordinates(
            &self,
            registry: &str,
            package_name: &str,
            version: &str,
            format: &SbomFormat,
        ) -> Result<Option<ArtifactSbom>, CoreError> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|s| {
                    s.registry == registry
                        && s.package_name == package_name
                        && s.version == version
                        && &s.format == format
                })
                .cloned())
        }

        async fn get_license_for_coordinate(
            &self,
            registry: &str,
            package_name: &str,
            version: &str,
        ) -> Result<Option<String>, CoreError> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .filter(|s| {
                    s.registry == registry && s.package_name == package_name && s.version == version
                })
                .find_map(|s| s.license.clone()))
        }

        async fn list_sboms_for_export(
            &self,
            _registry: Option<&str>,
            _from: Option<DateTime<Utc>>,
            _to: Option<DateTime<Utc>>,
            _limit: u64,
            _offset: u64,
        ) -> Result<Vec<ArtifactSbom>, CoreError> {
            Ok(self.items.lock().unwrap().clone())
        }
    }

    struct StubExtractor {
        deps: Vec<SbomDependency>,
        license: Option<String>,
    }

    impl SbomExtractor for StubExtractor {
        fn extract(&self, _data: &Bytes, _registry_type: &str) -> ExtractedManifest {
            ExtractedManifest {
                dependencies: self.deps.clone(),
                license: self.license.clone(),
                readme: None,
            }
        }
    }

    struct StubFetcher {
        doc: Option<serde_json::Value>,
    }

    #[async_trait]
    impl UpstreamSbomFetcher for StubFetcher {
        async fn fetch(
            &self,
            _registry_type: &str,
            _name: &str,
            _version: &str,
        ) -> Result<Option<serde_json::Value>, CoreError> {
            Ok(self.doc.clone())
        }
    }

    fn make_meta(name: &str, version: &str) -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("cargo", name, version),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    fn one_dep() -> SbomDependency {
        SbomDependency {
            name: "dep-a".into(),
            version_req: Some("1.0.0".into()),
            ecosystem: "cargo".into(),
        }
    }

    #[tokio::test]
    async fn record_for_proxied_generated_when_no_extractor_no_fetch() {
        let repo = InMemorySbomRepo::new();
        let svc = SbomService::new(repo.clone(), None, None);
        let meta = make_meta("tokio", "1.0.0");

        svc.record_for_proxied(
            &meta,
            "artifact:cargo/tokio/1.0.0",
            &Bytes::new(),
            SbomProxiedOptions {
                registry_type: "cargo",
                formats: &[SbomFormat::Spdx, SbomFormat::CycloneDx],
                fetch_upstream: true,
            },
        )
        .await
        .unwrap();

        let items = repo.items.lock().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|s| s.source == SbomSource::Generated));
    }

    #[tokio::test]
    async fn record_for_proxied_extracted_when_extractor_returns_deps() {
        let repo = InMemorySbomRepo::new();
        let extractor = Arc::new(StubExtractor {
            deps: vec![one_dep()],
            license: None,
        });
        let svc = SbomService::new(repo.clone(), Some(extractor), None);
        let meta = make_meta("express", "4.0.0");

        svc.record_for_proxied(
            &meta,
            "artifact:npm/express/4.0.0",
            &Bytes::new(),
            SbomProxiedOptions {
                registry_type: "npm",
                formats: &[SbomFormat::Spdx],
                fetch_upstream: false,
            },
        )
        .await
        .unwrap();

        let items = repo.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, SbomSource::Extracted);
        assert_eq!(items[0].document["packages"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn record_for_proxied_upstream_when_fetch_succeeds() {
        let repo = InMemorySbomRepo::new();
        let upstream_doc = serde_json::json!({"spdxVersion": "SPDX-2.3", "packages": []});
        let fetcher = Arc::new(StubFetcher {
            doc: Some(upstream_doc.clone()),
        });
        let svc = SbomService::new(repo.clone(), None, Some(fetcher));
        let meta = make_meta("rails", "7.0.0");

        svc.record_for_proxied(
            &meta,
            "artifact:rubygems/rails/7.0.0",
            &Bytes::new(),
            SbomProxiedOptions {
                registry_type: "rubygems",
                formats: &[SbomFormat::Spdx, SbomFormat::CycloneDx],
                fetch_upstream: true,
            },
        )
        .await
        .unwrap();

        let items = repo.items.lock().unwrap();
        let spdx = items.iter().find(|s| s.format == SbomFormat::Spdx).unwrap();
        assert_eq!(spdx.source, SbomSource::Upstream);
        assert_eq!(spdx.document, upstream_doc);

        let cdx = items
            .iter()
            .find(|s| s.format == SbomFormat::CycloneDx)
            .unwrap();
        assert_eq!(cdx.source, SbomSource::Upstream);
    }

    #[tokio::test]
    async fn record_for_published_required_with_no_deps_returns_access_denied() {
        let repo = InMemorySbomRepo::new();
        let svc = SbomService::new(repo.clone(), None, None);

        let err = svc
            .record_for_published(
                "cargo",
                "tokio",
                "1.0.0",
                "artifact:cargo/tokio/1.0.0",
                &Bytes::new(),
                SbomPublishOptions {
                    registry_type: "cargo",
                    formats: &[SbomFormat::Spdx],
                    required: true,
                },
            )
            .await
            .unwrap_err();

        assert!(matches!(err, CoreError::AccessDenied(_)));
        assert!(repo.items.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn record_for_published_extracted_success() {
        let repo = InMemorySbomRepo::new();
        let extractor = Arc::new(StubExtractor {
            deps: vec![one_dep()],
            license: None,
        });
        let svc = SbomService::new(repo.clone(), Some(extractor), None);

        svc.record_for_published(
            "cargo",
            "myapp",
            "0.1.0",
            "artifact:cargo/myapp/0.1.0",
            &Bytes::new(),
            SbomPublishOptions {
                registry_type: "cargo",
                formats: &[SbomFormat::Spdx, SbomFormat::CycloneDx],
                required: true,
            },
        )
        .await
        .unwrap();

        let items = repo.items.lock().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|s| s.source == SbomSource::Extracted));
        assert!(items.iter().all(|s| s.registry == "cargo"));
        assert!(items.iter().all(|s| s.package_name == "myapp"));
    }

    #[tokio::test]
    async fn export_org_sbom_spdx_and_cyclonedx() {
        let repo = InMemorySbomRepo::new();
        let svc = SbomService::new(repo.clone(), None, None);
        let meta = make_meta("tokio", "1.0.0");

        svc.record_for_proxied(
            &meta,
            "artifact:cargo/tokio/1.0.0",
            &Bytes::new(),
            SbomProxiedOptions {
                registry_type: "cargo",
                formats: &[SbomFormat::Spdx, SbomFormat::CycloneDx],
                fetch_upstream: false,
            },
        )
        .await
        .unwrap();

        let spdx = svc
            .export_org_sbom(None, None, None, &SbomFormat::Spdx)
            .await
            .unwrap();
        assert_eq!(spdx["spdxVersion"], "SPDX-2.3");
        assert_eq!(spdx["packages"].as_array().unwrap().len(), 1);

        let cdx = svc
            .export_org_sbom(None, None, None, &SbomFormat::CycloneDx)
            .await
            .unwrap();
        assert_eq!(cdx["bomFormat"], "CycloneDX");
        assert_eq!(cdx["components"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_artifact_sbom_returns_stored_value() {
        let repo = InMemorySbomRepo::new();
        let svc = SbomService::new(repo.clone(), None, None);
        let meta = make_meta("tokio", "1.0.0");

        svc.record_for_proxied(
            &meta,
            "artifact:cargo/tokio/1.0.0",
            &Bytes::new(),
            SbomProxiedOptions {
                registry_type: "cargo",
                formats: &[SbomFormat::Spdx],
                fetch_upstream: false,
            },
        )
        .await
        .unwrap();

        let found = svc
            .get_artifact_sbom("artifact:cargo/tokio/1.0.0", &SbomFormat::Spdx)
            .await
            .unwrap();
        assert!(found.is_some());

        let missing = svc
            .get_artifact_sbom("artifact:cargo/other/1.0.0", &SbomFormat::Spdx)
            .await
            .unwrap();
        assert!(missing.is_none());
    }
}
