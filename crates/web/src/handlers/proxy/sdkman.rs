//! SDKMAN — the candidates API and the download broker (RFC 0010 phase 6).
//!
//! `sdk` is a bash function that calls ten endpoints, all `GET`, all
//! `text/plain`, on two hosts. Both are served here under one registry:
//! `SDKMAN_CANDIDATES_API` points at `…/sdkman` and `SDKMAN_BROKER_API` at
//! `…/sdkman/broker`. What each route *is* decides how it is served:
//!
//! - **Three filtered listings** — `versions/all`, `candidates/default/{c}`
//!   and the rendered `versions/list` — go through [`proxy_document`], so a
//!   blocked version is gone before `sdk list` prints it or `sdk install`
//!   resolves a default to it (§4.4). The default names one version and
//!   carries no list, so it is repaired here against the filtered
//!   `versions/all`, the composition Go's `@latest` already does.
//! - **The chokepoint**, `candidates/validate/{c}/{v}/{plat}`, answers
//!   `invalid` for a blocked version without asking upstream, and `sdk
//!   install` stops on its own *"Stop! … is not a valid java version."* with
//!   no download attempted (§7: a deliberate small lie, recorded truthfully
//!   in the audit log by the download gate should anyone ask for the bytes
//!   anyway).
//! - **Relayed documents** — `candidates/all`, `candidates/list`, the hook
//!   scripts, `healthcheck`, `broker/version/…`, `selfupdate/…` — cross
//!   byte-exact. The hooks are bash the client sources and runs; rewriting a
//!   byte of one would make this proxy a co-author of executed shell (§7).
//! - **The download**, `broker/download/{c}/{v}/{plat}`, goes through
//!   [`proxy_stream`] and is cached under `{c}/{v}/{plat}`; the client
//!   follows the broker's `302` server-side, so the bytes are cached here
//!   rather than referred to a CDN (decision 3).
//!
//! Every path segment reaches a storage or cache key, so each is validated
//! here for a clean `400`: the candidate as a package name, the version as a
//! single segment, the platform against SDKMAN's closed set of eight.

use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};
use serde::Deserialize;

use batlehub_core::{
    entities::{Action, PackageId, RegistryKind},
    ports::DocumentKind,
    services::{
        blocking::sdkman::{default_version, repaired_default},
        sdkman::{listing_package, parse_platform, DEFAULT_PLATFORM},
        validate_package_name, validate_path_safe, ProxyService,
    },
};

use super::common::{
    document_response, fetch_proxy_document, proxy_document, proxy_stream, require_registry_type,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

const KIND: &str = "sdkman";
const TEXT: &str = "text/plain; charset=utf-8";

/// A relayed API document's coordinate: the path is the package, so each is
/// its own cache entry under the registry's `metadata_ttl`.
fn relayed(registry: &str, path: String) -> PackageId {
    PackageId::new(registry, path, "doc")
}

/// A candidate name, refused when it could not be a storage-key segment.
fn candidate(name: &str) -> Result<&str, AppError> {
    validate_package_name(name).map_err(AppError::from)?;
    if name.contains('/') || name.contains('?') {
        return Err(AppError::bad_request(format!(
            "'{name}' is not an SDKMAN candidate name"
        )));
    }
    Ok(name)
}

/// A version identifier, refused when it is not a single safe segment.
fn version(v: &str) -> Result<&str, AppError> {
    validate_path_safe("version", v).map_err(AppError::from)?;
    Ok(v)
}

/// One of SDKMAN's eight platforms, refused at the edge rather than forwarded
/// upstream as a path segment.
fn platform(p: &str) -> Result<&'static str, AppError> {
    parse_platform(p).ok_or_else(|| {
        AppError::bad_request(format!(
            "'{p}' is not an SDKMAN platform (linuxx64, linuxx32, linuxarm32hf, linuxarm64, \
             darwinx64, darwinarm64, windowsx64, exotic)"
        ))
    })
}

/// A value of the rendered list's `current=`/`installed=` query: version
/// identifiers, comma-separated. Anything else is refused, so the query that
/// becomes part of a cache key and an upstream URL is one this proxy built.
fn query_value(name: &str, value: &str) -> Result<(), AppError> {
    let ok = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | ','));
    if ok {
        Ok(())
    } else {
        Err(AppError::bad_request(format!(
            "'{name}' must be a comma-separated list of version identifiers"
        )))
    }
}

/// `text/plain` for a relayed or composed body.
fn text_response(body: String) -> HttpResponse {
    HttpResponse::Ok().content_type(TEXT).body(body)
}

