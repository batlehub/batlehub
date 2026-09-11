mod eco_composer;
mod eco_conda;
mod eco_go;
mod eco_jetbrains;
mod eco_maven;
mod eco_nuget;
mod eco_openvsx;
mod eco_pypi;
mod eco_rubygems;
mod eco_terraform;
mod lifecycle;
mod publish;
mod read;

pub use eco_composer::COMPOSER_DIST_SHA1;
pub use eco_jetbrains::{build_in_range, JetbrainsPluginVersion};
pub use eco_openvsx::OpenVsxExtensionVersion;
pub use publish::PublishPolicyRequest;

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;

use crate::{
    entities::{
        AccessAction, AccessEvent, AccessResult, Action, Identity, PackageId, PublishedPackage,
        ReadmeFormat, SbomFormat, Visibility,
    },
    error::CoreError,
    ports::{
        LocalRegistryBackend, OwnershipPort, PackageRepository, StorageBackend, StorageMeta,
        TeamNamespacePort,
    },
    services::{
        explore_cache::ExploreCache,
        hot_config::{HotConfigLock, VersioningPolicy},
        quota::{QuotaCheck, QuotaService},
        readme::ReadmeService,
        sbom::{SbomPublishOptions, SbomService},
    },
};

// `VersioningPolicy` and `SigningConfig` are defined in hot_config and re-exported from services.

pub(super) fn validate_version(version: &str, policy: &VersioningPolicy) -> Result<(), CoreError> {
    if policy.enforce_semver {
        match semver::Version::parse(version) {
            Err(_) => {
                return Err(CoreError::InvalidVersion(format!(
                    "version '{version}' is not valid semver"
                )));
            }
            Ok(sv) if !policy.allow_prerelease && !sv.pre.is_empty() => {
                return Err(CoreError::InvalidVersion(format!(
                    "pre-release versions are not allowed (got '{version}')"
                )));
            }
            Ok(_) => {}
        }
    }
    if let Some(ref re) = policy.version_pattern {
        if !re.is_match(version) {
            return Err(CoreError::InvalidVersion(format!(
                "version '{version}' does not match required pattern '{}'",
                re.as_str()
            )));
        }
    }
    Ok(())
}

/// Reject a package coordinate component (name or version) that would let a
/// publish escape the storage root once interpolated into a storage key
/// (`{registry}/{name}/{version}`).
///
/// Package names legitimately contain `/` (npm scopes like `@scope/name`,
/// GitHub `owner/repo`), so interior `/` is allowed — but a `..` path segment,
/// an absolute path, a backslash, or a NUL byte are rejected. This runs
/// unconditionally at publish time, independent of the (optional) versioning
/// policy, and complements the storage-backend chokepoint that guards reads.
pub fn validate_path_safe(kind: &str, value: &str) -> Result<(), CoreError> {
    if value.is_empty() {
        return Err(CoreError::InvalidInput(format!("{kind} must not be empty")));
    }
    if value.starts_with('/') || value.ends_with('/') {
        return Err(CoreError::InvalidInput(format!(
            "{kind} '{value}' must not start or end with '/'"
        )));
    }
    // Check the literal form *and* every successive percent-decoding of it.
    //
    // Comparing raw bytes alone is not enough. `actix-router` decodes a path
    // parameter twice with two different protected sets — `Quoter::new(b"",
    // b"%/+")` while matching the route (so `%25` survives) and then
    // `Quoter::new(b"", b"")` in the `web::Path` extractor (which decodes it) —
    // so a request carrying `%252e%252e` reaches the handler as the literal
    // `%2e%2e`. That is not a `..` segment to `split('/')`, but `url::Url::parse`
    // *does* treat it as a dot segment and pops a path component, so a value
    // that passed this check still escaped its base once interpolated into an
    // upstream URL. Decoding to a fixed point closes the gap here, at the one
    // chokepoint every adapter funnels through, rather than per-adapter.
    check_decoded_rounds(kind, value)
}

