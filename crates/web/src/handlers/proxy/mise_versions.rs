//! mise's versions host, `mise-versions.jdx.dev` (RFC 0043).
//!
//! Four route families, and every path this kind answers is one of them — the
//! scope's default service is a `404`, so a new endpoint on jdx.dev cannot
//! start leaking through this instance without a change here (§5.2):
//!
//! - **The version list**, `data/{tool}.toml` and its pre-2026.6.6 spelling
//!   `tools/{tool}.toml`, through [`proxy_document`]: blocked versions and,
//!   with a `release_age_gate`, too-young ones removed. Under `[air_gap]` it is
//!   a `404` at once and not a recorded miss — mise reads `404` as "use the
//!   backend's own list", which RFC 0008-bis composes from held assets, and no
//!   bundle could ever carry this document (§5.3).
//! - **The python-build-standalone index**, `data/python-precompiled-*.gz`,
//!   relayed byte-exact through the passthrough cache (decision 3).
//! - **The install beacon**, in each of the three shapes mise has shipped:
//!   `204`, forwarded nowhere, logged nowhere above `trace` (decision 7).
//! - **The GitHub mirror**, `api/github/…` and `data/github/…`: `503`. mise asks
//!   api.github.com instead only on a timeout, a `429` or a `5xx`, and trusts a
//!   `404` as "no such release" — which would fail the install (decision 5).

use std::sync::Arc;

use actix_web::{get, post, web, HttpResponse, Responder};

use batlehub_core::{
    entities::{Action, PackageId},
    ports::DocumentKind,
    services::{validate_path_safe, ProxyService},
};

use super::common::{proxy_document, require_registry_type};
use super::upstream::{cached_forward_with, Outbound};
use crate::handlers::schemas::ProtocolDocument;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, UpstreamMap};

const KIND: &str = "mise-versions";

/// The tool a `{tool}.toml` file name names, validated for a storage key.
fn tool_of(file: &str) -> Result<&str, AppError> {
    let tool = file
        .strip_suffix(".toml")
        .ok_or_else(|| AppError::not_found(format!("no version list at '{file}'")))?;
    validate_path_safe("tool", tool).map_err(AppError::from)?;
    Ok(tool)
}

/// The target triple of a `python-precompiled-{triple}.gz` file name, or
/// `None` for any other file. Lowercase words joined by `-`, three or four of
/// them, as python-build-standalone spells a triple.
fn precompiled_triple(file: &str) -> Option<&str> {
    let triple = file
        .strip_prefix("python-precompiled-")?
        .strip_suffix(".gz")?;
    let parts: Vec<&str> = triple.split('-').collect();
    let well_formed = (3..=4).contains(&parts.len())
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    well_formed.then_some(triple)
}

/// The listing gate every route of this kind passes, the beacon included: a
/// closed registry stays closed to anonymous callers.
async fn authorize(
    svc: &ProxyService,
    registry: &str,
    name: &str,
    identity: &AuthIdentity,
) -> Result<(), AppError> {
    svc.authorize_listing(
        &PackageId::new(registry, name, "index"),
        &identity.0,
        Action::ReleasesList,
    )
    .await
    .map_err(AppError::from)
}

async fn air_gapped(svc: &ProxyService) -> bool {
    svc.hot.read().await.air_gap.enabled
}

