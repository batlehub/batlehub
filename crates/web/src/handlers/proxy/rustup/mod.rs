//! The Rust toolchain tree — rustup, and mise's `rust` backend (RFC 0024).
//!
//! Six routes for the eleven URLs of §6.5, because the ones that differ only by
//! a suffix inside a single path segment are parsed rather than routed:
//! `dist/{file}` covers `channel-rust-{name}.toml`, its `.sha256`, its `.asc`,
//! the extensionless v1 name and `channel-rust-stable-date.txt`, and
//! `dist/{date}/{file}` covers the same five under a dated directory plus every
//! component tarball. One parse in one place cannot be mis-ordered the way five
//! route patterns sharing a prefix can.
//!
//! The design is in which URL is a *document*:
//!
//! - A **channel manifest** is the enforcement chokepoint: rustup resolves
//!   every install through one, so an exact name whose release is blocked
//!   answers `404` (rustup's own *"could not download nonexistent rust
//!   version"*) and an alias is repaired to the newest release it could denote.
//!   Denied components are edited out of whatever is served, the way upstream
//!   edits a component that failed to build.
//! - Its **`.sha256`** is computed over the bytes this instance serves, never
//!   relayed. rustup refuses a manifest whose digest does not match the sidecar
//!   beside it, so an edited document with upstream's sidecar installs nothing.
//! - Its **`.asc`** is relayed byte-exact and verifies only an `upstream`
//!   manifest. No rustup since 1.26.0 reads it; the header says which case this
//!   is rather than pretending.
//! - **Component tarballs** go through `proxy_stream`, cached under
//!   `rust/{version}/{file}` — a stable per-release key whatever dated
//!   directory served them.
//!
//! Every path segment reaches a storage or cache key, so each is validated here
//! for a clean `400`; `validate_coordinate` in `ProxyService::handle` and
//! `ensure_safe_key` in the storage backends remain the deeper guards.

mod render;

use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_core::{
    entities::{Action, PackageId},
    ports::DocumentKind,
    services::rustup::{coordinate_of, sidecar_line, ToolchainName, RUSTUP_PACKAGE, RUST_PACKAGE},
    services::{validate_path_safe, ProxyService},
};

use super::common::{
    fetch_proxy_document, proxy_document, proxy_stream, registry_public_base, require_registry_type,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, UpstreamMap};

