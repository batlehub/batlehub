use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// Uniquely identifies a package (or sub-artifact) in a registry.
///
/// Examples:
/// - GitHub release asset: `{ registry: "github", name: "rust-lang/rust", version: "v1.80.0", artifact: Some("12345678") }`
/// - Cargo crate:          `{ registry: "cargo",  name: "tokio",           version: "1.38.0",  artifact: None }`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct PackageId {
    pub registry: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
}

impl PackageId {
    pub fn new(
        registry: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            registry: registry.into(),
            name: name.into(),
            version: version.into(),
            artifact: None,
        }
    }

    pub fn with_artifact(mut self, artifact: impl Into<String>) -> Self {
        self.artifact = Some(artifact.into());
        self
    }

    /// Whether this coordinate names a **checksum or signature file** that
    /// accompanies an artifact, rather than the artifact itself.
    ///
    /// Ecosystems that publish a version as several files make the client fetch
    /// all of them for one logical install: resolving a single Maven dependency
    /// pulls `.jar`, `.pom` and a `.sha1` beside each, and `mvn` will take
    /// `.md5` and `.asc` too where they exist. Counting each as a download
    /// reports one `mvn install` as four to six, which is what
    /// `package_statuses.access_count`, `/api/v1/me/downloads` and the console's
    /// popularity ordering were all reading.
    ///
    /// A sidecar fetch is still recorded — it is real access to a real file, and
    /// dropping it would put a hole in the audit trail — but as
    /// [`AccessAction::ViewMetadata`], which is what it is: a client verifying
    /// the artifact it is about to use.
    ///
    /// Suffix-based rather than a per-ecosystem list, because the suffixes are
    /// the convention every one of them borrowed. `.pom` is deliberately **not**
    /// here: a `pom`-packaged Maven module is nothing but its POM, so treating
    /// it as metadata would make such a module permanently show zero downloads.
    ///
    /// One exact name is admitted alongside the suffixes, and only because the
    /// suffix rule cannot reach it: Terraform's checksum manifest is addressed as
    /// the bare artifact `shasums` (`SHA256SUMS`), while the detached signature
    /// beside it is `shasums.sig` and already matches `.sig`. Leaving it out
    /// counted one `terraform init` as two downloads and let the two halves of
    /// the same verification step disagree — which is the defect this predicate
    /// exists to remove, not an exception to it.
    ///
    /// [`AccessAction::ViewMetadata`]: crate::entities::AccessAction::ViewMetadata
    pub fn is_verification_sidecar(&self) -> bool {
        const SIDECAR_SUFFIXES: &[&str] = &[
            ".sha1",
            ".sha256",
            ".sha512",
            ".md5",
            ".asc",
            ".sig",
            ".sigstore",
            // A VSIX's signature archive and the key it verifies under
            // (RFC 0020): what an editor fetches beside the package.
            ".sigzip",
            ".pubkey",
        ];
        const SIDECAR_NAMES: &[&str] = &["shasums"];
        self.artifact.as_deref().is_some_and(|artifact| {
            let lower = artifact.to_ascii_lowercase();
            SIDECAR_NAMES.contains(&lower.as_str())
                || SIDECAR_SUFFIXES.iter().any(|s| lower.ends_with(s))
        })
    }

    /// Stable string key suitable for use as a cache or storage key.
    pub fn cache_key(&self) -> String {
        match &self.artifact {
            Some(art) => format!("{}/{}/{}/{}", self.registry, self.name, self.version, art),
            None => format!("{}/{}/{}", self.registry, self.name, self.version),
        }
    }
}

impl std::fmt::Display for PackageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cache_key())
    }
}

