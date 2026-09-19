//! Debian APT (`deb`) and RPM/YUM (`rpm`) repository handlers.
//!
//! Both formats are addressed purely by file path. Reads try local storage first
//! in Local/Hybrid mode (serving BatleHub-generated, signed index files and the
//! stored packages), then fall back to the upstream proxy in Proxy/Hybrid mode.
//! Publishing is handled in [`publish`].

use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};

use batlehub_core::{
    entities::PackageId,
    services::{LocalRegistryService, ProxyService},
};

use batlehub_config::schema::RegistryMode;

use super::common::{collect_storage_stream, proxy_stream, require_registry_type};
use crate::handlers::schemas::ArtifactBytes;
use crate::{
    error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap, RepoSignerMap,
};
use batlehub_core::entities::Action;

pub mod apk;
pub mod publish;

/// Storage key for a file in a locally-hosted deb/rpm repository.
pub fn repo_storage_key(registry: &str, path: &str) -> String {
    format!("local:{registry}/{path}")
}

/// Whether `path` is the public-key download for `reg_type`. The key is served
/// live from the configured signer so clients can be set up before the first
/// publish (`deb` accepts both `key.gpg` and `key.asc`).
fn is_signing_key_path(reg_type: &str, path: &str) -> bool {
    match reg_type {
        "deb" => path == "key.gpg" || path == "key.asc",
        "rpm" => path == "repodata/repomd.xml.key",
        // pacman has no standard key-download path; we expose the armored public
        // key at a fixed `key.gpg` for `pacman-key --add`.
        "pacman" => path == "key.gpg",
        _ => false,
    }
}

/// Best-effort `Content-Type` for a repository file path.
pub fn repo_content_type(path: &str) -> &'static str {
    if path.ends_with(".deb") {
        "application/vnd.debian.binary-package"
    } else if path.ends_with(".rpm") {
        "application/x-rpm"
    } else if path.ends_with(".xml") || path.ends_with(".xml.gz") {
        // .xml.gz is gzip but DNF expects the gzip stream under an xml name; serve
        // as octet-stream so clients don't try to render it.
        if path.ends_with(".gz") {
            "application/octet-stream"
        } else {
            "application/xml"
        }
    } else if path.ends_with(".gz")
        || path.ends_with(".asc")
        || path.ends_with(".key")
        // pacman packages and databases: .pkg.tar.{zst,xz}, <repo>.db, <repo>.files,
        // and detached .sig files all serve as opaque binary.
        || path.ends_with(".zst")
        || path.ends_with(".xz")
        || path.ends_with(".sig")
        || path.ends_with(".db")
        || path.ends_with(".files")
    {
        "application/octet-stream"
    } else {
        // Release / InRelease / Packages and friends are plain text.
        "text/plain; charset=utf-8"
    }
}

/// Shared read path for `deb`/`rpm`: local-first (Local/Hybrid), then upstream proxy.
#[allow(clippy::too_many_arguments)]
async fn repo_get(
    reg_type: &str,
    registry: &str,
    path: &str,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    signers: &RepoSignerMap,
    identity: AuthIdentity,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
) -> Result<HttpResponse, AppError> {
    require_registry_type(registry, reg_type, map)?;
    // Edge validation: reject traversal before building a storage key. The storage
    // backend's `ensure_safe_key` is the deeper guard.
    batlehub_core::services::validate_path_safe("path", path).map_err(AppError::from)?;

    // The signing public key is served live from the configured signer, so clients
    // can import it and configure the repo *before* anything is published (the key
    // is otherwise only written to storage during index regeneration on publish).
    if is_signing_key_path(reg_type, path) {
        if let Some(signer) = signers.get(registry) {
            return Ok(HttpResponse::Ok()
                .content_type("application/pgp-keys")
                .body(signer.armored_public_key()));
        }
        // No signer configured: this is an unsigned repository; fall through so the
        // request 404s rather than implying a key exists.
    }

    let ct = repo_content_type(path);
    let mode = mode_map.get(registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // Enforce the registry RBAC before serving from local storage. A direct
        // storage read would otherwise hand a restricted repo to any caller; the
        // proxy fall-through below runs the same `releases:read` check via the
        // rule chain, so this keeps local and proxy reads consistent. deb/rpm have
        // no per-package model, so authorization is keyed on the synthetic `repo`
        // coordinate the proxy path also uses.
        let auth_pkg = PackageId::new(registry, "repo", "_").with_artifact(path);
        svc.authorize_read(&auth_pkg, &identity.0, Action::ReleasesRead)
            .await
            .map_err(AppError::from)?;

        let key = repo_storage_key(registry, path);
        match local_svc.storage.retrieve(&key).await {
            Ok(Some(artifact)) => {
                let buf = collect_storage_stream(artifact.stream).await?;
                return Ok(HttpResponse::Ok().content_type(ct).body(buf));
            }
            Ok(None) if mode == RegistryMode::Local => {
                return Err(AppError::not_found(format!("{path} not found in registry")));
            }
            Ok(None) => { /* hybrid: fall through to proxy */ }
            Err(e) if mode == RegistryMode::Hybrid => {
                tracing::warn!("local storage error, falling back to proxy: {e}");
            }
            Err(e) => return Err(AppError::from(e)),
        }
    }

    let pkg = PackageId::new(registry, "repo", "_").with_artifact(path);
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, Some(ct)).await
}

