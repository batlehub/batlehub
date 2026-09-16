use actix_web::http::StatusCode;

use super::{
    get, proxy_document, registry_public_base, require_cargo, serve_local_or_proxy_artifact, web,
    AppError, Arc, AuthIdentity, CargoIndexMap, CoreError, HttpRequest, HttpResponse,
    LocalOrProxyArtifactOpts, LocalRegistryService, ProxyService, RegistryMap, RegistryMode,
    RegistryModeMap, Responder,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use batlehub_core::entities::{Action, Identity, PackageId};

/// Cargo sparse registry `config.json`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/registry/config.json",
    tag = "proxy/cargo",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Sparse registry configuration", body = UpstreamDocument),
        (status = 404, description = "No cargo registry configured"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/registry/config.json")]
pub async fn cargo_registry_config(
    path: web::Path<String>,
    indexes: web::Data<CargoIndexMap>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    svc: web::Data<Arc<ProxyService>>,
    req: HttpRequest,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    if !map.is_type(&registry, "cargo") {
        return Err(AppError::not_found(format!(
            "unknown cargo registry '{registry}'"
        )));
    }

    let mode = mode_map.get(&registry);

    // Proxy and Hybrid modes require a configured upstream index.
    if matches!(mode, RegistryMode::Proxy | RegistryMode::Hybrid)
        && indexes.get(&registry).is_none()
    {
        return Err(AppError::not_found("no cargo index configured"));
    }

    let base = registry_public_base(&req, &registry);
    let dl = format!("{base}/{{crate}}/{{version}}/download");
    let mut resp = serde_json::json!({ "dl": dl });

    // Expose the publish API URL for local and hybrid registries.
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        resp["api"] = serde_json::Value::String(base);
    }

    // ── `auth-required` ──────────────────────────────────────────────────────
    //
    // The registry index reference defines it as marking "a private registry
    // that requires all operations to be authenticated including API requests,
    // crate downloads and sparse index updates", and that is the only switch
    // there is: **without it cargo sends no credential on a read at all.** A
    // registry that refuses anonymous callers was therefore not "authenticated"
    // to cargo, it was unusable — the token went out on `cargo publish` and on
    // nothing else.
    //
    // Derived rather than always-on, because the field is a claim about the
    // whole registry and setting it on an open one would break anonymous cargo
    // for no reason: an anonymous caller would be made to present a credential
    // it does not have. So the question asked here is the one the field
    // answers — *can an anonymous caller read this registry?* — and the answer
    // comes from the same authorization chain a download will consult.
    //
    // The coordinate is synthetic (`_@_`) because the question is about the
    // registry tier, which is what cargo is asking about: it reads this file
    // once, before it knows which crate it wants. A grant that re-opens one
    // package to `*` under a closed tier is therefore invisible here and the
    // registry is advertised as closed — `cargo_auth_required` in the registry
    // config is the operator's override for exactly that case.
    let explicit = {
        svc.hot
            .read()
            .await
            .cargo_auth_required
            .get(&registry)
            .copied()
    };
    let auth_required = match explicit {
        Some(required) => required,
        None => svc
            .authorize_read(
                &PackageId::new(&registry, "_", "_"),
                &Identity::anonymous(),
                Action::SourceRead,
            )
            .await
            .is_err(),
    };
    if auth_required {
        resp["auth-required"] = serde_json::Value::Bool(true);
    }

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(resp))
}

