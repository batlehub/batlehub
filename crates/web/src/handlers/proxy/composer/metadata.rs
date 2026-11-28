use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    services::{LocalRegistryService, ProxyService},
};

use crate::handlers::proxy::common::{
    registry_public_base, require_registry_type, serve_local_or_proxy_artifact,
    serve_local_or_proxy_document, LocalOrProxyArtifactOpts,
};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap, UpstreamMap};
use batlehub_core::entities::Action;

// ── packages.json ─────────────────────────────────────────────────────────────

/// Composer registry root index.
///
/// Returns a `packages.json` that points Composer clients to our `p2/` endpoints.
/// In local/hybrid mode the response also includes `available-packages` listing
/// all locally published package names.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/packages.json",
    tag = "proxy/composer",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Composer packages.json index", body = UpstreamDocument),
        (status = 403, description = "Access denied by the registry's rule chain"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/packages.json")]
pub async fn composer_packages_json(
    req: HttpRequest,
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "composer", &map)?;

    // `available-packages` below names every package in the registry, so this
    // document is a complete inventory and the registry's RBAC has to govern it.
    // The identity used to be taken and dropped (`_identity`), which made the
    // one Composer endpoint that enumerates private package names the one with
    // no gate on it. The coordinate is the registry itself: there is no package
    // to name, and `repo` is the same synthetic name the path-proxy routes use.
    //
    // `authorize_listing`, not `authorize_read`: this document names many
    // packages and no single version, so only the identity-keyed `rbac` rule can
    // judge it. The full chain would be handed a coordinate whose metadata is
    // synthetic by construction — `published_at`, `is_signed` and the recorded
    // licence are all absent for the pseudo-package `repo@_` — and
    // `release_age.deny_missing_timestamp`, `require_signed_release` or
    // `license_gate.allow_unknown = false` would then refuse `packages.json`
    // outright, which breaks `composer install` for the whole registry rather
    // than gating anything. See `authz::authorize_listing`.
    svc.authorize_listing(
        &PackageId::registry_document(&registry, "repo", "_"),
        &identity.0,
        // §4.2 — this document names many packages and no single version, which
        // is `releases:list`'s definition.
        Action::ReleasesList,
    )
    .await
    .map_err(AppError::from)?;

    let base_url = registry_public_base(&req, &registry);
    let metadata_url = format!("{base_url}/p2/%package%.json");

    let mode = mode_map.get(&registry);
    // `available-packages` is an assertion that this list is the **complete**
    // contents of the repository, and Composer treats it as authoritative: a
    // package absent from it is not requested at all, whatever `metadata-url`
    // would have answered.
    //
    // So it may only be sent when the claim is true — `local` mode, where this
    // registry is the whole world. It used to be sent in every mode
    // (RFC 0009 §12.10):
    //
    // - **proxy** sent `[]`, which says "this repository is empty". Composer
    //   read that, never requested `p2/…`, and reported every package as
    //   "could not be found in any version". A proxy-mode Composer registry
    //   could not resolve anything at all.
    // - **hybrid** sent the locally published packages only, which says
    //   "upstream's packages do not exist here" — so hybrid could resolve what
    //   it hosted and nothing it proxied.
    //
    // Omitted, Composer falls back to asking `metadata-url` per package, which
    // is exactly what a proxy can answer.
    let available_packages: Option<Vec<String>> = if mode == RegistryMode::Local {
        Some(
            local_svc
                .get_composer_packages_list(&registry, &identity.0)
                .await
                .unwrap_or_default(),
        )
    } else {
        None
    };

    let mut body = serde_json::json!({
        "packages": [],
        "metadata-url": metadata_url,
        // Composer discovers every endpoint but `packages.json` itself from a
        // URL template here. Without these two it never requests `search.json`
        // or `list.json`, however correctly they are implemented — measured
        // against Composer 2.10.2, which is how phase 6's search route and
        // phase 7's list route were found to be unreachable (RFC 0009 §12.5).
        "search": format!("{base_url}/search.json?q=%query%&type=%type%"),
        "list": format!("{base_url}/list.json"),
    });

    if let (Some(list), Some(obj)) = (available_packages, body.as_object_mut()) {
        obj.insert("available-packages".to_owned(), serde_json::json!(list));
    }

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(body))
}

// ── p2 metadata ───────────────────────────────────────────────────────────────

