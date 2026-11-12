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
use crate::handlers::schemas::{ProtocolDocument, UpstreamDocument};
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
    hosts: Option<web::Data<crate::RegistryHostMap>>,
) -> Result<HttpResponse, AppError> {
    require_registry_type(&registry, KIND, &map)?;
    let tool = tool_of(&file)?;
    // RFC 0045 T6: an overlay or retired name is answered here, before any
    // cache lookup, and never reaches jdx.dev.
    if let Some(refusal) = local_name_refusal(hosts.as_ref(), &registry, tool) {
        authorize(&svc, &registry, tool, &identity).await?;
        return Err(refusal);
    }
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
    hosts: Option<web::Data<crate::RegistryHostMap>>,
) -> Result<HttpResponse, AppError> {
    let (registry, file) = path.into_inner();
    let Some(triple) = precompiled_triple(&file) else {
        return version_list(registry, file, identity, svc, map, hosts).await;
    };
    require_registry_type(&registry, KIND, &map)?;
    authorize(&svc, &registry, "python", &identity).await?;
    let base = upstreams
        .upstream_for(&registry)
        .unwrap_or_else(|| batlehub_adapters::registry::mise_versions::DEFAULT_BASE.to_owned());
    // Rebuilt from the validated triple (`[a-z0-9_]` parts), never from the
    // request's own `file`.
    let url = format!(
        "{}/data/python-precompiled-{triple}.gz",
        base.trim_end_matches('/')
    );
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
    hosts: Option<web::Data<crate::RegistryHostMap>>,
) -> Result<HttpResponse, AppError> {
    let (registry, file) = path.into_inner();
    version_list(registry, file, identity, svc, map, hosts).await
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

// ── RFC 0045: the wire's own answers, and the registry archive ──────────────

/// `404 not_wired` for `host`. Built from the host alone, so the answer to an
/// unwired host and to a rewrite the caller cannot read is the same body
/// (§15 Q15): the outcome never maps the wired hosts.
fn not_wired(host: &str) -> AppError {
    AppError::not_found(format!(
        "{host} is not wired on this instance. Regenerate the rule: \
         batlehub-cli registry suggest --mise --auto-wire"
    ))
    .coded("not_wired")
}

/// What `_wire-answer` says to the caller of `request` for a rewrite whose
/// target refused it (§15 Q15): `not_wired` when the caller may read the
/// `mise-versions` registry, that registry's own refusal otherwise.
pub async fn wire_refusal(
    request: &actix_web::HttpRequest,
    registry: &str,
    host: &str,
) -> HttpResponse {
    use actix_web::{HttpMessage, ResponseError};
    let identity = request
        .extensions()
        .get::<batlehub_core::entities::Identity>()
        .cloned()
        .unwrap_or_else(batlehub_core::entities::Identity::anonymous);
    let Some(svc) = request.app_data::<web::Data<Arc<ProxyService>>>() else {
        return not_wired(host).error_response();
    };
    match svc
        .authorize_listing(
            &PackageId::new(registry, "index", "index"),
            &identity,
            Action::ReleasesList,
        )
        .await
    {
        Ok(()) => not_wired(host).error_response(),
        Err(e) => AppError::from(e).error_response(),
    }
}

/// The wire's own answer: a bounce, `not_wired`, a malformed path, or the
/// air-gap sink — behind read on this registry. Reached only as a rewrite;
/// called directly it is `404` and nothing else.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/_wire-answer/{outcome}",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("outcome" = String, Path, description = "`bounce`, `not-wired`, `malformed` or `sink` — set by the wire, not the client"),
    ),
    responses(
        // No `headers(...)`: Spectral (owasp:api8) then demands CORS headers on the response.
        (status = 307, description = "The origin, for a wired prefix with no typed rule; `Location` is the URL mise asked for"),
        (status = 400, description = "A malformed wire path"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not wired, or not reached through the wire"),
        (status = 501, description = "Air gap: recorded, this host has no mirror"),
    ),
    security(("bearer_token" = [])),
)]
// shortcut: GET only — mise reaches every wired host with GET but for the
// beacon, whose POST is a typed rewrite; widen if a client POSTs elsewhere.
#[get("/proxy/{registry}/mise-versions/_wire-answer/{outcome}")]
pub async fn mise_versions_wire_answer(
    req: actix_web::HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    use actix_web::HttpMessage;
    let (registry, _) = path.into_inner();
    let answer = req
        .extensions()
        .get::<crate::mise_wire::WireAnswer>()
        .cloned();
    let Some(answer) = answer else {
        return Err(AppError::not_found("no such route"));
    };
    require_registry_type(&registry, KIND, &map)?;
    authorize(&svc, &registry, "index", &identity).await?;
    Ok(match answer {
        crate::mise_wire::WireAnswer::Bounce(url) => HttpResponse::TemporaryRedirect()
            .insert_header((actix_web::http::header::LOCATION, url))
            .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
            .finish(),
        crate::mise_wire::WireAnswer::NotWired(host) => return Err(not_wired(&host)),
        crate::mise_wire::WireAnswer::Malformed => {
            return Err(AppError::bad_request("malformed mise wire path").coded("malformed"))
        }
        crate::mise_wire::WireAnswer::Sink(tail) => {
            crate::handlers::air_gap::sink_answer(&svc, tail).await
        }
    })
}