/// Which of the three manifest-shaped files a `dist/` name is.
enum ManifestFile<'a> {
    /// `channel-rust-{name}.toml`
    Manifest(&'a str),
    /// `channel-rust-{name}.toml.sha256`
    Sidecar(&'a str),
    /// `channel-rust-{name}.toml.asc`
    Signature(&'a str),
}

impl<'a> ManifestFile<'a> {
    /// `None` when the file is not a channel document at all.
    fn parse(file: &'a str) -> Option<Self> {
        let rest = file.strip_prefix("channel-rust-")?;
        if let Some(name) = rest.strip_suffix(".toml.sha256") {
            return Some(Self::Sidecar(name));
        }
        if let Some(name) = rest.strip_suffix(".toml.asc") {
            return Some(Self::Signature(name));
        }
        rest.strip_suffix(".toml").map(Self::Manifest)
    }
}

const MANIFEST_HEADER: &str = "X-BatleHub-Manifest";
const VERSION_HEADER: &str = "X-BatleHub-Version";
const TOML: &str = "text/plain; charset=utf-8";

/// Serve one of the three channel documents, dated or not.
async fn channel_document(
    req: &HttpRequest,
    registry: &str,
    date: Option<&str>,
    file: &ManifestFile<'_>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    upstreams: &UpstreamMap,
) -> Result<HttpResponse, AppError> {
    let raw = match file {
        ManifestFile::Manifest(n) | ManifestFile::Sidecar(n) | ManifestFile::Signature(n) => *n,
    };
    // The segment becomes part of a cache key, so a name no release could have
    // is a `400` rather than a lookup.
    let name = ToolchainName::parse(raw).map_err(AppError::bad_request)?;
    let public_base = registry_public_base(req, registry);
    let deny = svc.deny_components(registry).await;
    let upstream = upstreams.upstream_for(registry);

    let rendered = render::render(
        &svc,
        &render::RenderRequest {
            registry,
            name: &name,
            date,
            identity: &identity,
            public_base: &public_base,
            upstream_base: upstream.as_deref(),
            deny: &deny,
        },
    )
    .await?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header((MANIFEST_HEADER, rendered.state));
    if let Some(version) = &rendered.served_version {
        builder.insert_header((VERSION_HEADER, version.clone()));
    }

    match file {
        ManifestFile::Manifest(_) => Ok(builder.content_type(TOML).body(rendered.body)),
        ManifestFile::Sidecar(_) => {
            // The name rustup asked for, not the one that was served: the
            // sidecar's second field is cosmetic, and a repaired manifest is
            // still the answer to `channel-rust-stable.toml`.
            let file_name = format!("channel-rust-{raw}.toml");
            Ok(builder
                .content_type(TOML)
                .body(sidecar_line(rendered.body.as_bytes(), &file_name)))
        }
        ManifestFile::Signature(_) => {
            // Relayed byte-exact, and only after the manifest it signs was
            // found servable — a blocked release's signature is a `404` with
            // the document it covers.
            let channel = match date {
                Some(d) => format!("{d}/{}", name.as_name()),
                None => name.as_name(),
            };
            let doc = fetch_proxy_document(
                svc.clone(),
                PackageId::new(
                    registry,
                    batlehub_core::services::rustup::listing_package(&channel),
                    "signature",
                ),
                identity,
                Action::ReleasesList,
                DocumentKind::MANIFEST_ASC,
                public_base,
            )
            .await?;
            let body = doc.body.as_text().unwrap_or_default().to_owned();
            Ok(builder.content_type("application/pgp-signature").body(body))
        }
    }
}

/// `dist/{file}`: a channel manifest, its checksum, its signature, the v1 name
/// rustup falls back to, or the stable-date file.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rustup/dist/{file}",
    tag = "proxy/rustup",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "channel-rust-{name}.toml, .toml.sha256, .toml.asc, or channel-rust-stable-date.txt"),
    ),
    responses(
        (status = 200, description = "Channel manifest, its computed checksum line, its relayed signature, or the stable-date file", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Not a rustup toolchain name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Blocked release, unknown document, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rustup/dist/{file}")]
pub async fn rustup_dist_root(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstreams: web::Data<UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file) = path.into_inner();
    require_registry_type(&registry, "rustup", &map)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    if file == "channel-rust-stable-date.txt" {
        let public_base = registry_public_base(&req, &registry);
        return proxy_document(
            svc,
            PackageId::new(&registry, RUST_PACKAGE, "stable-date"),
            identity,
            Action::ReleasesList,
            DocumentKind::STABLE_DATE,
            public_base,
        )
        .await;
    }
    let Some(parsed) = ManifestFile::parse(&file) else {
        // `channel-rust-{name}` with no extension is rustup's v1 fallback,
        // which this tree stopped publishing in 2016 and this registry never
        // serves: a `404` here is the same answer upstream gives (§3).
        return Err(AppError::not_found(format!(
            "'{file}' is not a document this registry serves"
        )));
    };
    channel_document(&req, &registry, None, &parsed, identity, svc, &upstreams).await
}

/// `dist/{date}/{file}`: the same three channel documents under a dated
/// directory, or one component archive of that release.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rustup/dist/{date}/{file}",
    tag = "proxy/rustup",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("date" = String, Path, description = "Release date directory (YYYY-MM-DD)"),
        ("file" = String, Path, description = "A channel document, or a component archive such as rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz"),
    ),
    responses(
        (status = 200, description = "Channel document, or the component archive streamed from upstream (cached), byte-exact", body = ArtifactBytes),
        (status = 400, description = "Unsafe date or file name, or a file naming no Rust release"),
        (status = 403, description = "Access denied, or the release is blocked"),
        (status = 404, description = "Not found upstream, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rustup/dist/{date}/{file}")]