/// Metadata fetched from an upstream registry about a package/release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub id: PackageId,
    pub published_at: Option<DateTime<Utc>>,
    /// Direct URL to download the artifact (if applicable).
    pub download_url: Option<String>,
    /// Content hash of the artifact (SHA-256, hex-encoded).
    pub checksum: Option<String>,
    /// Whether this artifact has a detached signature (.asc / .sig).
    pub is_signed: Option<bool>,
    /// Registry-specific extra fields (e.g., GitHub release body, Cargo license).
    pub extra: Value,
    /// Raw `Cache-Control` header value from the upstream metadata response, if any.
    /// Used to apply `no-store`, `no-cache`, or `max-age` directives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<String>,
}

impl PackageMetadata {
    /// `PackageMetadata` with `extra` set and every other field defaulted to
    /// `None`. Shared by registry clients (forgejo/github/gitlab) whose
    /// `resolve_metadata` only knows the package coordinate, not upstream
    /// timestamps/checksums/signature status.
    pub fn minimal(id: PackageId, extra: Value) -> Self {
        Self {
            id,
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra,
            cache_control: None,
        }
    }
}

/// Administrative status of a package in this proxy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PackageStatus {
    Available,
    Blocked {
        reason: String,
        blocked_by: String,
        blocked_at: DateTime<Utc>,
    },
}

impl PackageStatus {
    pub fn is_blocked(&self) -> bool {
        matches!(self, PackageStatus::Blocked { .. })
    }
}

/// Lightweight summary used in listing endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PackageSummary {
    pub id: Uuid,
    pub package_id: PackageId,
    pub status: PackageStatus,
    pub last_accessed: Option<DateTime<Utc>>,
    /// User who last successfully downloaded this package. `None` means anonymous.
    pub last_accessed_by: Option<String>,
    pub access_count: u64,
}

/// Filter for listing packages.
#[derive(Debug, Clone, Default)]
pub struct PackageFilter {
    /// Single-registry filter. Mutually exclusive with `registries`.
    pub registry: Option<String>,
    /// Multi-registry allow-list. Empty means "all registries". Ignored when `registry` is set.
    ///
    /// **Empty means *all*, never *none*.** Every implementation reads it that
    /// way — the Postgres adapter binds `NULL` through `prepare_registries_param`
    /// and tests `$n::text[] IS NULL OR ps.registry = ANY($n)`, the in-memory
    /// repository tests `filter.registries.is_empty() || contains(…)`.
    ///
    /// So a caller that derives this list from a caller's *permissions* must
    /// reject an empty result before it gets here, or it asks for "everything"
    /// on behalf of someone entitled to nothing. Both endpoints that do such a
    /// derivation guard for it explicitly — `front_office/packages.rs` and
    /// `front_office/explore/list.rs` — and both guards exist because the
    /// second one was written after the first had shipped without it.
    pub registries: Vec<String>,
    pub name_contains: Option<String>,
    /// Exact match on `package_name` — takes priority over `name_contains`.
    pub name_exact: Option<String>,
    pub blocked_only: bool,
    pub limit: u64,
    pub offset: u64,
}