/// The wire of `hosts` when it belongs to `registry`.
fn wire_of(
    hosts: Option<&web::Data<crate::RegistryHostMap>>,
    registry: &str,
) -> Option<Arc<crate::mise_wire::MiseWire>> {
    hosts
        .and_then(|h| h.wire())
        .filter(|w| w.registry == registry)
}

/// Overlay and `retired` names are answered here and never forwarded (§4.2,
/// T6): mise then lists from the tool's backend, through the wire.
fn local_name_refusal(
    hosts: Option<&web::Data<crate::RegistryHostMap>>,
    registry: &str,
    tool: &str,
) -> Option<AppError> {
    let wire = wire_of(hosts, registry)?;
    wire.local_names.contains(tool).then(|| {
        AppError::not_found(format!(
            "'{tool}' is one of this instance's own tools; mise lists it from its backend"
        ))
        .coded("overlay_tool")
    })
}

/// The upstream archive check failed, or the merge did — for the config page.
#[derive(Debug, Clone, Default)]
pub struct OverlayBuild {
    pub collisions: Vec<batlehub_adapters::registry::mise_registry_overlay::CollisionReport>,
    pub upstream_invalid: Option<String>,
}

/// The last archive build of this process, read by the config page (†).
pub fn last_overlay_build() -> OverlayBuild {
    overlay_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last
        .clone()
}

/// `(sha256 of the raw archive, overlay hash)`.
type MemoKey = (Vec<u8>, Vec<u8>);

#[derive(Default)]
struct OverlayState {
    /// `(sha256 of the raw archive, overlay hash)` → merged bytes: one slot.
    memo: Option<(MemoKey, Arc<Vec<u8>>)>,
    last: OverlayBuild,
}

fn overlay_state() -> &'static std::sync::Mutex<OverlayState> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<OverlayState>> = std::sync::OnceLock::new();
    STATE.get_or_init(Default::default)
}

/// Single-flight for the archive fetch and the time of the last failure per
/// key: process-wide, not per actix worker (§6.4).
fn archive_gate(
) -> &'static tokio::sync::Mutex<std::collections::HashMap<String, std::time::Instant>> {
    static GATE: std::sync::OnceLock<
        tokio::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    > = std::sync::OnceLock::new();
    GATE.get_or_init(Default::default)
}

