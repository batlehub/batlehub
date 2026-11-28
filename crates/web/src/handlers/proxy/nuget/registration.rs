use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    ports::DocumentKind,
    services::{LocalRegistryService, ProxyService},
};

use super::super::common::{proxy_document, registry_public_base, require_registry_type};
use crate::handlers::schemas::UpstreamDocument;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

/// Return NuGet v3 registration metadata for a package.
///
/// In `local` mode this is generated from the DB. In proxy/hybrid mode it is
/// fetched from the upstream registration API.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nuget/v3/registration5/{id}/index.json",
    tag = "proxy/nuget",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id"       = String, Path, description = "Package ID"),
    ),
    responses(
        (status = 200, description = "Registration index JSON", body = UpstreamDocument),
        (status = 404, description = "Package not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nuget/v3/registration5/{id}/index.json")]
pub async fn nuget_registration(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, id_raw) = path.into_inner();
    require_registry_type(&registry, "nuget", &map)?;

    let id = id_raw.to_lowercase();
    let mode = mode_map.get(&registry);

    if mode == RegistryMode::Local {
        // Enforce registry RBAC before the local read (proxy path runs the rule
        // chain via `proxy_stream`; a local hit otherwise bypasses it).
        svc.authorize_read(
            &PackageId::listing(&registry, &id, "__registration__"),
            &identity.0,
            Action::ReleasesRead,
        )
        .await
        .map_err(AppError::from)?;
        let versions = local_svc
            .get_nuget_versions(&registry, &id, &identity)
            .await
            .map_err(AppError::from)?;

        let base = registry_public_base(&req, &registry);

        let items: Vec<serde_json::Value> = versions
            .iter()
            .filter(|v| !v.yanked)
            .map(|v| {
                let pkg_content = format!(
                    "{base}/nuget/v3/flat/{id}/{}/{id}.{}.nupkg",
                    v.version, v.version
                );
                let published = v.published_at.to_rfc3339();
                let original_id = v
                    .index_metadata
                    .get("id")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&id);
                let description = v
                    .index_metadata
                    .get("description")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let authors = v
                    .index_metadata
                    .get("authors")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");

                serde_json::json!({
                    "@id": format!("{base}/nuget/v3/registration5/{id}/{}.json", v.version),
                    "catalogEntry": {
                        "@id": format!("{base}/nuget/v3/registration5/{id}/{}.json", v.version),
                        "@type": "PackageDetails",
                        "id": original_id,
                        "version": v.version,
                        "description": description,
                        "authors": authors,
                        "listed": true,
                        "published": published
                    },
                    "packageContent": pkg_content
                })
            })
            .collect();

        let lower = versions.first().map(|v| v.version.as_str()).unwrap_or("");
        let upper = versions.last().map(|v| v.version.as_str()).unwrap_or("");

        let response = serde_json::json!({
            "@id": format!("{base}/nuget/v3/registration5/{id}/index.json"),
            "count": 1,
            "items": [{
                "@id": format!("{base}/nuget/v3/registration5/{id}/page/{lower}/{upper}.json"),
                "lower": lower,
                "upper": upper,
                "count": items.len(),
                "items": items
            }]
        });

        return Ok(HttpResponse::Ok()
            .content_type("application/json")
            .json(response));
    }

    // Proxy or hybrid mode: forward to upstream registration.
    // A listing, not an artifact. Registration leaves are filtered and each
    // page's `count`/`lower`/`upper` recomputed, so a UI or `dotnet list
    // package` never advertises a version the download gate will refuse.
    //
    // Registrations whose pages are served by URL rather than inline pass
    // through unfiltered and are logged; the flat index — what `dotnet restore`
    // actually resolves a version against — is filtered either way.
    proxy_document(
        svc,
        PackageId::listing(&registry, &id, "__registration__"),
        identity,
        Action::ReleasesRead,
        DocumentKind::REGISTRATION,
        String::new(),
    )
    .await
}
