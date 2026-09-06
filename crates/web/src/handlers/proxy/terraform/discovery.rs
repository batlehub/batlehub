//! Terraform service discovery and the provider network mirror.
//!
//! RFC 0009 §7.2. Two protocols, because they are not alternatives:
//!
//! - The **registry protocol** (`/v1/modules/…`, `/v1/providers/…`) is what the
//!   `/v1/` routes already implement, and it was unreachable: Terraform finds a
//!   registry's endpoints by fetching `https://{host}/.well-known/terraform.json`,
//!   and nothing served that. Without it Terraform assumes the default `/v1/`
//!   prefix at the host root, which is not where a path-routed BatleHub keeps
//!   them.
//! - The **provider network mirror** is a different protocol entirely — two JSON
//!   documents, no discovery, providers only. It works under path routing, and
//!   it is what `docs/registries/terraform.md` told operators to configure while
//!   the code implemented the other one.
//!
//! ## Why discovery is host-routed only
//!
//! `.well-known/terraform.json` is fetched from the *host root* by the protocol.
//! A path-routed deployment reaches many registries under one host, so a
//! discovery document served there could not say which registry it describes —
//! it would have to pick one, and be wrong for the rest. So it answers only when
//! the request arrived on a host bound to exactly one registry
//! (`host_routed_registry`), and 404s with the reason otherwise.
//!
//! Host routing shipped with RFC 0001, so this is configuration rather than new
//! machinery. It is also what makes a legal source address possible:
//! `tf.example.com/myorg/mycloud` is the three segments Terraform requires,
//! where `batlehub.example.com/proxy/internal-tf/myorg/mycloud` is five and is
//! rejected before a request is made.

use super::shared::{sign_artifact_url, signer_for};
use super::{get, require_registry_type, web, AppError, Arc, AuthIdentity, HttpRequest, Responder};
use crate::handlers::schemas::ProtocolDocument;
use crate::middleware::host_routing::host_routed_registry;
use crate::RegistryMap;

use actix_web::HttpResponse;
use batlehub_core::entities::Action;
use batlehub_core::{entities::PackageId, services::ProxyService};

/// `GET /.well-known/terraform.json` — the document Terraform reads first.
///
/// Served at the host root, not under `/proxy/{registry}/`, because that is
/// where the protocol looks for it.
#[utoipa::path(
    get,
    path = "/.well-known/terraform.json",
    tag = "proxy/terraform",
    responses(
        (status = 200, description = "Terraform service discovery document", body = ProtocolDocument),
        (status = 404, description = "This host is not bound to a single Terraform registry"),
    ),
)]
#[get("/.well-known/terraform.json")]
pub async fn terraform_discovery(
    req: HttpRequest,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    discovery_document(&req, &map)
}

/// The same document at the path the host-routing middleware actually produces.
///
/// This route is not a second public address; it is where the *first* one ends
/// up. The middleware rewrites every request on a vanity host to
/// `/proxy/{registry}{path}` before routing (see `middleware/host_routing.rs`),
/// so a request for `/.well-known/terraform.json` on `tf.example.com` never
/// reaches a route registered at the host root — it is matched, and was matched,
/// by the npm/cargo catch-all `/proxy/{registry}/{package}/{version}`, which
/// answered *"registry 'x' is not an npm or cargo registry"*.
///
/// Discovery is host-routed only, and host routing is the one condition under
/// which the host-root route cannot match: it was unreachable everywhere it was
/// meant to work. Measured against Terraform 1.8.5 through a TLS front on a
/// vanity host (RFC 0009 §12.11).
///
/// The `host_routed_registry` guard still applies, and the registry name in the
/// path is deliberately ignored in favour of it — so this does not become a way
/// to ask a shared host which registry it would like to be.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/.well-known/terraform.json",
    tag = "proxy/terraform",
    params(("registry" = String, Path, description = "Registry name (ignored: the host decides)")),
    responses(
        (status = 200, description = "Terraform service discovery document, as reached through host routing", body = ProtocolDocument),
        (status = 404, description = "This request did not arrive on a host bound to a single Terraform registry"),
    ),
)]
#[get("/proxy/{registry}/.well-known/terraform.json")]
pub async fn terraform_discovery_host_routed(
    req: HttpRequest,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    discovery_document(&req, &map)
}

