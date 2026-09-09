//! Importing a forge release into the registry that serves it (RFC 0021).
//!
//! An artifact built by CI is attached to a forge release. RFC 0019 made that
//! asset *reachable* — it proxies, caches, audits and scans like any other
//! download — and it is still invisible to the client that wants it, because an
//! editor reads a gallery and `pip` reads a simple index, and both are served by
//! a registry of the ecosystem's own kind that knows nothing about the release.
//!
//! This service is the bridge, and the shape of it is one decision: **an import
//! is a publish**, not a cache write (RFC 0021 §5.1). Warming writes bytes under
//! a coordinate; a gallery entry needs the *version* to exist, with its index
//! metadata, its visibility, its quota accounting and its signature. Those are
//! [`LocalRegistryService::publish`]'s job, so every byte an import brings in
//! enters through the same call the HTTP publish routes use — which is what
//! makes a gate added to publishing a gate on importing, with no second site to
//! remember.

mod coordinates;
mod principal;
mod run;

#[cfg(test)]
mod tests;

pub use coordinates::{coordinate_from_filename, FilenameCoordinate, FilenameCoordinates};
pub use principal::{ImportPrincipal, CONFIG_GROUP_PREFIX};

use std::sync::Arc;

use crate::ports::RegistryClient;
use crate::services::LocalRegistryService;

/// Which releases of a repository an import considers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseSelector {
    /// The newest release that is neither a draft nor a pre-release — what a
    /// release page shows by default (RFC 0021 §11 q5).
    Latest,
    /// Every published release. Pre-releases included, drafts never.
    All,
    /// One release, by tag. The only way to reach a pre-release deliberately.
    Tag(String),
}

impl ReleaseSelector {
    /// `"latest"`, `"all"`, or a tag — the config spelling.
    pub fn parse(s: &str) -> Self {
        match s {
            "latest" => Self::Latest,
            "all" => Self::All,
            tag => Self::Tag(tag.to_owned()),
        }
    }
}

/// One asset that did not import, and why.
///
/// Named rather than counted, for the reason [`WarmFailure`] is: an operator
/// importing eleven assets who reads `errors: 3` has no way to learn *which*
/// three without shell access to the instance they are administering through a
/// console.
///
/// [`WarmFailure`]: crate::services::warming::WarmFailure
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    /// The release tag the asset was attached to.
    pub tag: String,
    /// The asset's file name.
    pub asset: String,
    pub error: String,
}

/// What one import run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Assets fetched and published.
    pub imported: usize,
    /// Assets whose version the target already holds. Counted, not an error:
    /// this is what makes a scheduled import free to re-run.
    pub skipped: usize,
    /// Assets that matched a glob and did not publish.
    pub errors: usize,
    /// One entry per counted error. `errors` stays the authority on the count.
    pub failures: Vec<ImportFailure>,
}

impl ImportReport {
    pub(crate) fn failed(
        tag: impl Into<String>,
        asset: impl Into<String>,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            errors: 1,
            failures: vec![ImportFailure {
                tag: tag.into(),
                asset: asset.into(),
                error: error.to_string(),
            }],
            ..Default::default()
        }
    }
}

impl std::ops::AddAssign for ImportReport {
    fn add_assign(&mut self, mut other: Self) {
        self.imported += other.imported;
        self.skipped += other.skipped;
        self.errors += other.errors;
        self.failures.append(&mut other.failures);
    }
}

/// Reads the coordinate an artifact publishes under.
///
/// A port because the rule is per ecosystem and two of them read the *bytes*:
/// a VSIX names its own `publisher.name` and version in its manifest, which is
/// the same thing `POST /api/-/publish` already reads. An asset whose
/// coordinate cannot be read is skipped with a named failure, never guessed at
/// (RFC 0021 §4.2).
pub trait CoordinateReader: Send + Sync {
    /// `(name, version)`, or `None` when this artifact does not name itself.
    fn read(&self, filename: &str, bytes: &[u8]) -> Option<(String, String)>;

