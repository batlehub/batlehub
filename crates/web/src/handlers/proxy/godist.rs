//! Go's toolchain distribution — mise's `core:go` (RFC 0048 §6.7).
//!
//! mise downloads `{go.download_mirror}/go{version}.{os}-{arch}.{ext}` and
//! `{…}.sha256` (`mise 2026.9.5 src/plugins/core/go.rs:126-128`, `:384-387`),
//! so the mirror this registry answers as is `…/proxy/{registry}/godist`.
//!
//! - `{file}` goes through [`proxy_stream`] under the coordinate
//!   `go@{version}/{file}`, the version read from the file name: a blocked
//!   release is `403` on its archive *and* its `.sha256`, a file the release
//!   document does not list is `404` without an upstream request, and the
//!   archive is verified against the document's `sha256` at cache write.
//! - `index.json` is the release document through [`proxy_document`], blocked
//!   releases removed — for a client or an operator that reads Go's own
//!   `go.dev/dl/?mode=json`. mise does not read it: its list comes from the
//!   versions host (the `mise-versions` kind), or `git ls-remote`.
//! - `info/refs` is that same filtered document as a git ref list, for the
//!   `git ls-remote` path: with `MISE_GO_REPO` set to this registry's
//!   `…/godist`, mise lists Go from it and a blocked release is not offered
//!   (RFC 0048 §15 Q13). Dumb HTTP only, no object behind any ref; every other
//!   git path under `…/godist` is unrouted.
//!
//! The file name is matched against `go{version}.{os}-{arch}.{tar.gz|zip}`
//! with an optional `.sha256` before it reaches a storage key, for a clean
//! `400`; `validate_coordinate` and `ensure_safe_key` stay the deeper guards.

use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};

use batlehub_core::{
    entities::{Action, PackageId},
    ports::DocumentKind,
    services::{
        godist::{git_refs, parse_file_name, GO_PACKAGE},
        ProxyService,
    },
};

use super::common::{fetch_proxy_document, proxy_document, proxy_stream, require_registry_type};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

/// Go's release document, blocked releases removed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/godist/index.json",
    tag = "proxy/godist",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Go's release document (go.dev/dl/?mode=json&include=all), blocked releases removed", body = Vec<UpstreamDocument>),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/godist/index.json")]
pub async fn godist_index_json(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "godist", &map)?;
    proxy_document(
        svc,
        PackageId::new(&registry, GO_PACKAGE, "index"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// Go's releases as git tags, for `git ls-remote` (mise's `go.repo`).
///
/// The *dumb* HTTP ref list: `<sha>\trefs/tags/go{version}` per release of
/// the filtered document, so a blocked release is absent. git sends
/// `?service=git-upload-pack` (and may send `Git-Protocol: version=2`); a
/// `text/plain` answer is how a dumb server replies to both, and `ls-remote`
/// needs nothing more. The object ids are the SHA-1 of the ref name, not
/// commits — nothing can be fetched from this "repository".
#[utoipa::path(
    get,
    path = "/proxy/{registry}/godist/info/refs",
    tag = "proxy/godist",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "`<sha1-of-ref>\\trefs/tags/go{version}` per listed release, blocked releases removed (git's dumb HTTP ref list)", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/godist/info/refs")]
pub async fn godist_info_refs(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "godist", &map)?;
    let doc = fetch_proxy_document(
        svc,
        PackageId::new(&registry, GO_PACKAGE, "index"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await?;
    let refs = doc.body.as_json().map(git_refs).unwrap_or_default();
    Ok(HttpResponse::Ok()
        .content_type("text/plain; charset=utf-8")
        .body(refs))
}

/// One archive of one Go release, or its `.sha256`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/godist/{file}",
    tag = "proxy/godist",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "go{version}.{os}-{arch}.{tar.gz|zip}, optionally with .sha256 (e.g. go1.25.4.linux-amd64.tar.gz)"),
    ),
    responses(
        (status = 200, description = "The archive (cached, verified against the release document's sha256) or its sha256 as 64 hex digits", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Not a Go archive file name"),
        (status = 403, description = "Access denied, or the release is blocked"),
        (status = 404, description = "Not in the release document, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/godist/{file}")]
pub async fn godist_file(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file) = path.into_inner();
    require_registry_type(&registry, "godist", &map)?;
    let Some((version, _, is_checksum)) = parse_file_name(&file) else {
        return Err(AppError::bad_request(format!(
            "'{file}' is not a Go archive name: go{{version}}.{{os}}-{{arch}}.{{tar.gz|zip}}[.sha256]"
        )));
    };
    let content_type = if is_checksum {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    let pkg = PackageId::new(&registry, GO_PACKAGE, version).with_artifact(&file);
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, Some(content_type)).await
}