impl PackageFilter {
    pub fn new() -> Self {
        Self {
            limit: 50,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_without_artifact() {
        let id = PackageId::new("cargo", "tokio", "1.0.0");
        assert_eq!(id.cache_key(), "cargo/tokio/1.0.0");
    }

    #[test]
    fn cache_key_with_artifact() {
        let id = PackageId::new("github", "rust-lang/rust", "v1.80.0").with_artifact("12345678");
        assert_eq!(id.cache_key(), "github/rust-lang/rust/v1.80.0/12345678");
    }

    #[test]
    fn display_equals_cache_key() {
        let id = PackageId::new("npm", "lodash", "4.17.21");
        assert_eq!(id.to_string(), id.cache_key());
    }

    #[test]
    fn with_artifact_sets_artifact_field() {
        let id = PackageId::new("cargo", "serde", "1.0.0").with_artifact("serde-1.0.0.crate");
        assert_eq!(id.artifact.as_deref(), Some("serde-1.0.0.crate"));
    }

    fn maven(artifact: &str) -> PackageId {
        PackageId::new("maven1", "com.acme:lib", "1.2.3").with_artifact(artifact)
    }

    /// The files `mvn` fetches beside a jar for one dependency. Counting these
    /// as downloads reported a single resolution as four to six.
    #[test]
    fn checksums_and_signatures_are_sidecars() {
        for artifact in [
            "lib-1.2.3.jar.sha1",
            "lib-1.2.3.jar.md5",
            "lib-1.2.3.jar.sha256",
            "lib-1.2.3.jar.sha512",
            "lib-1.2.3.jar.asc",
            "lib-1.2.3.jar.sig",
        ] {
            assert!(maven(artifact).is_verification_sidecar(), "{artifact}");
        }
    }

    /// Terraform's two verification files must agree: `shasums.sig` matched the
    /// `.sig` suffix while `shasums` — the `SHA256SUMS` manifest, addressed as a
    /// bare artifact name — did not, so one `terraform init` counted twice.
    #[test]
    fn the_terraform_checksum_manifest_and_its_signature_are_both_sidecars() {
        let tf = |artifact: &str| {
            PackageId::new("tf", "providers/acme/vault", "1.2.3").with_artifact(artifact)
        };
        assert!(tf("shasums").is_verification_sidecar());
        assert!(tf("SHASUMS").is_verification_sidecar());
        assert!(tf("shasums.sig").is_verification_sidecar());
        // The archive itself still counts as a download.
        assert!(!tf("linux/amd64").is_verification_sidecar());
    }

    /// The bytes a client actually installs are not sidecars, whatever their
    /// extension.
    #[test]
    fn the_artifact_itself_is_not_a_sidecar() {
        for artifact in [
            "lib-1.2.3.jar",
            "lib-1.2.3-sources.jar",
            "acme.crypto.2.1.0.nupkg",
            "linux/amd64",
        ] {
            assert!(!maven(artifact).is_verification_sidecar(), "{artifact}");
        }
    }

    /// A `pom`-packaged Maven module *is* its POM, so calling it metadata would
    /// leave such a module permanently reporting zero downloads.
    #[test]
    fn a_pom_is_not_a_sidecar_but_its_checksum_is() {
        assert!(!maven("lib-1.2.3.pom").is_verification_sidecar());
        assert!(maven("lib-1.2.3.pom.sha1").is_verification_sidecar());
    }

    /// A coordinate naming no file cannot be a sidecar — that is the ordinary
    /// single-file version, and it must keep counting as a download.
    #[test]
    fn a_coordinate_without_an_artifact_is_never_a_sidecar() {
        assert!(!PackageId::new("npm", "lodash", "4.17.21").is_verification_sidecar());
    }

    /// Suffix matching is case-insensitive: `.SHA1` is written by more than one
    /// publishing tool.
    #[test]
    fn the_suffix_match_ignores_case() {
        assert!(maven("lib-1.2.3.jar.SHA1").is_verification_sidecar());
        assert!(maven("lib-1.2.3.jar.Asc").is_verification_sidecar());
    }

    /// A filename that merely *contains* a suffix is not a sidecar — the match
    /// is anchored at the end.
    #[test]
    fn a_suffix_in_the_middle_of_a_name_does_not_match() {
        assert!(!maven("sha1-utils-1.0.0.jar").is_verification_sidecar());
        assert!(!maven("lib.asc.jar").is_verification_sidecar());
    }

    #[test]
    fn package_filter_new_default_limit() {
        let f = PackageFilter::new();
        assert_eq!(f.limit, 50);
        assert_eq!(f.offset, 0);
        assert!(f.registry.is_none());
        assert!(!f.blocked_only);
    }

    #[test]
    fn package_status_is_blocked() {
        use chrono::Utc;
        let blocked = PackageStatus::Blocked {
            reason: "test".into(),
            blocked_by: "admin".into(),
            blocked_at: Utc::now(),
        };
        assert!(blocked.is_blocked());
        assert!(!PackageStatus::Available.is_blocked());
    }
}