    /// `(name, version)` from the file name alone, when the ecosystem's
    /// coordinate is in it.
    ///
    /// The point is the *fetch it saves*: an import whose every asset is
    /// already held then costs one API call and downloads nothing, which is
    /// what makes an interval affordable. `None` — the default — means this
    /// artifact names itself inside the archive, and the held check waits for
    /// the bytes.
    fn read_from_name(&self, _filename: &str) -> Option<(String, String)> {
        None
    }

    /// The `index_metadata` the target registry renders its listing from.
    ///
    /// Defaulted to the bare coordinate, which is what the publish routes fall
    /// back to when a manifest will not parse.
    fn index_metadata(&self, name: &str, version: &str, _bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({ "id": name, "version": version })
    }
}

/// What runs on the published bytes once the version exists.
///
/// The signature, today: `[registries.vsx_signing]` signs a VSIX at publish,
/// and that code reads the gallery's storage layout, which is a `crates/web`
/// concern. Rather than move it, an import runs the same hook the publish route
/// runs — so a gallery entry an import created is signed exactly as one a `PUT`
/// created, by the same call.
#[async_trait::async_trait]
pub trait PostPublish: Send + Sync {
    async fn after_publish(&self, registry: &str, name: &str, version: &str, artifact: &[u8]);
}

/// One configured import: a repository's assets into one registry.
pub struct ReleaseImportService {
    /// Where the version is published. The *target* registry's service.
    pub local: Arc<LocalRegistryService>,
    /// The source registry's client: the release listing *and* the asset bytes
    /// come through it, so the fetch keeps that registry's credential,
    /// allowlist and SSRF guard rather than growing this service a second way
    /// out (RFC 0021 §7). Its `releases()` is `Some` for the three forge kinds
    /// and `None` for everything else, which `AppConfig::validate()` has
    /// already refused.
    pub client: Arc<dyn RegistryClient>,
    /// How the artifact names itself.
    pub coordinates: Arc<dyn CoordinateReader>,
    /// The registry published into.
    pub into: String,
    /// The registry fetched from — the coordinate's registry, not the target's.
    pub from: String,
    /// `owner/repo` on the source forge.
    pub repo: String,
    /// Asset globs. Never empty: "every asset" is not what an operator means,
    /// and a release's checksums would be published as packages.
    pub assets: Vec<String>,
    pub select: ReleaseSelector,
    /// Who the publish is, and answers as.
    pub principal: ImportPrincipal,
    /// Run after each successful publish. `None` in a deployment with no
    /// signing key, and in the tests that do not care.
    pub after_publish: Option<Arc<dyn PostPublish>>,
}

impl crate::entities::ImportRun {
    /// One finished run, from the report it produced.
    ///
    /// Here rather than beside the entity so it can take the [`ImportReport`]
    /// itself: passing the three counts and the failure list separately was
    /// eight parameters, and four of them were one value that had been taken
    /// apart at the call site and put back together here.
    ///
    /// A constructor rather than each caller building the struct, because the
    /// two callers — the HTTP handler and the scheduled task — must agree on
    /// what a run row means. They already disagreed once about whether a run
    /// happened at all: only one of them recorded anything.
    pub fn from_report(
        registry: impl Into<String>,
        repo: impl Into<String>,
        started_at: chrono::DateTime<chrono::Utc>,
        report: &ImportReport,
        triggered_by: Option<String>,
    ) -> Self {
        // One line per failure, `tag asset: error`. Plain text rather than JSON
        // because the console renders it and an operator reads it; the
        // structured form already travels on the HTTP response.
        let failures = (!report.failures.is_empty()).then(|| {
            report
                .failures
                .iter()
                .map(|f| format!("{} {}: {}", f.tag, f.asset, f.error))
                .collect::<Vec<_>>()
                .join("\n")
        });
        Self {
            id: uuid::Uuid::new_v4(),
            registry: registry.into(),
            repo: repo.into(),
            started_at,
            finished_at: chrono::Utc::now(),
            imported: report.imported as u64,
            skipped: report.skipped as u64,
            errors: report.errors as u64,
            triggered_by,
            failures,
        }
    }
}