/// Cargo sparse registry index entries.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/registry/{path}",
    tag = "proxy/cargo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path"     = String, Path, description = "Crate index path, e.g. se/rd/serde"),
    ),
    responses(
        (status = 200, description = "Sparse index entry (newline-delimited JSON)", body = ProtocolDocument, content_type = "text/plain"),
        (status = 404, description = "Crate not found in index"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/registry/{path:.*}")]
pub async fn cargo_registry_index(
    path: web::Path<(String, String)>,
    indexes: web::Data<CargoIndexMap>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    identity: AuthIdentity,
) -> Result<impl Responder, AppError> {
    let (registry, index_path) = path.into_inner();
    if !map.is_type(&registry, "cargo") {
        return Err(AppError::not_found(format!(
            "unknown cargo registry '{registry}'"
        )));
    }

    let mode = mode_map.get(&registry);

    match mode {
        RegistryMode::Local => {
            serve_local_index(&local_svc, &registry, &index_path, &identity).await
        }
        RegistryMode::Hybrid => {
            match serve_local_index(&local_svc, &registry, &index_path, &identity).await {
                Err(e) if e.status == StatusCode::NOT_FOUND => {
                    proxy_upstream_index(svc, &indexes, &registry, &index_path, identity).await
                }
                other => other,
            }
        }
        RegistryMode::Proxy => {
            proxy_upstream_index(svc, &indexes, &registry, &index_path, identity).await
        }
    }
}

/// The crate name a sparse-index path addresses.
///
/// The layout is `{prefix1}/{prefix2}/{name}` for names of 4+ characters and
/// `{len}/{name}` for shorter ones, so the name is always the final component.
/// `splitn(3, '/')` keeps slashes inside the name intact — a name decoded from
/// `scope%2Fpkg` stays `scope/pkg` rather than being truncated to `pkg`.
fn crate_name_from_index_path(index_path: &str) -> &str {
    index_path.splitn(3, '/').last().unwrap_or(index_path)
}

async fn serve_local_index(
    local_svc: &LocalRegistryService,
    registry: &str,
    index_path: &str,
    identity: &batlehub_core::entities::Identity,
) -> Result<HttpResponse, AppError> {
    let name = crate_name_from_index_path(index_path);
    match local_svc.get_index(registry, name, identity).await {
        Ok(content) => Ok(HttpResponse::Ok()
            .content_type("text/plain; charset=utf-8")
            .body(content)),
        Err(CoreError::NotFound(_)) => Err(AppError::not_found(format!(
            "crate '{name}' not found in local registry"
        ))),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Serve a crate's sparse-index entry from upstream, through `ProxyService`.
///
/// This route used to answer with a bare `reqwest` GET forwarded to the client:
/// no rule chain, no access event, no metadata cache. Blocked-version filtering
/// is why it moved, but the **authorisation** gap was the more serious finding
/// — a private cargo registry's crate names and versions were readable by
/// anyone who could reach the port. Both are closed by the same change, and a
/// client that was relying on the unauthenticated read will now get a `403`.
///
/// Blocked versions are marked `yanked` rather than dropped; see
/// `blocking::cargo`.
async fn proxy_upstream_index(
    svc: web::Data<Arc<ProxyService>>,
    indexes: &CargoIndexMap,
    registry: &str,
    index_path: &str,
    identity: AuthIdentity,
) -> Result<HttpResponse, AppError> {
    // The map is still the record of *whether* an index is configured; the URL
    // itself now lives on the registry client, which is what fetches it.
    if indexes.get(registry).is_none() {
        return Err(AppError::not_found("no cargo registry configured"));
    }
    let name = crate_name_from_index_path(index_path);
    proxy_document(
        svc,
        batlehub_core::entities::PackageId::new(registry, name, "__index__"),
        identity,
        Action::ReleasesRead,
        batlehub_core::ports::DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// Download a `.crate` file for a specific version.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{name}/{version}/download",
    tag = "proxy/cargo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Crate name"),
        ("version"  = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = ".crate file stream", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{name}/{version}/download")]
pub async fn download_crate(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    require_cargo(&registry, &map)?;

    let mut resp = serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &name,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: "dl",
            local_content_type: "application/octet-stream",
            proxy_content_type: None,
            action: Action::SourceRead,
            check_prerelease: true,
            append_signature: true,
        },
    )
    .await?;
    // The route ends in `/download`, so a browser saving it writes a file called
    // `download`. cargo does not read this header; the console's link does.
    resp.headers_mut().insert(
        actix_web::http::header::CONTENT_DISPOSITION,
        crate::handlers::proxy::common::attachment_disposition(&format!("{name}-{version}.crate"))?,
    );
    Ok(resp)
}