async fn version_list(
    registry: String,
    file: String,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    require_registry_type(&registry, KIND, &map)?;
    let tool = tool_of(&file)?;
    if air_gapped(&svc).await {
        authorize(&svc, &registry, tool, &identity).await?;
        return Err(AppError::not_found(format!(
            "air gap: this instance lists no versions for '{tool}'; mise falls back to the \
             tool's own registry"
        )));
    }
    proxy_document(
        svc,
        PackageId::new(&registry, tool, "index"),
        identity,
        Action::ReleasesList,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// A tool's version list, blocked and too-young versions removed — or a file
/// of the python-build-standalone index, byte-exact.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/data/{file}",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "`{tool}.toml`, or `python-precompiled-{triple}.gz`"),
    ),
    responses(
        (status = 200, description = "The version list as TOML, filtered; or the gzipped python index, byte-exact", body = ProtocolDocument, content_type = "application/toml"),
        (status = 400, description = "Unsafe tool name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown tool or file, unknown registry, or an air-gapped listing"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-versions/data/{file}")]
pub async fn mise_versions_data(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstreams: web::Data<UpstreamMap>,
    client: web::Data<reqwest::Client>,
) -> Result<HttpResponse, AppError> {
    let (registry, file) = path.into_inner();
    let Some(triple) = precompiled_triple(&file) else {
        return version_list(registry, file, identity, svc, map).await;
    };
    require_registry_type(&registry, KIND, &map)?;
    authorize(&svc, &registry, "python", &identity).await?;
    let base = upstreams
        .upstream_for(&registry)
        .unwrap_or_else(|| batlehub_adapters::registry::mise_versions::DEFAULT_BASE.to_owned());
    let url = format!("{}/data/{file}", base.trim_end_matches('/'));
    let offline = air_gapped(&svc).await;
    // shortcut: held-or-503 in an air gap, no miss recorded; a bundle cannot
    // carry this index yet (RFC 0043 §4.3).
    cached_forward_with(
        &svc,
        &client,
        &registry,
        &format!("mise-versions:{registry}:python-precompiled:{triple}"),
        Outbound::get(url),
        offline,
    )
    .await
}

/// The same version list under the path mise 2026.1.0–2026.6.5 asks for
/// (RFC 0043 §11, decision 4).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/tools/{file}",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "`{tool}.toml`"),
    ),
    responses(
        (status = 200, description = "The version list as TOML, filtered", body = ProtocolDocument, content_type = "application/toml"),
        (status = 400, description = "Unsafe tool name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown tool, unknown registry, or an air-gapped listing"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-versions/tools/{file}")]
pub async fn mise_versions_tools(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    let (registry, file) = path.into_inner();
    version_list(registry, file, identity, svc, map).await
}

async fn beacon(
    registry: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
    map: &RegistryMap,
) -> Result<HttpResponse, AppError> {
    require_registry_type(registry, KIND, map)?;
    authorize(svc, registry, "beacon", identity).await?;
    // Nothing from the request is read, kept or forwarded: the query string
    // and body name the estate's tools, and that inventory is the leak this
    // route exists to stop (RFC 0043 §7).
    tracing::trace!(registry, "mise install beacon absorbed");
    Ok(HttpResponse::NoContent().finish())
}

/// The install beacon of mise 2026.11 and later — answered here, sent nowhere.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/t",
    tag = "proxy/mise-versions",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 204, description = "Absorbed: nothing stored, nothing forwarded"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-versions/t")]
pub async fn mise_versions_beacon(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    beacon(&path.into_inner(), &identity, &svc, &map).await
}

/// The install beacon of mise 2026.5–2026.10 — answered here, sent nowhere.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/mise-versions/api/tools/{tool}",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("tool" = String, Path, description = "Tool name, as mise sends it"),
    ),
    responses(
        (status = 204, description = "Absorbed: nothing stored, nothing forwarded"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/mise-versions/api/tools/{tool}")]
pub async fn mise_versions_beacon_tools(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    beacon(&path.into_inner().0, &identity, &svc, &map).await
}

/// The install beacon of mise 2026.1–2026.4 — answered here, sent nowhere.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/mise-versions/api/track",
    tag = "proxy/mise-versions",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 204, description = "Absorbed: nothing stored, nothing forwarded"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/mise-versions/api/track")]
pub async fn mise_versions_beacon_track(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    beacon(&path.into_inner(), &identity, &svc, &map).await
}

async fn github_mirror(
    registry: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
    map: &RegistryMap,
) -> Result<HttpResponse, AppError> {
    require_registry_type(registry, KIND, map)?;
    authorize(svc, registry, "github-mirror", identity).await?;
    Err(AppError::service_unavailable(
        "this instance does not mirror GitHub metadata: route api.github.com through the \
         github registry (`batlehub-cli registry suggest --mise` writes the rule), and mise \
         will not ask here",
    )
    .coded("mise_versions_github_mirror"))
}

/// The GitHub mirror's API family — refused with the `503` that makes mise ask
/// api.github.com instead (RFC 0043 §11, decision 5).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/api/github/{tail}",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("tail" = String, Path, description = "Anything under `api/github/`"),
    ),
    responses(
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
        (status = 503, description = "Not mirrored here: mise falls back to api.github.com"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-versions/api/github/{tail:.*}")]
pub async fn mise_versions_github_api(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    github_mirror(&path.into_inner().0, &identity, &svc, &map).await
}

/// The GitHub mirror's whole-list family — refused like the API family.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/data/github/{tail}",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("tail" = String, Path, description = "Anything under `data/github/`"),
    ),
    responses(
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
        (status = 503, description = "Not mirrored here: mise falls back to api.github.com"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-versions/data/github/{tail:.*}")]
pub async fn mise_versions_github_data(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    github_mirror(&path.into_inner().0, &identity, &svc, &map).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_triple_is_three_or_four_lowercase_words() {
        assert_eq!(
            precompiled_triple("python-precompiled-x86_64-unknown-linux-gnu.gz"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            precompiled_triple("python-precompiled-aarch64-apple-darwin.gz"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(precompiled_triple("python-precompiled-../x.gz"), None);
        assert_eq!(precompiled_triple("python-precompiled-a-b.gz"), None);
        assert_eq!(precompiled_triple("node.toml"), None);
    }

    #[test]
    fn a_tool_file_is_a_toml_with_a_safe_name() {
        assert_eq!(tool_of("jq.toml").unwrap(), "jq");
        assert!(tool_of("jq.json").is_err());
        assert!(tool_of("..toml").is_err());
    }
}