/// Packagist v2 package metadata (all versions).
///
/// Handles both `{vendor}/{package}.json` and `{vendor}/{package}~dev.json`.
/// In proxy mode: fetched and cached from the upstream Packagist.
/// In local mode: built from locally published packages only.
/// In hybrid mode: local packages first, falling back to upstream.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/p2/{path}",
    tag = "proxy/composer",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path"     = String, Path, description = "Vendor/package path, e.g. symfony/console.json"),
    ),
    responses(
        (status = 200, description = "Packagist v2 metadata JSON", body = UpstreamDocument),
        (status = 404, description = "Package not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/p2/{path:.*}")]
pub async fn composer_p2_metadata(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, p2_path) = path.into_inner();
    require_registry_type(&registry, "composer", &map)?;

    // Parse the path: "vendor/package.json" or "vendor/package~dev.json".
    // The `~dev` suffix is significant — Packagist serves different JSON for dev variants,
    // so the cache key must distinguish them.
    let package_name = parse_p2_package_name(&p2_path)
        .ok_or_else(|| AppError::bad_request(format!("invalid Composer p2 path: '{p2_path}'")))?;
    let is_dev = p2_path.contains('~');
    let p2_artifact = if is_dev { "p2~dev" } else { "p2" };

    let base_url = registry_public_base(&req, &registry);
    let local_base_url = base_url.clone();

    // Use version="_index" so the artifact key is stable; p2_artifact encodes the ~dev variant.
    let pkg = PackageId::listing(&registry, &package_name, "_index").with_artifact(p2_artifact);
    let not_found_msg =
        format!("composer package '{package_name}' not found in local registry '{registry}'");
    let (fetch_registry, fetch_package_name) = (registry.clone(), package_name.clone());
    // A version listing, not an artifact. The proxy fall-through fetches and
    // filters the p2 document — expanding the `composer/2.0` minified encoding
    // first, because removing a middle entry from a minified list silently
    // changes what the entries after it inherit — and repoints `dist.url` at
    // this proxy.
    let doc_kind = if is_dev {
        batlehub_core::ports::DocumentKind::P2_DEV
    } else {
        batlehub_core::ports::DocumentKind::Versions
    };
    serve_local_or_proxy_document(
        svc,
        &mode_map,
        &registry,
        identity,
        move |identity: batlehub_core::entities::Identity| async move {
            local_svc
                .get_composer_p2_response(
                    &fetch_registry,
                    &fetch_package_name,
                    &local_base_url,
                    &identity,
                )
                .await
        },
        not_found_msg,
        pkg,
        Action::ReleasesRead,
        doc_kind,
        "application/json",
        base_url,
    )
    .await
}

// ── dist artifact download ────────────────────────────────────────────────────

/// Download a Composer package ZIP artifact.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/dist/{vendor}/{package}/{version}",
    tag = "proxy/composer",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("vendor"   = String, Path, description = "Vendor name"),
        ("package"  = String, Path, description = "Package name"),
        ("version"  = String, Path, description = "Version string"),
    ),
    responses(
        (status = 200, description = "Package ZIP artifact", body = ArtifactBytes, content_type = "application/zip"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Package or version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/dist/{vendor}/{package}/{version}")]
pub async fn composer_dist(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, vendor, package, version) = path.into_inner();
    require_registry_type(&registry, "composer", &map)?;

    let name = format!("{vendor}/{package}");

    serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &name,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: "dist",
            local_content_type: "application/zip",
            proxy_content_type: Some("application/zip"),
            action: Action::ReleasesRead,
            check_prerelease: true,
            append_signature: false,
        },
    )
    .await
}

// ── Security advisories ───────────────────────────────────────────────────────