pub async fn rustup_dist_dated(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstreams: web::Data<UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let (registry, date, file) = path.into_inner();
    require_registry_type(&registry, "rustup", &map)?;
    validate_path_safe("date", &date).map_err(AppError::from)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    if let Some(parsed) = ManifestFile::parse(&file) {
        return channel_document(
            &req,
            &registry,
            Some(&date),
            &parsed,
            identity,
            svc,
            &upstreams,
        )
        .await;
    }

    // A component archive. The coordinate is read off the directory and the
    // file name, with no manifest fetch, and a name that carries no Rust
    // version is a `400` rather than a guess.
    let (version, _parts) = coordinate_of(&date, &file).map_err(AppError::bad_request)?;
    let pkg = PackageId::new(&registry, RUST_PACKAGE, &version).with_artifact(&file);
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(content_type_for(&file)),
    )
    .await
}

/// `Content-Type` for a dist file. rustup reads none of these; a browser
/// saving a checksum or a signature does.
fn content_type_for(file: &str) -> &'static str {
    if file.ends_with(".sha256") {
        "text/plain; charset=utf-8"
    } else if file.ends_with(".asc") {
        "application/pgp-signature"
    } else {
        "application/octet-stream"
    }
}

/// `manifests.txt`: every manifest the release tooling published, blocked
/// releases removed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rustup/manifests.txt",
    tag = "proxy/rustup",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "One manifest path per line, blocked releases removed", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rustup/manifests.txt")]
pub async fn rustup_manifests_txt(
    req: HttpRequest,
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "rustup", &map)?;
    let public_base = registry_public_base(&req, &registry);
    proxy_document(
        svc,
        PackageId::new(&registry, RUST_PACKAGE, "manifests"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        public_base,
    )
    .await
}

/// `rustup/release-stable.toml`: the installer's own current version, relayed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rustup/rustup/release-stable.toml",
    tag = "proxy/rustup",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The installer's current version, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rustup/rustup/release-stable.toml")]
pub async fn rustup_release_stable(
    req: HttpRequest,
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "rustup", &map)?;
    let public_base = registry_public_base(&req, &registry);
    proxy_document(
        svc,
        PackageId::new(&registry, RUSTUP_PACKAGE, "release"),
        identity,
        Action::ReleasesList,
        DocumentKind::RUSTUP_RELEASE,
        public_base,
    )
    .await
}

/// `rustup/archive/{version}/{triple}/{file}`: one installer release.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rustup/rustup/archive/{version}/{triple}/{file}",
    tag = "proxy/rustup",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("version" = String, Path, description = "rustup release (e.g. 1.29.1)"),
        ("triple" = String, Path, description = "Target triple"),
        ("file" = String, Path, description = "rustup-init, rustup-init.exe, or a checksum sibling"),
    ),
    responses(
        (status = 200, description = "Installer binary streamed from upstream (cached)", body = ArtifactBytes),
        (status = 400, description = "Unsafe path segment"),
        (status = 403, description = "Access denied, or the release is blocked"),
        (status = 404, description = "Not found upstream, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rustup/rustup/archive/{version}/{triple}/{file}")]
pub async fn rustup_archive(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, version, triple, file) = path.into_inner();
    require_registry_type(&registry, "rustup", &map)?;
    validate_path_safe("version", &version).map_err(AppError::from)?;
    validate_path_safe("triple", &triple).map_err(AppError::from)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    let pkg = PackageId::new(&registry, RUSTUP_PACKAGE, &version)
        .with_artifact(format!("{triple}/{file}"));
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(content_type_for(&file)),
    )
    .await
}

/// `rustup/dist/{triple}/{file}`: what `rustup-init.sh` fetches, which names no
/// version.
///
/// The version is read from `release-stable.toml` and the archive entry is
/// served under it, so the cache holds a release rather than a moving name —
/// RFC 0019's lesson, that a cache entry is a commit and not a branch. The
/// bytes are identical upstream by construction: the release process copies the
/// archive into `dist/`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rustup/rustup/dist/{triple}/{file}",
    tag = "proxy/rustup",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("triple" = String, Path, description = "Target triple"),
        ("file" = String, Path, description = "rustup-init or rustup-init.exe"),
    ),
    responses(
        (status = 200, description = "Installer binary for the current release, streamed from upstream (cached)", body = ArtifactBytes),
        (status = 400, description = "Unsafe path segment"),
        (status = 403, description = "Access denied, or the release is blocked"),
        (status = 404, description = "Not found upstream, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rustup/rustup/dist/{triple}/{file}")]
