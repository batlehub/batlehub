use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};

use crate::entities::{ArtifactSbom, SbomFormat};
use crate::error::CoreError;

#[async_trait]
pub trait SbomRepository: Send + Sync {
    /// Store or replace an SBOM for the given artifact key and format (upsert).
    async fn upsert_sbom(&self, sbom: ArtifactSbom) -> Result<(), CoreError>;

    /// Fetch the SBOM for a specific artifact key and format.
    async fn get_sbom(
        &self,
        artifact_key: &str,
        format: &SbomFormat,
    ) -> Result<Option<ArtifactSbom>, CoreError>;

    /// Fetch the most recently recorded SBOM for a registry/package/version,
    /// regardless of the exact `artifact_key` (proxy keys carry a per-registry
    /// artifact suffix such as `/tarball` or `/dl` that callers cannot predict).
    async fn get_sbom_by_coordinates(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
        format: &SbomFormat,
    ) -> Result<Option<ArtifactSbom>, CoreError>;

    /// Which SBOM formats this instance actually holds for a
    /// registry/package/version.
    ///
    /// Asked before a download is *offered*, not after it fails. The formats a
    /// version has are decided per registry by `[registries.sbom].formats`, so a
    /// console that assumed both draws a CycloneDX button on an SPDX-only
    /// registry — a link whose only possible outcome is a `404`, after a
    /// spinner. Ordered `spdx`, `cyclonedx`; empty means none are held.
    ///
    /// The default implementation asks [`Self::get_sbom_by_coordinates`] once
    /// per format, which is correct everywhere and cheap for the in-memory
    /// backends. The Postgres repository overrides it with a single
    /// `SELECT DISTINCT format`, because this runs once per row of a version
    /// page and the default would load two whole SBOM documents to answer a
    /// question about their existence.
    async fn sbom_formats_for_coordinate(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
    ) -> Result<Vec<SbomFormat>, CoreError> {
        let mut held = Vec::new();
        for format in [SbomFormat::Spdx, SbomFormat::CycloneDx] {
            if self
                .get_sbom_by_coordinates(registry, package_name, version, &format)
                .await?
                .is_some()
            {
                held.push(format);
            }
        }
        Ok(held)
    }

    /// The licence recorded for a registry/package/version, if one is known.
    ///
    /// Separate from [`Self::get_sbom_by_coordinates`] because
    /// `LicenseGateRule` runs on every gated request and needs one string, not
    /// a whole SBOM document — and because it must not have to pick a
    /// `SbomFormat` to ask a question that has nothing to do with format.
    ///
    /// `Ok(None)` means unknown, which is not the same as unlicensed.
    async fn get_license_for_coordinate(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
    ) -> Result<Option<String>, CoreError>;

    /// List SBOMs for org-level export, optionally filtered by registry and time range.
    async fn list_sboms_for_export(
        &self,
        registry: Option<&str>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<ArtifactSbom>, CoreError>;
}

/// A single dependency parsed from a package manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SbomDependency {
    pub name: String,
    pub version_req: Option<String>,
    pub ecosystem: String,
}

/// What a package's own manifest declares, read in a single pass.
///
/// Dependencies and the licence come from the same file — `Cargo.toml`,
/// `package.json`, `pom.xml`, `METADATA`, `.nuspec` — so they are returned
/// together rather than through two trait methods that would each open and
/// decompress the archive (RFC 0004-bis §13.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedManifest {
    pub dependencies: Vec<SbomDependency>,
    /// The licence exactly as the manifest declares it, trimmed and no more.
    ///
    /// Deliberately not normalised to canonical SPDX here: `LicenseGateRule`
    /// does its own case-insensitive comparison, and rewriting `Apache License
    /// 2.0` to `Apache-2.0` at this layer would put a guess in the stored
    /// record where the operator can no longer see what the package actually
    /// said. `None` means the manifest declared nothing, which the gate treats
    /// as unknown rather than as permissive.
    pub license: Option<String>,
    /// The README the same archive carries, when this registry kind's README is
    /// archive-borne (RFC 0007 §5.2).
    ///
    /// A third fact from the same decompression, for the same reason the licence
    /// is a second one: the archive is already open, decompressed and in memory,
    /// and a feature that opened it again would repeat the waste RFC 0004-bis
    /// §13.1 removed.
    pub readme: Option<ExtractedReadme>,
}