fn discovery_document(
    req: &HttpRequest,
    map: &web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    let registry = host_routed_registry(req).ok_or_else(|| {
        AppError::not_found(
            "Terraform service discovery is only available on a host bound to a single \
             registry. Configure [subdomain_routing] or a vanity host for this registry \
             (RFC 0001), then point Terraform at that host."
                .to_owned(),
        )
    })?;
    require_registry_type(&registry, "terraform", map)?;

    // Relative to the host root, which is where discovery was fetched from — so
    // these resolve correctly whether the host is a wildcard subdomain or a
    // vanity name, without this document having to know which.
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(serde_json::json!({
            "modules.v1": "/v1/modules/",
            "providers.v1": "/v1/providers/",
        })))
}

/// `GET {mirror}/{hostname}/{namespace}/{type}/index.json` — the versions a
/// network mirror offers for one provider.
///
/// The mirror protocol's own shape: a bare `versions` object whose keys are
/// version strings and whose values are (currently empty) objects.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{hostname}/{namespace}/{ptype}/index.json",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("hostname"  = String, Path, description = "Origin registry hostname, e.g. registry.terraform.io"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
    ),
    responses(
        (status = 200, description = "Mirror version index", body = ProtocolDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or hostname does not match this registry's upstream"),
    ),
    security(("bearer_token" = [])),
)]
// `{hostname}` must contain a dot. Without that constraint this pattern
// claims any four-segment path ending in `index.json` — and it did: it
// swallowed RubyGems' `/api/v1/versions/{gem}.json` as
// host="api", ns="v1", type="versions", version="{gem}". A registry
// hostname always has a dot; `api`, `v1` and `v3` never do.
#[get(r"/proxy/{registry}/{hostname:[^/]+\.[^/]+}/{namespace}/{ptype}/index.json")]
pub async fn terraform_mirror_index(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<crate::UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let (registry, hostname, namespace, ptype) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;
    require_matching_origin(&registry, &hostname, &upstream_map)?;

    let versions = mirror_versions(&svc, &registry, &namespace, &ptype, identity).await?;

    let map_out: serde_json::Map<String, serde_json::Value> = versions
        .into_iter()
        .map(|v| (v, serde_json::json!({})))
        .collect();
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(serde_json::json!({ "versions": map_out })))
}