pub async fn rustup_bootstrap(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, triple, file) = path.into_inner();
    require_registry_type(&registry, "rustup", &map)?;
    validate_path_safe("triple", &triple).map_err(AppError::from)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    let public_base = registry_public_base(&req, &registry);
    let doc = fetch_proxy_document(
        svc.clone(),
        PackageId::new(&registry, RUSTUP_PACKAGE, "release"),
        identity.clone(),
        Action::ReleasesList,
        DocumentKind::RUSTUP_RELEASE,
        public_base,
    )
    .await?;
    let version = doc
        .body
        .as_text()
        .and_then(release_version)
        .ok_or_else(|| {
            AppError::not_found(
                "rustup/release-stable.toml names no version, so the installer's \
                                 current release cannot be resolved",
            )
        })?;

    let pkg = PackageId::new(&registry, RUSTUP_PACKAGE, &version)
        .with_artifact(format!("{triple}/{file}"));
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(content_type_for(&file)),
    )
    .await
}

/// `version = "1.29.1"` out of `release-stable.toml`.
///
/// Two lines of text rather than a TOML parse: the document is forty bytes and
/// the handler needs one field of it.
fn release_version(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("version")?;
        let rest = rest.trim_start().strip_prefix('=')?.trim();
        // Both TOML string quotes. The release tooling writes `version =
        // '1.29.1'` — single — and a reader that took only `"` answered 404 to
        // every `rustup-init` this tree has ever served, with the fixture that
        // was written by hand passing. `tests/heavy/rustup.sh` §7 is what found
        // it; the test below is upstream's own bytes.
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let inner = rest.strip_prefix(quote)?;
        let end = inner.find(quote)?;
        Some(inner[..end].to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_channel_documents_are_told_apart_by_suffix() {
        assert!(matches!(
            ManifestFile::parse("channel-rust-stable.toml"),
            Some(ManifestFile::Manifest("stable"))
        ));
        assert!(matches!(
            ManifestFile::parse("channel-rust-nightly.toml.sha256"),
            Some(ManifestFile::Sidecar("nightly"))
        ));
        assert!(matches!(
            ManifestFile::parse("channel-rust-1.98.1.toml.asc"),
            Some(ManifestFile::Signature("1.98.1"))
        ));
        // rustup's v1 fallback, and anything else in the directory.
        assert!(ManifestFile::parse("channel-rust-stable").is_none());
        assert!(ManifestFile::parse("channel-rust-stable-date.txt").is_none());
        assert!(ManifestFile::parse("rustc-1.98.1-x.tar.xz").is_none());
    }

    #[test]
    fn the_installers_version_is_read_off_two_lines_of_text() {
        // Upstream's own bytes, single-quoted, as `curl
        // https://static.rust-lang.org/rustup/release-stable.toml` returns
        // them. The double-quoted spelling below is legal TOML nobody writes,
        // and reading only it was a 404 on every `rustup-init`.
        assert_eq!(
            release_version("schema-version = '1'\nversion = '1.29.1'\n").as_deref(),
            Some("1.29.1")
        );
        assert_eq!(
            release_version("schema-version = \"1\"\nversion = \"1.29.1\"\n").as_deref(),
            Some("1.29.1")
        );
        assert_eq!(release_version("schema-version = '1'\n"), None);
        assert_eq!(release_version("version = 1.29.1\n"), None);
        assert_eq!(release_version(""), None);
    }

    #[test]
    fn checksums_and_signatures_are_text_and_archives_are_bytes() {
        assert_eq!(
            content_type_for("rustc-1.98.1-x.tar.xz.sha256"),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            content_type_for("channel-rust-stable.toml.asc"),
            "application/pgp-signature"
        );
        assert_eq!(
            content_type_for("rustc-1.98.1-x.tar.xz"),
            "application/octet-stream"
        );
    }
}