const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(60);
const ARCHIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The raw upstream archive, checked as mise checks it before it is cached,
/// so a truncated `200` or a captive portal never evicts a good stale copy.
async fn raw_archive(
    svc: &Arc<ProxyService>,
    client: &reqwest::Client,
    registry: &str,
    upstream: &str,
    offline: bool,
) -> Result<Vec<u8>, AppError> {
    use batlehub_adapters::registry::mise_registry_overlay as overlay;
    use batlehub_core::error::CoreError;
    use batlehub_core::services::proxy::{FetchOutcome, UpstreamBytes};

    let key = format!("mise-versions:{registry}:registry-archive:{upstream}");
    let url = format!("{upstream}/registry/latest.tar.zst");
    // Followers wait for the leader, then read what it cached.
    let mut gate = archive_gate().lock().await;
    let failed_recently = gate.get(&key).is_some_and(|t| t.elapsed() < NEGATIVE_TTL);
    let ttl = super::upstream::passthrough_ttl(svc, registry).await;
    let result = svc
        .cached_passthrough(registry, &key, Some(ttl), || async {
            if offline {
                return Err(CoreError::Registry(
                    "air gap: this instance does not dial out".to_owned(),
                ));
            }
            if failed_recently {
                return Err(CoreError::Registry(
                    "the archive upstream failed in the last 60 s".to_owned(),
                ));
            }
            // Any non-2xx is a failure, so the held copy is served (§6.4).
            let body = super::upstream::get_bounded(
                client,
                &url,
                ARCHIVE_TIMEOUT,
                overlay::MAX_COMPRESSED,
            )
            .await?;
            if let Err(e) = overlay::check(&body) {
                overlay_state()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .last
                    .upstream_invalid = Some(e.to_string());
                return Err(CoreError::Registry(format!("archive refused: {e}")));
            }
            Ok(FetchOutcome::Cacheable(UpstreamBytes {
                content_type: "application/zstd".to_owned(),
                body,
            }))
        })
        .await;
    match result {
        Ok(p) => Ok(p.bytes.body),
        Err(e) => {
            if !offline && !failed_recently {
                gate.insert(key, std::time::Instant::now());
            }
            drop(gate);
            // §15 Q3: an air-gapped instance with nothing cached serves the
            // archive a bundle carried in, checked as an upstream one is.
            if offline {
                if let Some(held) = held_archive(svc, registry).await {
                    return Ok(held);
                }
            }
            tracing::warn!(error = %e, "mise registry archive unavailable");
            Err(AppError::service_unavailable(format!(
                "the mise registry archive is unavailable: {e}"
            )))
        }
    }
}

/// The storage key a bundle files the raw archive under (§15 Q3): the
/// `artifact:` prefix every import writes, under the registry's own name.
pub fn held_archive_key(registry: &str) -> String {
    format!("artifact:{registry}/_registry/raw/latest.tar.zst")
}

/// The raw archive a bundle carried in, if one is stored and passes the same
/// check an upstream archive must pass. Anything else is `None`, so the route
/// keeps its `503`.
async fn held_archive(svc: &ProxyService, registry: &str) -> Option<Vec<u8>> {
    use batlehub_adapters::registry::mise_registry_overlay as overlay;
    use futures::StreamExt;

    let stored = match svc.storage.retrieve(&held_archive_key(registry)).await {
        Ok(Some(s)) => s,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "reading the held mise registry archive failed");
            return None;
        }
    };
    let mut body = Vec::new();
    let mut stream = stored.stream;
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > overlay::MAX_COMPRESSED {
            tracing::warn!("the held mise registry archive is over the size bound");
            return None;
        }
    }
    match overlay::check(&body) {
        Ok(_) => Some(body),
        Err(e) => {
            tracing::warn!(error = %e, "the held mise registry archive is invalid");
            None
        }
    }
}

#[derive(serde::Deserialize)]
pub struct ArchiveQuery {
    raw: Option<String>,
}