/// A README read out of a package archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedReadme {
    pub content: String,
    pub format: crate::entities::ReadmeFormat,
    /// The archive-relative path it was read from, so an operator can check
    /// which file the panel is showing.
    pub path: String,
    /// The entry was longer than [`README_EXTRACT_CEILING`] and `content` is a
    /// prefix. The registry's own `max_bytes` is applied on top, by
    /// `ReadmeService`, which is where per-registry configuration lives.
    pub truncated: bool,
}

/// The hard ceiling on how much of an archive entry a README extractor reads.
///
/// The extractor cannot see the registry's `max_bytes`: `SbomExtractor::extract`
/// deliberately keeps its signature, because it answers for three facts and only
/// one of them is configurable. So it reads no more than any registry could ever
/// ask for — `AppConfig::validate` refuses a `readme.max_bytes` above this — and
/// the service truncates to the registry's actual cap afterwards.
///
/// A bound on **decompressed** bytes from a single entry, which is the number
/// that matters: the input is attacker-controlled and compresses well.
pub const README_EXTRACT_CEILING: usize = 4 * 1024 * 1024;

/// Registry types whose archives carry a manifest the extractor can read a
/// licence out of.
///
/// Declared here rather than in the adapter because two places need it and they
/// must not disagree: `ArchiveSbomExtractor`'s dispatch, and the config warning
/// that tells an operator their `license_gate` can never observe a licence on
/// this registry (RFC 0004-bis §13.1). A parser added to the adapter without
/// updating this list would leave operators warned about a type that now works;
/// a type added here without a parser would silence a warning that is still
/// true. `extractor/mod.rs` has the test that refuses the drift.
pub const LICENSE_EXTRACTION_TYPES: &[&str] = &["cargo", "maven", "npm", "nuget", "pypi"];

/// Registry types whose archives the extractor can read a README out of.
///
/// A different list from [`LICENSE_EXTRACTION_TYPES`], and deliberately so: a
/// kind can carry a licence in its manifest and no README in its archive
/// (maven — the POM has `<description>`, which is a sentence), or a README and
/// no machine-readable licence (composer, terraform, conda, rubygems, go).
/// Sharing one list would make each of those a lie about the other feature.
///
/// The same drift test in `extractor/mod.rs` refuses a type listed here with no
/// parser, or a parser added without a listing. `RegistryKind::readme_support()`
/// is the user-facing answer; this is the adapter-side one, and
/// `readme_support_matches_the_extractors` keeps them from disagreeing.
pub const README_EXTRACTION_TYPES: &[&str] = &[
    "cargo",
    "composer",
    "conda",
    "goproxy",
    "npm",
    "nuget",
    "pypi",
    "rubygems",
    "terraform",
];

/// Extracts dependency and licence information from a package archive.
/// Implementations live in `crates/adapters` where archive crates are available.
pub trait SbomExtractor: Send + Sync {
    /// Parse `data` (the raw artifact bytes) for the given `registry_type` and return
    /// what the embedded manifest declares, or a default (no dependencies, no
    /// licence) if the format is unrecognised or no manifest is present.
    fn extract(&self, data: &Bytes, registry_type: &str) -> ExtractedManifest;
}

/// Fetches an SBOM document from an upstream registry API.
/// Implementations live in `crates/adapters` where reqwest is available.
#[async_trait]
pub trait UpstreamSbomFetcher: Send + Sync {
    /// Attempt to fetch a pre-built SBOM document from the upstream.
    /// Returns `None` if the upstream does not provide one.
    async fn fetch(
        &self,
        registry_type: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<serde_json::Value>, CoreError>;
}