/// Proxy Composer security advisory queries to the upstream Packagist server.
///
/// `composer audit` calls `GET /api/security-advisories/?packages[]=vendor/pkg&…`
/// against each configured repository. This handler forwards the request (including
/// all query parameters) to the upstream Packagist URL so `composer audit` works
/// without direct internet access to packagist.org.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/security-advisories/",
    tag = "proxy/composer",
    params(
        ("registry" = String, Path, description = "Registry name (must be a composer registry)"),
    ),
    responses(
        (status = 200, description = "Security advisory JSON from upstream", body = UpstreamDocument),
        (status = 404, description = "Registry not found or not a Composer registry"),
        (status = 502, description = "Upstream Packagist error"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/security-advisories/")]
pub async fn composer_security_advisories(
    req: HttpRequest,
    path: web::Path<String>,
    _identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<UpstreamMap>,
    svc: web::Data<Arc<ProxyService>>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "composer", &map)?;

    let upstream = upstream_map
        .upstream_for(&registry)
        .ok_or_else(|| AppError::not_found(format!("no upstream configured for '{registry}'")))?;

    // Forward query string (?packages[]=vendor/pkg&...) verbatim.
    let query = req.query_string();
    let url = if query.is_empty() {
        format!("{upstream}/api/security-advisories/")
    } else {
        format!("{upstream}/api/security-advisories/?{query}")
    };

    // Through the §4.2 helper like every other advisory passthrough: a
    // `composer audit` whose upstream is unreachable is answered from cache
    // rather than failed, for the same reason `npm audit` is.
    let key = format!("advisories:{registry}:{query}");
    crate::handlers::proxy::upstream::cached_forward(
        &svc,
        &client,
        &registry,
        &key,
        crate::handlers::proxy::upstream::Outbound::get(url),
    )
    .await
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the `vendor/package` name from a p2 path like:
/// - `"vendor/package.json"` → `"vendor/package"`
/// - `"vendor/package~dev.json"` → `"vendor/package"`
///
/// Rejects paths whose vendor or package segments contain characters outside
/// `[a-z0-9A-Z_.-]` to prevent path-traversal and injection.
pub(crate) fn parse_p2_package_name(path: &str) -> Option<String> {
    // Must contain exactly one slash (vendor/package[~dev].json)
    let (vendor, rest) = path.split_once('/')?;
    if vendor.is_empty() || !is_valid_composer_segment(vendor) {
        return None;
    }
    // Strip .json, then optionally the `~dev` suffix. Only `~dev` is a valid
    // Packagist v2 variant; any other tilde sequence is rejected to prevent
    // silent truncation (e.g. `pkg~evil.json` being served as `pkg`).
    let without_json = rest.strip_suffix(".json")?;
    let package = if let Some((base, suffix)) = without_json.split_once('~') {
        if suffix != "dev" {
            return None;
        }
        base
    } else {
        without_json
    };
    if package.is_empty() || !is_valid_composer_segment(package) {
        return None;
    }
    Some(format!("{vendor}/{package}"))
}

/// Returns `true` when every character in `s` is a safe Composer name segment
/// character: ASCII alphanumeric, hyphen, underscore, or dot.
fn is_valid_composer_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_p2_normal_package() {
        assert_eq!(
            parse_p2_package_name("symfony/console.json"),
            Some("symfony/console".to_owned())
        );
    }

    #[test]
    fn parse_p2_dev_variant() {
        assert_eq!(
            parse_p2_package_name("symfony/console~dev.json"),
            Some("symfony/console".to_owned())
        );
    }

    #[test]
    fn parse_p2_unknown_tilde_suffix_rejected() {
        // Only `~dev` is valid; any other suffix must be rejected, not truncated.
        assert_eq!(parse_p2_package_name("vendor/pkg~2.json"), None);
        assert_eq!(parse_p2_package_name("vendor/pkg~evil.json"), None);
        assert_eq!(parse_p2_package_name("vendor/pkg~beta.json"), None);
    }

    #[test]
    fn parse_p2_no_slash_returns_none() {
        assert_eq!(parse_p2_package_name("console.json"), None);
    }

    #[test]
    fn parse_p2_empty_returns_none() {
        assert_eq!(parse_p2_package_name(""), None);
    }

    #[test]
    fn parse_p2_wrong_extension_returns_none() {
        assert_eq!(parse_p2_package_name("vendor/pkg.txt"), None);
    }

    #[test]
    fn parse_p2_empty_vendor_returns_none() {
        assert_eq!(parse_p2_package_name("/pkg.json"), None);
    }

    #[test]
    fn parse_p2_empty_package_returns_none() {
        assert_eq!(parse_p2_package_name("vendor/.json"), None);
    }

    #[test]
    fn parse_p2_path_traversal_rejected() {
        assert_eq!(parse_p2_package_name("../etc/pkg.json"), None);
        assert_eq!(parse_p2_package_name("vendor/../etc.json"), None);
    }
}