/// Run [`reject_traversal`] over `value` and every percent-decoding of it.
fn check_decoded_rounds(kind: &str, value: &str) -> Result<(), CoreError> {
    let mut current = value.to_owned();
    for _ in 0..MAX_PERCENT_DECODE_ROUNDS {
        reject_traversal(kind, value, &current)?;
        match percent_decode_round(&current) {
            Some(next) => current = next,
            // Fixed point: nothing further decodes, the value is fully checked.
            None => return Ok(()),
        }
    }
    // Still decoding after this many rounds: no real coordinate is nested
    // percent-encoding that deeply, so treat it as an attack rather than
    // decoding forever.
    Err(CoreError::InvalidInput(format!(
        "{kind} '{value}' is excessively percent-encoded"
    )))
}

/// Whether `value` — or anything a percent-decoder could turn it into — carries
/// a `..` path segment, an empty or `.` segment (a trailing `/` excepted, so a
/// prefix passes), a backslash, or a NUL.
///
/// The predicate form of [`validate_path_safe`]'s traversal rules, for callers
/// that already have their own error type and their own notion of what the
/// value is. The storage-backend `ensure_safe_key` chokepoint uses it so the
/// edge and the last line of defence agree on what a dot segment is.
pub fn has_traversal_after_decoding(value: &str) -> bool {
    check_decoded_rounds("path", value).is_err()
}

/// How many percent-decoding rounds [`validate_path_safe`] inspects before it
/// gives up and rejects. Each round strictly shrinks the string (three bytes
/// become one), so a legitimate value reaches its fixed point almost
/// immediately; the real attack needs two.
const MAX_PERCENT_DECODE_ROUNDS: usize = 8;

/// The traversal rules, applied to one decoding round of `value`.
///
/// `value` is the caller-facing original and is used only for the error
/// message, so a rejection names what was actually sent rather than a
/// half-decoded intermediate.
fn reject_traversal(kind: &str, value: &str, decoded: &str) -> Result<(), CoreError> {
    if decoded.contains('\0') || decoded.contains('\\') {
        return Err(CoreError::InvalidInput(format!(
            "{kind} '{value}' contains an illegal character"
        )));
    }
    if decoded.split('/').any(|segment| segment == "..") {
        return Err(CoreError::InvalidInput(format!(
            "{kind} '{value}' contains a path-traversal segment"
        )));
    }
    // An empty or `.` segment stays inside the tree but does not stay *distinct*:
    // the filesystem backend joins the key verbatim, and the OS resolves
    // `a//b` and `a/./b` to the same file as `a/b`. Two coordinates, one set of
    // bytes — whichever is published last is what both serve. Found by
    // `fuzz_path_safe`, which asserts an accepted key has no empty component.
    //
    // The storage chokepoint (`has_traversal_after_decoding`, kind `path`) also
    // sees *prefixes* — `delete_by_prefix("{key}/")` — so one trailing empty
    // segment is a prefix there, not an alias. A coordinate never ends in `/`:
    // `validate_path_safe` rejects that on the raw value, and this rejects it
    // on every decoded form, so `a%2f` cannot become `a/` past the edge.
    let segments: Vec<&str> = decoded.split('/').collect();
    let (last, inner) = segments.split_last().expect("split yields one segment");
    let trailing_empty = segments.len() > 1 && last.is_empty();
    if inner.iter().any(|segment| segment.is_empty())
        || (trailing_empty && kind != "path")
        || segments.contains(&".")
    {
        return Err(CoreError::InvalidInput(format!(
            "{kind} '{value}' contains an empty or '.' path segment"
        )));
    }
    // The `version` component is a single path segment of the
    // `{registry}/{name}/{version}` storage key. An interior `/` there is never
    // legitimate (no registry uses slashes in a version) and would let two
    // distinct coordinates collapse onto the same key — e.g. name `foo` +
    // version `bar/1.0.0` produces the same key as name `foo/bar` + version
    // `1.0.0`, enabling cross-package artifact overwrite / cache poisoning. The
    // `..` check above doesn't catch that, so reject `/` outright for the
    // version — encoded as readily as literal, since either form collapses the
    // key. Names, upstream paths, and the `artifact` selector legitimately
    // contain `/` (npm scopes, `owner/repo`, git-forge `raw/{ref}/{path}` and
    // `link/{name}` selectors, mirrored file trees) and stay exempt.
    if kind == "version" && decoded.contains('/') {
        return Err(CoreError::InvalidInput(format!(
            "{kind} '{value}' must not contain '/'"
        )));
    }
    Ok(())
}