/// Serve a file from a Debian APT repository (`/proxy/{registry}/deb/{path}`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/deb/{path}",
    tag = "proxy/deb",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Repository file path (e.g. dists/stable/Release, pool/.../x.deb)"),
    ),
    responses(
        (status = 200, description = "Repository file", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not found or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/deb/{path:.*}")]
pub async fn deb_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    signers: web::Data<RepoSignerMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file_path) = path.into_inner();
    repo_get(
        "deb", &registry, &file_path, svc, local_svc, &signers, identity, &map, &mode_map,
    )
    .await
}

/// Serve a file from an RPM/YUM repository (`/proxy/{registry}/rpm/{path}`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/rpm/{path}",
    tag = "proxy/rpm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Repository file path (e.g. repodata/repomd.xml, packages/x.rpm)"),
    ),
    responses(
        (status = 200, description = "Repository file", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not found or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/rpm/{path:.*}")]
pub async fn rpm_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    signers: web::Data<RepoSignerMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file_path) = path.into_inner();
    repo_get(
        "rpm", &registry, &file_path, svc, local_svc, &signers, identity, &map, &mode_map,
    )
    .await
}

/// Serve a file from an Arch Linux pacman repository (`/proxy/{registry}/pacman/{path}`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/pacman/{path}",
    tag = "proxy/pacman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Repository file path (e.g. x86_64/arch.db, x86_64/hello-1.0-1-x86_64.pkg.tar.zst)"),
    ),
    responses(
        (status = 200, description = "Repository file", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not found or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/pacman/{path:.*}")]
pub async fn pacman_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    signers: web::Data<RepoSignerMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file_path) = path.into_inner();
    repo_get(
        "pacman", &registry, &file_path, svc, local_svc, &signers, identity, &map, &mode_map,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(
            repo_content_type("pool/main/h/hello/hello_1.0_amd64.deb"),
            "application/vnd.debian.binary-package"
        );
        assert_eq!(
            repo_content_type("packages/hello-1.0-1.x86_64.rpm"),
            "application/x-rpm"
        );
        assert_eq!(repo_content_type("repodata/repomd.xml"), "application/xml");
        assert_eq!(
            repo_content_type("repodata/primary.xml.gz"),
            "application/octet-stream"
        );
        assert_eq!(
            repo_content_type("dists/stable/Release"),
            "text/plain; charset=utf-8"
        );
        // pacman package, database, and signature.
        assert_eq!(
            repo_content_type("x86_64/hello-1.0-1-x86_64.pkg.tar.zst"),
            "application/octet-stream"
        );
        assert_eq!(
            repo_content_type("x86_64/arch.db"),
            "application/octet-stream"
        );
        assert_eq!(
            repo_content_type("x86_64/arch.db.sig"),
            "application/octet-stream"
        );
    }

    #[test]
    fn storage_key_format() {
        assert_eq!(
            repo_storage_key("myapt", "dists/stable/Release"),
            "local:myapt/dists/stable/Release"
        );
    }
}