/// The registry archive mise floats, with this instance's overlay tools
/// merged in (§4.2). With the overlay off it is the public archive, checked
/// (§15 Q17). Air-gapped with nothing cached, it is the raw archive a bundle
/// carried in, merged with this instance's overlay (§15 Q3).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/mise-versions/_registry/latest.tar.zst",
    tag = "proxy/mise-versions",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("raw" = Option<String>, Query, description = "`1`: the upstream archive as fetched and checked, without the overlay — what `batlehub-cli mise export --include-mise-registry` carries"),
    ),
    responses(
        (status = 200, description = "mise's registry archive, the overlay merged in", body = UpstreamDocument, content_type = "application/zstd"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "auto_wire is not enabled on this registry"),
        (status = 503, description = "The upstream archive is unavailable and none is held"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/mise-versions/_registry/latest.tar.zst")]
pub async fn mise_versions_registry_archive(
    path: web::Path<String>,
    query: web::Query<ArchiveQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    client: web::Data<reqwest::Client>,
    hosts: Option<web::Data<crate::RegistryHostMap>>,
) -> Result<HttpResponse, AppError> {
    use batlehub_adapters::registry::mise_registry_overlay as overlay;
    use sha2::{Digest, Sha256};

    let registry = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    authorize(&svc, &registry, "registry", &identity).await?;
    let Some(wire) = wire_of(hosts.as_ref(), &registry) else {
        return Err(AppError::not_found(
            "auto_wire is not enabled on this registry",
        ));
    };
    let offline = air_gapped(&svc).await;
    let raw = raw_archive(
        &svc,
        &client,
        &registry,
        wire.auto_wire.registry_upstream(),
        offline,
    )
    .await?;
    let respond = |body: Vec<u8>| {
        HttpResponse::Ok()
            .content_type("application/zstd")
            .body(body)
    };
    // §15 Q3: the export carries the raw archive and each side merges its own
    // overlay. The header is the key a bundle files it under.
    if query.raw.as_deref() == Some("1") {
        return Ok(HttpResponse::Ok()
            .content_type("application/zstd")
            .insert_header(("X-BatleHub-Storage-Key", held_archive_key(&registry)))
            .body(raw));
    }
    if !wire.auto_wire.overlay || wire.overlay.is_empty() {
        return Ok(respond(raw));
    }
    let key = (
        Sha256::digest(&raw).to_vec(),
        Sha256::digest(format!("{:?}", wire.overlay).as_bytes()).to_vec(),
    );
    if let Some((k, bytes)) = &overlay_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .memo
    {
        if *k == key {
            return Ok(respond(bytes.as_ref().clone()));
        }
    }
    let checked = overlay::check(&raw).map_err(|e| {
        AppError::service_unavailable(format!("the held registry archive is invalid: {e}"))
    })?;
    let (entries, collisions) = overlay::merge(&checked, &wire.overlay)
        .map_err(|e| AppError::internal(format!("overlay entry: {e}")))?;
    let merged = overlay::pack(&entries)
        .map_err(|e| AppError::internal(format!("repacking the registry archive: {e}")))?;
    for resolution in [
        overlay::Collision::Drop,
        overlay::Collision::Overlay,
        overlay::Collision::Upstream,
    ] {
        let n = collisions
            .iter()
            .filter(|c| c.resolution == resolution)
            .count();
        metrics::gauge!("batlehub_mise_overlay_collisions", "resolution" => resolution.as_str())
            .set(n as f64);
    }
    for c in &collisions {
        tracing::warn!(
            tool = %c.tool,
            upstream_entry = %c.upstream_entry,
            on_collision = c.resolution.as_str(),
            "mise overlay: a tool name collides with the upstream registry"
        );
    }
    let merged = Arc::new(merged);
    {
        let mut st = overlay_state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.memo = Some((key, Arc::clone(&merged)));
        st.last = OverlayBuild {
            collisions,
            upstream_invalid: None,
        };
    }
    Ok(respond(merged.as_ref().clone()))
}

/// The config page's RFC 0045 rows (§4.3): what the table left out, the
/// overlay's static checks, and the last archive build of this replica (†).
pub fn mise_wire_warnings(
    wire: &crate::mise_wire::MiseWire,
    readable_anonymously: &dyn Fn(&str) -> bool,
) -> Vec<batlehub_config::schema::ConfigWarning> {
    use batlehub_config::schema::warnings as w;
    use batlehub_config::schema::ConfigWarning;
    use batlehub_core::services::mise_wire::{url_host, WireWarning};

    let at = |name: &str| {
        wire.indices.get(name).map_or_else(
            || format!("registries[{name}]"),
            |i| format!("registries[{i}]"),
        )
    };
    let tools = format!("registries[{}].auto_wire.tools", wire.index);
    let mut out = Vec::new();
    for warning in &wire.warnings {
        if let WireWarning::PrefixClaimed {
            registry,
            claimed_by,
            host,
            prefix,
        } = warning
        {
            out.push(ConfigWarning::new(
                w::MISE_WIRE_PREFIX_CLAIMED,
                format!("{}.upstreams", at(registry)),
                format!(
                    "{host}/{prefix} is claimed by {claimed_by} and {registry}; {claimed_by} wins."
                ),
            ));
        }
    }
    let wired_hosts: std::collections::BTreeSet<&str> =
        wire.table.wired().map(|(h, _, _)| h).collect();
    for (j, t) in wire.auto_wire.tools.iter().enumerate() {
        for b in &t.backends {
            let host = match b {
                batlehub_config::schema::mise_auto_wire::MiseBackendConfig::Full {
                    full,
                    options,
                } if full.starts_with("http:") => options
                    .get("url")
                    .and_then(|u| u.as_str())
                    .and_then(url_host),
                other if other.full().starts_with("github:") => Some("api.github.com".to_owned()),
                other if other.full().starts_with("gitlab:") => Some("gitlab.com".to_owned()),
                _ => None,
            };
            if let Some(h) = host.filter(|h| !wired_hosts.contains(h.as_str())) {
                out.push(ConfigWarning::new(
                    w::MISE_OVERLAY_BACKEND_UNWIRED,
                    format!("{tools}[{j}].backends"),
                    format!(
                        "{} names {h}, which is not wired: installs go there directly.",
                        b.full()
                    ),
                ));
            }
        }
        if let Some(src) = &t.source {
            if readable_anonymously(&wire.registry) && !readable_anonymously(&src.registry) {
                out.push(ConfigWarning::new(
                    w::MISE_OVERLAY_READERS_WIDER,
                    format!("{tools}[{j}].source.registry"),
                    format!(
                        "\"{}\" and its backend reach readers of {} who cannot read {}.",
                        t.name, wire.registry, src.registry
                    ),
                ));
            }
        }
    }
    let build = last_overlay_build();
    for c in &build.collisions {
        let Some(j) = wire.auto_wire.tools.iter().position(|t| t.name == c.tool) else {
            continue;
        };
        out.push(ConfigWarning::new(
            w::MISE_OVERLAY_COLLISION,
            format!("{tools}[{j}].name"),
            format!(
                "\"{}\" is in the upstream mise registry too (as \"{}\"); settled by on_collision = \"{}\".",
                c.tool,
                c.upstream_entry,
                c.resolution.as_str()
            ),
        ));
        if c.resolution == batlehub_adapters::registry::mise_registry_overlay::Collision::Overlay {
            out.push(
                ConfigWarning::new(
                    w::MISE_OVERLAY_FORCED_SHADOW,
                    format!("{tools}[{j}].on_collision"),
                    format!(
                        "\"{}\" shadows the public tool while on_collision = \"overlay\".",
                        c.upstream_entry
                    ),
                )
                .not_dismissible(),
            );
        }
    }
    if let Some(why) = &build.upstream_invalid {
        out.push(ConfigWarning::new(
            w::MISE_OVERLAY_UPSTREAM_INVALID,
            format!("registries[{}].auto_wire.registry_upstream", wire.index),
            format!("The last archive from {} failed mise's checks ({why}); the held copy, if any, is served.", wire.auto_wire.registry_upstream()),
        ));
    }
    out
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