/// Percent-decode one round of `%XX` escapes, returning `None` once nothing
/// decodes (the fixed point).
///
/// A malformed escape (`%zz`, a trailing `%`) is left literal rather than
/// erroring: this is a security check, not a parser, and the question is only
/// what an upstream URL parser could still turn the value into. Decoded bytes
/// are appended raw and the result is read back lossily, so a multi-byte UTF-8
/// sequence split across escapes still reassembles.
fn percent_decode_round(value: &str) -> Option<String> {
    if !value.contains('%') {
        return None;
    }
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut decoded_any = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                decoded_any = true;
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    decoded_any.then(|| String::from_utf8_lossy(&out).into_owned())
}

/// Validate a package name is safe to use as a storage-key component.
/// Reusable by registry adapters that accept a package name from the request.
pub fn validate_package_name(name: &str) -> Result<(), CoreError> {
    validate_path_safe("package name", name)
}

/// Validate every user-controlled component of a package coordinate (`name`,
/// `version`, and optional sub-`artifact`) before it is interpolated into a
/// storage/cache key.
///
/// This is the edge counterpart to the storage-backend `ensure_safe_key`
/// chokepoint: the proxy read funnel (`ProxyService::handle`) and the local read
/// funnel (`LocalRegistryService::get_artifact`) call it so a traversal attempt is
/// rejected with a clean `400` for every registry — present and future —
/// regardless of whether the individual adapter validated its own input.
pub fn validate_coordinate(
    name: &str,
    version: &str,
    artifact: Option<&str>,
) -> Result<(), CoreError> {
    validate_path_safe("package name", name)?;
    validate_path_safe("version", version)?;
    if let Some(artifact) = artifact {
        validate_path_safe("artifact", artifact)?;
    }
    Ok(())
}

pub async fn check_team_visibility(
    ns_port: &dyn TeamNamespacePort,
    registry: &str,
    package: &str,
    identity: &Identity,
) -> Result<(), CoreError> {
    match ns_port.find_namespace(registry, package).await? {
        Some(ns)
            if identity
                .groups
                .iter()
                .any(|g| g.replace(' ', "") == ns.group_id.replace(' ', "")) =>
        {
            Ok(())
        }
        Some(ns) => Err(CoreError::AccessDenied(format!(
            "package visibility is 'team'; must be a member of group '{}'",
            ns.group_id
        ))),
        // No claim found: deny everyone. Falling back to "any authenticated user"
        // would allow non-team members to read team-private packages whenever
        // the namespace claim is missing or has been deleted.
        None => Err(CoreError::AccessDenied(
            "package visibility is 'team' but no namespace claim is configured; access denied"
                .into(),
        )),
    }
}

/// Input to `LocalRegistryService::publish`.
pub struct PublishRequest {
    pub registry: String,
    pub name: String,
    pub version: String,
    /// Raw artifact bytes.
    pub artifact: Bytes,
    /// SHA-256 hex of `artifact`, computed by the caller (handler layer).
    pub checksum: String,
    /// Ecosystem-specific index metadata serialised as JSON.
    /// Cargo: serialised `CargoIndexEntry` (with `cksum` already set).
    /// npm: version metadata from the publish payload (`dist.tarball` stripped).
    /// VSIX: `{"id": "pub.name", "version": "1.0.0"}`.
    pub index_metadata: serde_json::Value,
    /// Publish the version hidden from registry-protocol listings (still
    /// downloadable by exact coordinate) — maps to `PublishedPackage::unlisted`.
    /// Used by ecosystems whose publish protocol carries a hidden flag
    /// (JetBrains Marketplace `isHidden`).
    pub unlisted: bool,
    /// Identity of the publishing user.
    pub publisher: Identity,
    /// Raw signature bytes decoded from `X-Artifact-Signature` header, if present.
    pub signature_bytes: Option<Vec<u8>>,
    /// Signature type from `X-Signature-Type` header, if present.
    pub signature_type: Option<String>,
}

