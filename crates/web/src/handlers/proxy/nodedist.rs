//! The `nodejs.org/dist` tree — nvm, fnm, `n` and mise (RFC 0010).
//!
//! Three routes, and the whole design is in which of them is a *document*:
//!
//! - `index.tab` and `index.json` go through [`proxy_document`], so a blocked
//!   release is removed before any client sees the table. That is the
//!   enforcement point: `nvm_remote_version` resolves **every** install
//!   through `index.tab`, including a fully specified `nvm install 22.11.0`,
//!   and prints its own *"Version '22.11.0' not found"* when the row is gone
//!   (§4.4). No download is attempted, so there is nothing for this proxy to
//!   refuse mid-transfer.
//! - `{version}/{file}` goes through [`proxy_stream`] and is cached under the
//!   coordinate `node/{version}/{file}` — a stable per-release key, so the
//!   second build agent to ask for a tarball does not leave the site. A direct
//!   request for a blocked release still gets its `403` from the download gate
//!   (hiding governs resolution; it does not replace diagnosis). This route
//!   also serves `SHASUMS256.txt` and its `.asc`/`.sig` siblings, **byte
//!   exact**: nvm verifies every download against the checksum file and a
//!   detached signature covers it, so rewriting it would break both (§7).
//!
//! The package name is `node` — or `iojs` when the registry's upstream is the
//! io.js tree — for every route, so a block, a cache key and a statistics row
//! agree without consulting the config (§4.3).
//!
//! Every path segment reaches a storage key, so each is validated here for a
//! clean `400`; `validate_coordinate` in `ProxyService::handle` and
//! `ensure_safe_key` in the storage backends remain the deeper guards.

use std::sync::Arc;

use actix_web::{get, web, Responder};

use batlehub_core::{
    entities::{Action, PackageId},
    ports::DocumentKind,
    services::{nodedist::package_name_for_upstream, validate_path_safe, ProxyService},
};

use super::common::{proxy_document, proxy_stream, require_registry_type};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, UpstreamMap};

/// The one package a `nodedist` registry serves, from its configured upstream.
fn package_name(registry: &str, upstreams: &UpstreamMap) -> &'static str {
    package_name_for_upstream(upstreams.upstream_for(registry).as_deref())
}

/// `Content-Type` for a dist file. nvm reads none of these; a browser saving
/// the checksum file does.
fn content_type_for(file: &str) -> &'static str {
    if file.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else if file.ends_with(".asc") {
        "application/pgp-signature"
    } else if file.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

/// The release table nvm resolves every install through, blocked releases
/// removed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nodedist/index.tab",
    tag = "proxy/nodedist",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Tab-separated release table, header preserved, blocked releases removed", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nodedist/index.tab")]
pub async fn nodedist_index_tab(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstreams: web::Data<UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "nodedist", &map)?;
    let name = package_name(&registry, &upstreams);
    proxy_document(
        svc,
        PackageId::new(&registry, name, "index"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// The same release table as JSON — what fnm and mise read.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nodedist/index.json",
    tag = "proxy/nodedist",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Release table as a JSON array, blocked releases removed", body = Vec<UpstreamDocument>),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nodedist/index.json")]
pub async fn nodedist_index_json(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstreams: web::Data<UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "nodedist", &map)?;
    let name = package_name(&registry, &upstreams);
    proxy_document(
        svc,
        PackageId::new(&registry, name, "index"),
        identity,
        Action::ReleasesList,
        DocumentKind::INDEX_JSON,
        String::new(),
    )
    .await
}

/// One file of one release: a tarball, `SHASUMS256.txt`, or a signature.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nodedist/{version}/{file}",
    tag = "proxy/nodedist",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("version" = String, Path, description = "Release directory, as the tree spells it (e.g. v22.11.0)"),
        ("file" = String, Path, description = "File inside the release directory (e.g. node-v22.11.0-linux-x64.tar.xz, SHASUMS256.txt)"),
    ),
    responses(
        (status = 200, description = "File streamed from upstream (cached), byte-exact", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Unsafe version or file name"),
        (status = 403, description = "Access denied, or the release is blocked"),
        (status = 404, description = "Not found upstream, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nodedist/{version}/{file}")]
pub async fn nodedist_file(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstreams: web::Data<UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let (registry, version, file) = path.into_inner();
    require_registry_type(&registry, "nodedist", &map)?;
    // Edge validation: both segments become a storage key. `validate_path_safe`
    // rejects `..`, separators and an empty value, for a clean `400` here
    // rather than a refusal three layers down.
    validate_path_safe("version", &version).map_err(AppError::from)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    let name = package_name(&registry, &upstreams);
    let pkg = PackageId::new(&registry, name, &version).with_artifact(&file);
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(content_type_for(&file)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_files_are_text_and_tarballs_are_bytes() {
        assert_eq!(
            content_type_for("SHASUMS256.txt"),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            content_type_for("SHASUMS256.txt.asc"),
            "application/pgp-signature"
        );
        assert_eq!(
            content_type_for("node-v22.11.0-linux-x64.tar.xz"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for("SHASUMS256.txt.sig"),
            "application/octet-stream"
        );
    }
}