// ── candidates ───────────────────────────────────────────────────────────────

/// Every candidate name, comma-separated — what `sdk update` caches and
/// `sdkman-init.sh` reads at every shell start.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/candidates/all",
    tag = "proxy/sdkman",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Comma-separated candidate names, relayed byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/candidates/all")]
pub async fn sdkman_candidates_all(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    proxy_document(
        svc,
        relayed(&registry, "candidates/all".to_owned()),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

/// The rendered candidate table `sdk list` prints with no argument.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/candidates/list",
    tag = "proxy/sdkman",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Rendered candidate table, relayed byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/candidates/list")]
pub async fn sdkman_candidates_list(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    proxy_document(
        svc,
        relayed(&registry, "candidates/list".to_owned()),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

/// The version `sdk install <candidate>` resolves to with no version given,
/// repaired to the newest allowed version when the upstream default is
/// blocked.
///
/// The repair reads the filtered `versions/all` for the default platform:
/// `candidates/default/{c}` carries no platform, and every vendor publishes
/// for Linux x64, so its list is the nearest thing to the union of the eight.
/// A default that names a version some other platform lacks fails at
/// `validate`, which is where a wrong version has always failed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/candidates/default/{candidate}",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("candidate" = String, Path, description = "Candidate (e.g. java, maven)"),
    ),
    responses(
        (status = 200, description = "The default version identifier, repaired past a blocked one", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unsafe candidate name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or candidate"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/candidates/default/{candidate}")]
pub async fn sdkman_candidate_default(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, candidate_name) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let candidate = candidate(&candidate_name)?;

    let default = fetch_proxy_document(
        svc.clone(),
        PackageId::new(&registry, candidate, "default"),
        AuthIdentity(identity.0.clone(), identity.1.clone()),
        Action::ReleasesList,
        DocumentKind::SDKMAN_DEFAULT,
        String::new(),
    )
    .await?;
    let Some(body) = default.body.as_text() else {
        return Ok(document_response(default));
    };

    let blocked = svc
        .blocked_versions_for(&registry, candidate, RegistryKind::Sdkman)
        .await;
    if blocked.is_empty() || !blocked.contains(default_version(body)) {
        return Ok(document_response(default));
    }

    // The named default is blocked: pick the newest survivor of the filtered
    // list, which `version_document` has already stripped.
    let list = fetch_proxy_document(
        svc,
        PackageId::new(
            &registry,
            listing_package(candidate, DEFAULT_PLATFORM),
            "versions",
        ),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await;
    let Ok(list) = list else {
        tracing::warn!(
            registry = %registry,
            candidate = %candidate,
            "could not load the filtered versions/all; serving candidates/default unrepaired"
        );
        return Ok(document_response(default));
    };
    match repaired_default(body, list.body.as_text().unwrap_or_default(), &blocked) {
        Some(replacement) => {
            tracing::info!(
                registry = %registry,
                candidate = %candidate,
                blocked = %default_version(body),
                replacement = %replacement,
                "candidates/default named a blocked version; repaired"
            );
            Ok(text_response(replacement))
        }
        None => Ok(document_response(default)),
    }
}

/// The chokepoint: `valid` or `invalid` for one version on one platform. A
/// blocked version is `invalid` here, and upstream is not asked.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/candidates/validate/{candidate}/{version}/{platform}",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("candidate" = String, Path, description = "Candidate (e.g. java)"),
        ("version" = String, Path, description = "Version identifier (e.g. 21.0.5-tem)"),
        ("platform" = String, Path, description = "One of linuxx64, linuxx32, linuxarm32hf, linuxarm64, darwinx64, darwinarm64, windowsx64, exotic"),
    ),
    responses(
        (status = 200, description = "`valid` or `invalid`; a blocked version is `invalid`", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unsafe candidate, version or unknown platform"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/candidates/validate/{candidate}/{version}/{platform}")]
pub async fn sdkman_validate(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, c, v, p) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let candidate = candidate(&c)?;
    let version = version(&v)?;
    let platform = platform(&p)?;

    // Authorise first, then decide locally: a blocked version is refused
    // without an upstream request, which is the "upstream never asked" of
    // RFC 0010 §5.3. The verb is the listing one — this document names a
    // version but carries no bytes.
    let pkg = PackageId::new(&registry, candidate, version).with_artifact(platform);
    svc.authorize_listing(&pkg, &identity.0, Action::ReleasesList)
        .await
        .map_err(AppError::from)?;
    let blocked = svc
        .blocked_versions_for(&registry, candidate, RegistryKind::Sdkman)
        .await;
    if blocked.contains(version) {
        tracing::info!(
            registry = %registry,
            candidate = %candidate,
            version = %version,
            platform = %platform,
            "validate: blocked version answered as invalid"
        );
        return Ok(text_response("invalid".to_owned()));
    }

    proxy_document(
        svc,
        relayed(
            &registry,
            format!("candidates/validate/{candidate}/{version}/{platform}"),
        ),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

/// Every version of a candidate on a platform, comma-separated, blocked
/// versions removed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/all",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("candidate" = String, Path, description = "Candidate (e.g. java)"),
        ("platform" = String, Path, description = "SDKMAN platform (e.g. linuxx64)"),
    ),
    responses(
        (status = 200, description = "Comma-separated version identifiers, blocked versions removed", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unsafe candidate or unknown platform"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or candidate"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/all")]
pub async fn sdkman_versions_all(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, c, p) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let candidate = candidate(&c)?;
    let platform = platform(&p)?;
    proxy_document(
        svc,
        PackageId::new(&registry, listing_package(candidate, platform), "versions"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// The `?current=&installed=` pair `sdk list <candidate>` sends. Upstream
/// answers `400` without it, so both default to empty rather than absent.
#[derive(Debug, Deserialize)]
pub struct VersionsListQuery {
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub installed: String,
}

/// The rendered table `sdk list <candidate>` prints, in either of its two
/// layouts, with blocked versions removed (a whole row of the vendor table,
/// a blanked cell of the grid).
///
/// The client's `current`/`installed` query is forwarded, because the table
/// marks those versions; it is also part of the cache key, so two clients
/// with different installed sets never share an entry.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/list",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("candidate" = String, Path, description = "Candidate (e.g. java)"),
        ("platform" = String, Path, description = "SDKMAN platform (e.g. linuxx64)"),
        ("current" = Option<String>, Query, description = "The client's current version, as `sdk list` sends it"),
        ("installed" = Option<String>, Query, description = "The client's installed versions, comma-separated"),
    ),
    responses(
        (status = 200, description = "Rendered version table, blocked versions removed, layout preserved", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unsafe candidate, unknown platform or malformed query"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or candidate"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/list")]
pub async fn sdkman_versions_list(
    path: web::Path<(String, String, String)>,
    query: web::Query<VersionsListQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, c, p) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let candidate = candidate(&c)?;
    let platform = platform(&p)?;
    query_value("current", &query.current)?;
    query_value("installed", &query.installed)?;
    // Rebuilt rather than forwarded raw, so the string that becomes a cache
    // key and an upstream URL is one this proxy assembled.
    let package = format!(
        "{}?current={}&installed={}",
        listing_package(candidate, platform),
        query.current,
        query.installed
    );
    proxy_document(
        svc,
        PackageId::new(&registry, package, "versions-list"),
        identity,
        Action::ReleasesList,
        DocumentKind::SDKMAN_VERSIONS_LIST,
        String::new(),
    )
    .await
}

// ── hooks, health, self-update ───────────────────────────────────────────────

/// A pre- or post-install hook: bash the client sources and runs, relayed
/// byte-exact (RFC 0010 §7).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/hooks/{phase}/{candidate}/{version}/{platform}",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("phase" = String, Path, description = "`pre` or `post`"),
        ("candidate" = String, Path, description = "Candidate (e.g. java)"),
        ("version" = String, Path, description = "Version identifier"),
        ("platform" = String, Path, description = "SDKMAN platform"),
    ),
    responses(
        (status = 200, description = "The hook script, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unsafe candidate, version or unknown platform"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, phase or hook"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/hooks/{phase}/{candidate}/{version}/{platform}")]
pub async fn sdkman_hook(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, phase, c, v, p) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    if !matches!(phase.as_str(), "pre" | "post") {
        return Err(AppError::not_found(format!(
            "SDKMAN has pre and post hooks, not '{phase}'"
        )));
    }
    let candidate = candidate(&c)?;
    let version = version(&v)?;
    let platform = platform(&p)?;
    proxy_document(
        svc,
        relayed(
            &registry,
            format!("hooks/{phase}/{candidate}/{version}/{platform}"),
        ),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

/// The API's health token, read by every `sdk` invocation. Relayed as-is:
/// the client treats an HTML body as "proxy detected" and an empty one as
/// "offline", so neither may be synthesised here.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/healthcheck",
    tag = "proxy/sdkman",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The upstream health token, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/healthcheck")]
pub async fn sdkman_healthcheck(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    proxy_document(
        svc,
        relayed(&registry, "healthcheck".to_owned()),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

/// The current SDKMAN script or native version on a channel — what `sdk
/// selfupdate` and the daily upgrade check read.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/broker/version/sdkman/{component}/{channel}",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("component" = String, Path, description = "`script` or `native`"),
        ("channel" = String, Path, description = "`stable` or `beta`"),
    ),
    responses(
        (status = 200, description = "The version string, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, component or channel"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/broker/version/sdkman/{component}/{channel}")]
pub async fn sdkman_selfupdate_version(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, component, channel) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    if !matches!(component.as_str(), "script" | "native") {
        return Err(AppError::not_found(format!(
            "SDKMAN has a script and a native component, not '{component}'"
        )));
    }
    channel_of(&channel)?;
    proxy_document(
        svc,
        relayed(
            &registry,
            format!("broker/version/sdkman/{component}/{channel}"),
        ),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

/// The self-update script for a channel and platform — bash the client
/// pipes into `bash`, relayed byte-exact for the same reason the hooks are.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/selfupdate/{channel}/{platform}",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("channel" = String, Path, description = "`stable` or `beta`"),
        ("platform" = String, Path, description = "SDKMAN platform"),
    ),
    responses(
        (status = 200, description = "The self-update script, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unknown platform"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or channel"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/selfupdate/{channel}/{platform}")]
pub async fn sdkman_selfupdate(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, channel, p) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    channel_of(&channel)?;
    let platform = platform(&p)?;
    proxy_document(
        svc,
        relayed(&registry, format!("selfupdate/{channel}/{platform}")),
        identity,
        Action::ReleasesList,
        DocumentKind::RELAYED,
        String::new(),
    )
    .await
}

fn channel_of(channel: &str) -> Result<(), AppError> {
    if matches!(channel, "stable" | "beta") {
        Ok(())
    } else {
        Err(AppError::not_found(format!(
            "SDKMAN has a stable and a beta channel, not '{channel}'"
        )))
    }
}

// ── the broker ───────────────────────────────────────────────────────────────

/// The archive for one version on one platform, streamed through the
/// broker's redirect chain and cached under `{candidate}/{version}/{platform}`.
///
/// A direct request for a blocked version gets the download gate's `403`
/// here even though `validate` already said `invalid`: hiding governs
/// resolution, it does not replace diagnosis.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/sdkman/broker/download/{candidate}/{version}/{platform}",
    tag = "proxy/sdkman",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("candidate" = String, Path, description = "Candidate (e.g. java)"),
        ("version" = String, Path, description = "Version identifier (e.g. 21.0.5-tem)"),
        ("platform" = String, Path, description = "SDKMAN platform (e.g. linuxx64)"),
    ),
    responses(
        (status = 200, description = "The archive, streamed from the vendor's CDN through the broker's redirect (cached)", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Unsafe candidate, version or unknown platform"),
        (status = 403, description = "Access denied, or the version is blocked"),
        (status = 404, description = "Not found upstream, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/sdkman/broker/download/{candidate}/{version}/{platform}")]
pub async fn sdkman_download(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, c, v, p) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let candidate = candidate(&c)?;
    let version = version(&v)?;
    let platform = platform(&p)?;
    let pkg = PackageId::new(&registry, candidate, version).with_artifact(platform);
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some("application/octet-stream"),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candidate_is_one_safe_segment() {
        assert!(candidate("java").is_ok());
        assert!(candidate("kotlin").is_ok());
        assert!(candidate("..").is_err());
        assert!(candidate("a/b").is_err());
        assert!(candidate("a?b").is_err());
        assert!(candidate("").is_err());
    }

    #[test]
    fn a_version_is_one_safe_segment() {
        assert!(version("21.0.5-tem").is_ok());
        assert!(version("17.0.20+1.1-librca").is_ok());
        assert!(version("..").is_err());
        assert!(version("a/b").is_err());
    }

    #[test]
    fn the_platform_is_one_of_eight() {
        assert_eq!(platform("linuxx64").unwrap(), "linuxx64");
        assert!(platform("linux").is_err());
        assert!(platform("../x").is_err());
    }

    #[test]
    fn the_list_query_is_version_identifiers_only() {
        assert!(query_value("current", "").is_ok());
        assert!(query_value("installed", "21.0.5-tem,17.0.20+1.1-librca").is_ok());
        assert!(query_value("installed", "../x").is_err());
        assert!(query_value("installed", "a b").is_err());
        assert!(query_value("installed", "x&y=1").is_err());
    }
}