/// Authoritative local-registry service: publish, yank, index, artifact retrieval.
pub struct LocalRegistryService {
    pub backend: Arc<dyn LocalRegistryBackend>,
    pub storage: Arc<dyn StorageBackend>,
    /// Hot-swappable state (versioning, signing, beta_channel, size limit).
    pub hot: HotConfigLock,
    /// Optional publish quota enforcement. When `None`, quotas are disabled.
    pub quota: Option<Arc<QuotaService>>,
    /// Optional per-package ownership enforcement. When `None`, ownership is not enforced.
    pub ownership: Option<Arc<dyn OwnershipPort>>,
    /// Optional team namespace enforcement. When `None`, namespace gating is disabled.
    pub team_namespace: Option<Arc<dyn TeamNamespacePort>>,
    /// Optional SBOM service; when `None`, SBOM generation is disabled globally.
    pub sbom: Option<Arc<SbomService>>,
    /// Optional README service; when `None`, README capture is disabled globally.
    ///
    /// Used by the publish path — a publish document carries the README text
    /// (cargo) or the packument root does (npm) — and by delete, which is the
    /// only thing that removes a stored README: the table has no foreign key,
    /// because a README outlives the bytes (RFC 0007 §5.4).
    pub readme: Option<Arc<ReadmeService>>,
    /// Optional explore cache; invalidated automatically on successful publish.
    pub explore_cache: Option<Arc<ExploreCache>>,
    /// The admin package store, serving two purposes for local/hybrid reads.
    ///
    /// 1. **Audit.** `get_artifact` records the same
    ///    `AccessEvent::allowed_download`/`denied_download` shape
    ///    `ProxyService::handle` records on the proxy-fallback path, so
    ///    Local-mode registries aren't a silent audit gap.
    /// 2. **Blocks.** Version listings ask it which versions an administrator
    ///    has blocked, so a blocked version is left out of what clients resolve
    ///    against (see [`crate::services::blocking`]).
    ///
    /// `None` disables both. That is safe rather than a silent hole for (2):
    /// this is the very store `AdminService` writes blocks to, so a deployment
    /// without it has nowhere for a block to exist in the first place.
    pub package_repo: Option<Arc<dyn PackageRepository>>,
}

/// OS/architecture pair identifying a specific Terraform provider binary.
#[derive(Debug, Clone, Copy)]
pub struct TerraformPlatform<'a> {
    pub os: &'a str,
    pub arch: &'a str,
}

/// Stable storage key for a locally published artifact.
/// Distinct from the proxy `artifact:…` namespace to avoid collisions.
pub fn artifact_storage_key(registry: &str, name: &str, version: &str) -> String {
    format!("local:{}/{}/{}", registry, name, version)
}

/// Storage key for a non-POM Maven artifact (jar, checksum, etc.).
/// Multiple artifact files can coexist under the same version.
pub fn maven_artifact_storage_key(
    registry: &str,
    name: &str,
    version: &str,
    filename: &str,
) -> String {
    format!("local:{}/{}/{}/{}", registry, name, version, filename)
}

/// Storage key for a Terraform provider platform binary.
pub fn terraform_provider_binary_storage_key(
    registry: &str,
    namespace: &str,
    ptype: &str,
    version: &str,
    os: &str,
    arch: &str,
) -> String {
    format!("local:{registry}/providers/{namespace}/{ptype}/{version}/{os}-{arch}")
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;

/// Returns `true` when `version` is a pre-release.
///
/// Re-exported from [`version_order::is_prerelease`](crate::services::version_order::is_prerelease),
/// which is where the definition lives as of RFC 0015 phase 4. It used to be
/// implemented here, on a strict semver parse whose `unwrap_or(false)` called
/// `1.0-SNAPSHOT` a release — see §4.5, and the doc comment on the function this
/// now points at.
///
/// Kept as a re-export rather than deleted because this module and its callers
/// name it in a dozen places, and the point of the convergence is that there is
/// one implementation, not that there is one path to it.
pub use crate::services::version_order::is_prerelease;
