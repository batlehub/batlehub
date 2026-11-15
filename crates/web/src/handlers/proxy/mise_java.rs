//! mise's JVM catalogue — mise's `core:java` (RFC 0048 §4.4, §6.4).
//!
//! mise reads `https://mise-java.jdx.dev/jvm/{type}/{os}/{arch}.json` and
//! downloads the URL of the row it picks (mise 2026.11.3
//! `src/plugins/core/java.rs:375-395`, `:207-234`). With the rewrite rules
//! `client setup` writes, both land here:
//!
//! - `jvm/{type}/{os}/{arch}.json` is the document through the listing funnel
//!   under the package `java/{type}/{os}/{arch}`: rows off `vendor_hosts`
//!   dropped by the client, blocked and too-young builds by the filter, every
//!   URL left as upstream wrote it;
//! - `dl/{host}/{path}` is a row's URL. The `(host, path)` is a lookup key and
//!   never a fetch target: it is resolved to its row's coordinate *after* the
//!   registry grant and before anything is fetched, then streamed through
//!   [`proxy_stream`] under `java@{coordinate}`, so a blocked build is `403`
//!   whether mise read the document or a lock named the URL (T4).
//!
//! Air-gapped, the document is composed from the held archives' rows, and a
//! download is answered from the held archive with that key.

use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_adapters::registry::mise_java::UNRESOLVED_VERSION;
use batlehub_core::{
    entities::{Action, PackageId},
    error::CoreError,
    ports::DocumentKind,
    services::{
        mise_java::{host_path, index_key, Platform, JAVA_PACKAGE},
        ProxyService,
    },
};

use super::common::{proxy_document, proxy_stream, require_registry_type};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

/// One metadata document, filtered.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-java/jvm/{release_type}/{os}/{arch}.json",
    tag = "proxy/mise-java",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("release_type" = String, Path, description = "ga or ea, as the registry's release_types allow"),
        ("os" = String, Path, description = "linux, macosx, windows or alpine-linux"),
        ("arch" = String, Path, description = "x86_64, aarch64, i686, arm32-vfp-hflt, ppc64le, s390x or riscv64"),
    ),
    responses(
        (status = 200, description = "mise-java's metadata rows for the platform: rows outside vendor_hosts, blocked builds and builds younger than the age gate removed; URLs unchanged", body = Vec<UpstreamDocument>),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, platform or release type"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-java/jvm/{release_type}/{os}/{arch}.json")]
pub async fn mise_java_document(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, release_type, os, arch) = path.into_inner();
    require_registry_type(&registry, "mise-java", &map)?;
    let Some(platform) = Platform::parse(&release_type, &os, &arch) else {
        return Err(AppError::not_found(format!(
            "mise-java has no document jvm/{release_type}/{os}/{arch}.json"
        )));
    };
    proxy_document(
        svc,
        PackageId::new(&registry, platform.listing_name(), "index"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// One archive, by its row's URL.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-java/dl/{path}",
    tag = "proxy/mise-java",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "A row's URL without its scheme: {host}/{path} (e.g. cdn.azul.com/zulu/bin/zulu21.38.21-ca-jdk21.0.5-linux_x64.tar.gz)"),
    ),
    responses(
        (status = 200, description = "The archive (cached, verified against the row's checksum when it has a SHA-256, SHA-512 or SHA-1 one)", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied, or the build is blocked"),
        (status = 404, description = "No row of this registry's documents names this URL, or unknown registry"),
        (status = 502, description = "The vendor failed, or redirected off https, vendor_hosts and redirect_hosts"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-java/dl/{path:.*}")]
pub async fn mise_java_download(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    let (registry, _) = path.into_inner();
    require_registry_type(&registry, "mise-java", &map)?;
    // The raw tail, as the client sent it: the key is decoded exactly once,
    // the row's spelling the same way, so `%2B` and `+` meet.
    let raw = req
        .uri()
        .path()
        .split_once("/mise-java/dl/")
        .map_or("", |(_, tail)| tail);
    let not_a_row = || {
        AppError::not_found(format!(
            "https://{raw} is not a download any mise-java document of this registry lists"
        ))
    };
    let url = format!("https://{raw}");
    let Some(hp) = host_path(&url) else {
        return Err(not_a_row());
    };
    let key = index_key(hp);
    if batlehub_core::services::validate_coordinate(JAVA_PACKAGE, UNRESOLVED_VERSION, Some(&key))
        .is_err()
    {
        return Err(not_a_row());
    }

    // Grants before the lookup: resolving a key may fetch documents, and a
    // refused caller learns nothing about which URLs exist (RFC 0048 §7.2).
    let lookup = PackageId::new(&registry, JAVA_PACKAGE, UNRESOLVED_VERSION).with_artifact(&key);
    svc.authorize_listing(&lookup, &identity.0, Action::ReleasesRead)
        .await
        .map_err(AppError::from)?;

    let coordinate = match svc.canonical_coordinate(&lookup).await {
        Ok(Some(id)) => Some(id),
        // Air-gapped (the offline client resolves nothing), or a row no longer
        // listed: an archive already held under this key is still served —
        // unless blocked, which `proxy_stream` decides (R6).
        Ok(None) | Err(CoreError::NotFound(_)) | Err(CoreError::ContentUnavailable { .. }) => {
            held_coordinate(&svc, &registry, &key).await
        }
        Err(e) => return Err(AppError::from(e)),
    };
    match coordinate {
        Some(pkg) => {
            proxy_stream(
                svc,
                pkg,
                identity,
                Action::ReleasesRead,
                Some("application/octet-stream"),
            )
            .await
        }
        // Air-gapped and not held: the funnel answers `503` and records the
        // miss, as it does for every kind.
        None if svc.hot.read().await.air_gap.enabled => {
            proxy_stream(
                svc,
                lookup,
                identity,
                Action::ReleasesRead,
                Some("application/octet-stream"),
            )
            .await
        }
        None => Err(not_a_row()),
    }
}

/// The coordinate an archive held under `key` was cached as.
async fn held_coordinate(svc: &ProxyService, registry: &str, key: &str) -> Option<PackageId> {
    svc.held_artifacts(registry)
        .await
        .into_iter()
        .find(|(name, h)| name == JAVA_PACKAGE && h.artifact.as_deref() == Some(key))
        .map(|(_, h)| PackageId::new(registry, JAVA_PACKAGE, h.version).with_artifact(key))
}