/// `GET {mirror}/{hostname}/{namespace}/{type}/{version}.json` — where one
/// version's archives live.
///
/// `url` is **relative to this document**, which is what makes the mirror
/// protocol immune to the problem §7.2 had to fix on the registry protocol: the
/// download target points back at our own artifact route by construction, so
/// there is no upstream URL to leak and no gate to bypass.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{hostname}/{namespace}/{ptype}/{version}.json",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("hostname"  = String, Path, description = "Origin registry hostname"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
        ("version"   = String, Path, description = "Provider version"),
    ),
    responses(
        (status = 200, description = "Mirror archive list for one version", body = ProtocolDocument),
        (status = 403, description = "Access denied, or this version is blocked"),
        (status = 404, description = "Unknown registry, hostname mismatch, or unknown version"),
    ),
    security(("bearer_token" = [])),
)]
// Same dot constraint, and for the same regression — see the index route.
#[get(r"/proxy/{registry}/{hostname:[^/]+\.[^/]+}/{namespace}/{ptype}/{version}.json")]
pub async fn terraform_mirror_version(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<crate::UpstreamMap>,
) -> Result<impl Responder, AppError> {
    let (registry, hostname, namespace, ptype, version) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;
    require_matching_origin(&registry, &hostname, &upstream_map)?;

    // Kept because `mirror_versions` consumes the extractor, and a signed URL
    // has to carry the identity the rule chain just approved.
    let caller = identity.0.clone();

    // The version list is filtered, so asking it whether this version survives
    // is how a blocked version is hidden from the mirror as well: no separate
    // filter, and no way for the two documents to disagree.
    let versions = mirror_versions(&svc, &registry, &namespace, &ptype, identity).await?;
    if !versions.iter().any(|v| v == &version) {
        return Err(AppError::not_found(format!(
            "provider {namespace}/{ptype} {version} is not available from this mirror"
        )));
    }

    // RFC 0012 §5: this is a legitimate minting site precisely because
    // `mirror_versions` above has already run the rule chain for this provider
    // as this caller, and the version has survived the filtered list. The
    // signature below records that verdict rather than creating one.
    let signer = signer_for(&svc, &registry).await;
    let pkg_name = format!("providers/{namespace}/{ptype}");

    // Relative to *this* document — `{version}.json` sits beside the platform
    // archives in the mirror's namespace, so `../` walks back to the provider
    // root that the registry-protocol artifact route hangs off. A query string
    // rides along unharmed: a relative reference resolves the path and keeps
    // the query it was written with (RFC 0009 §12.3, RFC 0012 §4.3).
    let archives: serde_json::Map<String, serde_json::Value> = MIRROR_PLATFORMS
        .iter()
        .map(|(os, arch)| {
            let url =
                format!("../../../v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}");
            let url = match signer.as_ref() {
                Some(signer) => sign_artifact_url(
                    signer,
                    &url,
                    &registry,
                    &pkg_name,
                    &version,
                    &format!("{os}/{arch}"),
                    &caller,
                ),
                None => url,
            };
            (format!("{os}_{arch}"), serde_json::json!({ "url": url }))
        })
        .collect();

    // Every URL above carries `bh_sig` when the registry signs, and `bh_sig`
    // encodes *this* caller's id, role and groups — so the document stops being
    // the same for everyone the moment signing is on. Nothing here sets a
    // validator or an expiry, which is exactly the shape a shared cache is
    // allowed to guess a freshness lifetime for (RFC 9111 §4.2.2); without this
    // it could hand one user's capability to the next.
    let mut resp = HttpResponse::Ok();
    resp.content_type("application/json");
    super::mark_uncacheable_if_signed(&mut resp, signer.is_some());
    Ok(resp.json(serde_json::json!({ "archives": archives })))
}

/// The platforms a mirror advertises.
///
/// The mirror protocol has no "ask upstream which platforms exist" call — the
/// document simply lists what the mirror holds. Advertising the common set and
/// letting an absent one 404 at download is the protocol's own failure mode; the
/// alternative, probing every platform upstream to build one index, would turn a
/// single request into a dozen.
const MIRROR_PLATFORMS: &[(&str, &str)] = &[
    ("linux", "amd64"),
    ("linux", "arm64"),
    ("darwin", "amd64"),
    ("darwin", "arm64"),
    ("windows", "amd64"),
];

/// The filtered version list for one provider, through the normal listing path.
async fn mirror_versions(
    svc: &Arc<ProxyService>,
    registry: &str,
    namespace: &str,
    ptype: &str,
    identity: AuthIdentity,
) -> Result<Vec<String>, AppError> {
    let name = format!("providers/{namespace}/{ptype}");
    let req = batlehub_core::services::ProxyRequest {
        package_id: PackageId::new(registry, &name, "versions"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let doc = svc
        .version_document(&req, batlehub_core::ports::DocumentKind::Versions, "")
        .await
        .map_err(AppError::from)?;

    let json = doc
        .body
        .as_json()
        .ok_or_else(|| AppError::bad_gateway("terraform version document was not JSON"))?;

    Ok(json
        .get("versions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("version").and_then(|v| v.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

/// The mirror path carries the *origin* registry's hostname so one mirror can
/// serve several registries. We are one upstream per registry, so the segment is
/// redundant — but redundant is not ignorable (RFC 0009 §11.1): echoing it back
/// unchecked would attach an `example.com` provenance to a
/// `registry.terraform.io` provider.
fn require_matching_origin(
    registry: &str,
    hostname: &str,
    upstream_map: &crate::UpstreamMap,
) -> Result<(), AppError> {
    let upstream = upstream_map
        .upstream_for(registry)
        .ok_or_else(|| AppError::not_found(format!("no upstream configured for '{registry}'")))?;
    let upstream_host = reqwest::Url::parse(&upstream)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();

    if upstream_host.eq_ignore_ascii_case(hostname) {
        return Ok(());
    }
    Err(AppError::not_found(format!(
        "registry '{registry}' mirrors '{upstream_host}', not '{hostname}'"
    )))
}
