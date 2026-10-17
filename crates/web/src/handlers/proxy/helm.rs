//! Classic Helm chart repositories (RFC 0029).
//!
//! Everything is under `/proxy/{registry}/helm/`, the URL `helm repo add`
//! takes:
//!
//! - **`index.yaml`** — the enforcement chokepoint. `helm` resolves every
//!   `repo/chart` reference through its cached copy, so a version absent from
//!   it fails on helm's own *"no chart version found"*. Blocked versions are
//!   removed registry-wide (`dispatch_multi`), every `urls` entry is rewritten
//!   to `charts/{name}-{version}.tgz` on this host — which is also the host
//!   `helm` sends the repository's basic auth to — and `oci://` entries follow
//!   `oci_entries` (§4.4). Local and hybrid compose it from rows.
//! - **`charts/{file}`** — the archive or its `.prov`. The coordinate is the
//!   file name; the upstream URL is looked up in the upstream index, never
//!   taken from the request.
//! - **`api/charts…`** — ChartMuseum's API, local and hybrid only: the publish
//!   `helm cm-push` makes, the delete, and JSON listings composed from the same
//!   rows as the index.
//!
//! Every route carries [`is_helm`] as a guard, so it matches only on a helm
//! registry — without it `…/helm/index.yaml` would be an npm package called
//! `helm` at version `index.yaml` to the npm route below it.

use std::sync::Arc;

use actix_multipart::Multipart;
use actix_web::{
    delete, guard::GuardContext, post, route, web, HttpRequest, HttpResponse, Responder,
};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{Action, PackageId},
    error::CoreError,
    ports::DocumentKind,
    services::helm::{
        chart_file_name, coordinate_of, index_metadata_from_chart_yaml, render_index, rewrite,
        validate_chart_name, validate_version, INDEX_ADDRESS, PROV_SIGNATURE_TYPE,
    },
    services::{local_registry::PublishRequest, LocalRegistryService, ProxyRequest, ProxyService},
};

use super::common::{
    collect_payload, proxy_stream, publish_and_respond, registry_public_base, require_local_mode,
    serve_local_or_proxy_artifact, synthesised_headers, LocalOrProxyArtifactOpts, MAX_UPLOAD_BYTES,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use crate::middleware::extract_registry_from_path;
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};

const KIND: &str = "helm";
const YAML: &str = "text/yaml; charset=utf-8";
const GZIP: &str = "application/gzip";
const PROV: &str = "application/octet-stream";

/// Match only on a helm registry — devfile's guard, one kind over.
pub fn is_helm(ctx: &GuardContext) -> bool {
    let Some(map) = ctx.app_data::<web::Data<RegistryMap>>() else {
        return false;
    };
    extract_registry_from_path(ctx.head().uri.path()).is_some_and(|r| map.is_type(r, KIND))
}

/// `POST api/charts` — what ChartMuseum answers and `helm cm-push` reads the status of.
#[derive(Debug, Serialize, ToSchema)]
pub struct HelmSaved {
    pub saved: bool,
}

/// `DELETE api/charts/{name}/{version}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct HelmDeleted {
    pub deleted: bool,
}

/// The index as this caller may see it, before the URL rewrite: local rows,
/// the filtered upstream index, or both with local winning.
async fn index_value(
    registry: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
    mode: RegistryMode,
) -> Result<(Value, Option<u32>), AppError> {
    let pkg = PackageId::new(registry, INDEX_ADDRESS, "latest");
    if mode == RegistryMode::Local {
        svc.authorize_read(&pkg, &identity.0, Action::ReleasesList)
            .await
            .map_err(AppError::from)?;
        let local = local_svc
            .get_helm_index(registry, &identity.0)
            .await
            .map_err(AppError::from)?;
        return Ok((local, None));
    }
    let preq = ProxyRequest {
        package_id: pkg,
        identity: identity.0.clone(),
        action: Action::ReleasesList.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let doc = svc
        .multi_package_document(&preq, DocumentKind::Versions, "")
        .await
        .map_err(AppError::from)?;
    let synthesised = doc.synthesised;
    let mut index = doc
        .body
        .as_json()
        .cloned()
        .ok_or_else(|| AppError::bad_gateway("the upstream index.yaml did not parse"))?;
    if mode == RegistryMode::Hybrid {
        let local = local_svc
            .get_helm_index(registry, &identity.0)
            .await
            .map_err(AppError::from)?;
        batlehub_core::services::helm::merge(&mut index, &local);
    }
    Ok((index, synthesised))
}

/// The repository index `helm repo add` and `helm repo update` fetch — blocked versions removed, every chart URL on this host.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/helm/index.yaml",
    tag = "proxy/helm",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "index.yaml, filtered and rewritten", body = ProtocolDocument, content_type = "text/yaml"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
        (status = 502, description = "The upstream index could not be read"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/helm/index.yaml",
    method = "GET",
    method = "HEAD",
    guard = "is_helm"
)]
pub async fn helm_index(
    req: HttpRequest,
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    let (mut index, synthesised) =
        index_value(&registry, &identity, &svc, &local_svc, modes.get(&registry)).await?;
    let chart_base = format!("{}/helm/charts", registry_public_base(&req, &registry));
    // ponytail: rendered per request; cache the YAML under the blocked set's
    // fingerprint for `metadata_ttl` (RFC 0029 §4.4) when a fleet's `repo
    // update` shows up in the profile.
    let r = rewrite(
        &mut index,
        &chart_base,
        svc.helm_oci_entries(&registry).await,
    );
    if r.oci_dropped > 0 || r.multi_url > 0 {
        tracing::debug!(
            registry = %registry,
            oci_dropped = r.oci_dropped,
            multi_url = r.multi_url,
            "helm: index entries dropped or narrowed to one URL"
        );
    }
    let body = render_index(&index).map_err(AppError::from)?;
    let mut builder = HttpResponse::Ok();
    builder.content_type(YAML);
    synthesised_headers(&mut builder, synthesised);
    Ok(builder.body(body))
}

/// A chart archive, or with `.prov` appended its provenance file — byte-exact, fetched from the URL the upstream index names.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/helm/charts/{file}",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "{name}-{version}.tgz or {name}-{version}.tgz.prov"),
    ),
    responses(
        (status = 200, description = "The archive or its provenance file", body = ArtifactBytes, content_type = "application/gzip"),
        (status = 400, description = "Not a chart file name"),
        (status = 403, description = "Blocked version or access denied"),
        (status = 404, description = "Not in the index, no provenance, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/helm/charts/{file}",
    method = "GET",
    method = "HEAD",
    guard = "is_helm"
)]
pub async fn helm_chart(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file) = path.into_inner();
    // Parsed back into its coordinate before it becomes a key: this handler
    // names the artifact itself, so the edge validation is its own (§6.5).
    let chart = coordinate_of(&file).map_err(AppError::from)?;
    if !chart.prov {
        return serve_local_or_proxy_artifact(
            svc,
            local_svc,
            &modes,
            &registry,
            &chart.name,
            &chart.version,
            identity,
            LocalOrProxyArtifactOpts {
                artifact_suffix: chart.artifact(),
                local_content_type: GZIP,
                proxy_content_type: Some(GZIP),
                action: Action::ReleasesRead,
                check_prerelease: true,
                append_signature: false,
            },
        )
        .await;
    }

    // A local `.prov` rides the version row's signature slot. The archive read
    // runs first because it is what authorizes the caller and applies
    // visibility and the block — a `.prov` is readable exactly when its chart is.
    let mode = modes.get(&registry);
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        local_svc
            .check_prerelease_access(&registry, &chart.version, &identity)
            .await
            .map_err(AppError::from)?;
        match local_svc
            .get_artifact(
                &registry,
                &chart.name,
                &chart.version,
                Action::ReleasesRead,
                &identity,
                &identity.1,
            )
            .await
        {
            Ok(_) => {
                let prov = local_svc
                    .get_version_meta(&registry, &chart.name, &chart.version)
                    .await
                    .filter(|m| m.signature_type.as_deref() == Some(PROV_SIGNATURE_TYPE))
                    .and_then(|m| m.signature_bytes)
                    .ok_or_else(|| {
                        AppError::not_found(format!(
                            "{} {} was published without a provenance file",
                            chart.name, chart.version
                        ))
                    })?;
                return Ok(HttpResponse::Ok().content_type(PROV).body(prov));
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }
    let pkg =
        PackageId::new(&registry, &chart.name, &chart.version).with_artifact(chart.artifact());
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, Some(PROV)).await
}

/// What a publish carried: the archive, and the `.prov` beside it if any.
struct Upload {
    chart: Bytes,
    prov: Option<Bytes>,
}

/// ChartMuseum's two body shapes: the archive as the whole body
/// (`curl --data-binary`), or a multipart form with `chart` and optional
/// `prov` (`helm cm-push`).
async fn read_upload(req: &HttpRequest, payload: web::Payload) -> Result<Upload, AppError> {
    let multipart = req
        .headers()
        .get(actix_web::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/form-data"));
    if !multipart {
        return Ok(Upload {
            chart: collect_payload(payload).await?,
            prov: None,
        });
    }
    let mut form = Multipart::new(req.headers(), payload);
    let (mut chart, mut prov) = (None, None);
    let mut total: u64 = 0;
    while let Some(field) = form.next().await {
        let mut field =
            field.map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?;
        let name = field
            .content_disposition()
            .and_then(|cd| cd.get_name())
            .unwrap_or("")
            .to_owned();
        let mut buf = BytesMut::new();
        while let Some(chunk) = field.next().await {
            let chunk = chunk.map_err(|e| AppError::bad_request(format!("chunk error: {e}")))?;
            total += chunk.len() as u64;
            if total > MAX_UPLOAD_BYTES {
                return Err(AppError::from(CoreError::PayloadTooLarge(format!(
                    "upload exceeds {MAX_UPLOAD_BYTES} bytes"
                ))));
            }
            buf.extend_from_slice(&chunk);
        }
        match name.as_str() {
            "chart" => chart = Some(buf.freeze()),
            "prov" => prov = Some(buf.freeze()),
            _ => {}
        }
    }
    Ok(Upload {
        chart: chart
            .ok_or_else(|| AppError::bad_request("no 'chart' field in the multipart body"))?,
        prov,
    })
}

/// Whether `?force` was asked for — ChartMuseum reads its presence.
fn wants_force(req: &HttpRequest) -> bool {
    req.query_string()
        .split('&')
        .any(|p| p == "force" || (p.starts_with("force=") && p != "force=false"))
}

/// Publish a packaged chart — ChartMuseum's upload, the call `helm cm-push` makes.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/helm/api/charts",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("force" = Option<bool>, Query, description = "Replace an existing version (needs releases:overwrite)"),
    ),
    request_body(content = String, description = "The .tgz as the body, or multipart/form-data with `chart` and optional `prov`", content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Stored", body = HelmSaved),
        (status = 400, description = "Not a packaged chart, or its Chart.yaml names no valid name or version"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or mode = proxy"),
        (status = 409, description = "That version is already published"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/helm/api/charts", guard = "is_helm")]
#[allow(clippy::too_many_arguments)]
pub async fn helm_publish(
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notifications: web::Data<Option<Arc<NotificationService>>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_local_mode(&registry, &modes)?;
    let upload = read_upload(&req, payload).await?;

    // Name and version come from inside the archive and are validated before
    // either reaches a storage key (§6.5).
    // Decompression is CPU work bounded at 100 MiB, so off the worker thread.
    let archive = upload.chart.clone();
    let chart =
        web::block(move || batlehub_adapters::registry::helm::chart_yaml_from_archive(&archive))
            .await
            .map_err(|e| AppError::internal(format!("reading the chart archive: {e}")))?
            .map_err(AppError::from)?;
    let name = chart
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let version = chart
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    validate_chart_name(&name).map_err(AppError::from)?;
    validate_version(&version).map_err(AppError::from)?;

    // `?force` is `releases:overwrite` (§4.4). The row store refuses every
    // republish whatever the caller holds, so a forced publish of an existing
    // version is still a `409`; the verb is checked first so a caller without it
    // learns nothing about which versions exist.
    if wants_force(&req) {
        batlehub_core::services::authz::authorize_grants_public(
            &svc.hot,
            &PackageId::new(&registry, &name, &version),
            &identity.0,
            Action::ReleasesOverwrite,
        )
        .await
        .map_err(AppError::from)?;
    }

    let checksum = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(&upload.chart))
    };
    let publish = PublishRequest {
        registry,
        name,
        version,
        artifact: upload.chart,
        checksum,
        index_metadata: index_metadata_from_chart_yaml(&chart),
        unlisted: false,
        publisher: identity.0.clone(),
        // The `.prov` is the chart's signature, OpenPGP by the publisher's own
        // key, stored as uploaded and never re-signed (§3).
        signature_type: upload.prov.as_ref().map(|_| PROV_SIGNATURE_TYPE.to_owned()),
        signature_bytes: upload.prov.map(|p| p.to_vec()),
    };
    publish_and_respond(
        &local_svc,
        &notifications,
        publish,
        actix_web::http::StatusCode::CREATED,
        HelmSaved { saved: true },
    )
    .await
}

/// Delete a published chart version (`releases:delete`); it leaves the next index.
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/helm/api/charts/{name}/{version}",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Chart name"),
        ("version" = String, Path, description = "Chart version"),
    ),
    responses(
        (status = 200, description = "Deleted", body = HelmDeleted),
        (status = 400, description = "Not a chart coordinate"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not published here, unknown registry, or mode = proxy"),
    ),
    security(("bearer_token" = [])),
)]
#[delete(
    "/proxy/{registry}/helm/api/charts/{name}/{version}",
    guard = "is_helm"
)]
pub async fn helm_delete(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    require_local_mode(&registry, &modes)?;
    validate_chart_name(&name).map_err(AppError::from)?;
    validate_version(&version).map_err(AppError::from)?;
    if !local_svc
        .delete_version(&registry, &name, &version, &identity.0)
        .await
        .map_err(AppError::from)?
    {
        return Err(AppError::not_found(format!(
            "{name} {version} is not published here"
        )));
    }
    Ok(HttpResponse::Ok().json(HelmDeleted { deleted: true }))
}

/// The local index's entries as ChartMuseum lists them: `urls` relative to the
/// repository, as `chartmuseum` writes them.
async fn api_entries(
    registry: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
    modes: &RegistryModeMap,
) -> Result<serde_json::Map<String, Value>, AppError> {
    require_local_mode(registry, modes)?;
    svc.authorize_read(
        &PackageId::new(registry, INDEX_ADDRESS, "latest"),
        &identity.0,
        Action::ReleasesList,
    )
    .await
    .map_err(AppError::from)?;
    let mut index = local_svc
        .get_helm_index(registry, &identity.0)
        .await
        .map_err(AppError::from)?;
    let mut entries = index
        .get_mut("entries")
        .and_then(Value::as_object_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    for (name, versions) in entries.iter_mut() {
        for entry in versions.as_array_mut().into_iter().flatten() {
            let version = entry["version"].as_str().unwrap_or_default().to_owned();
            entry["urls"] =
                serde_json::json!([format!("charts/{}", chart_file_name(name, &version))]);
        }
    }
    Ok(entries)
}

#[derive(serde::Deserialize)]
pub struct Paging {
    offset: Option<usize>,
    limit: Option<usize>,
}

/// Every chart published here and its versions — ChartMuseum's listing, paged by chart with `offset`/`limit`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/helm/api/charts",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("offset" = Option<usize>, Query, description = "Charts to skip"),
        ("limit" = Option<usize>, Query, description = "Charts to return"),
    ),
    responses(
        (status = 200, description = "Chart name → its versions", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or mode = proxy"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/helm/api/charts",
    method = "GET",
    method = "HEAD",
    guard = "is_helm"
)]
pub async fn helm_api_list(
    path: web::Path<String>,
    paging: web::Query<Paging>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    let entries = api_entries(&registry, &identity, &svc, &local_svc, &modes).await?;
    let paged: serde_json::Map<String, Value> = entries
        .into_iter()
        .skip(paging.offset.unwrap_or(0))
        .take(paging.limit.unwrap_or(usize::MAX))
        .collect();
    Ok(HttpResponse::Ok().json(paged))
}

/// One chart's versions as ChartMuseum lists them.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/helm/api/charts/{name}",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Chart name"),
    ),
    responses(
        (status = 200, description = "The chart's versions", body = Vec<UpstreamDocument>),
        (status = 400, description = "Not a chart name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "No such chart, unknown registry, or mode = proxy"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/helm/api/charts/{name}",
    method = "GET",
    method = "HEAD",
    guard = "is_helm"
)]
pub async fn helm_api_chart(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    validate_chart_name(&name).map_err(AppError::from)?;
    let mut entries = api_entries(&registry, &identity, &svc, &local_svc, &modes).await?;
    let versions = entries
        .remove(&name)
        .ok_or_else(|| AppError::not_found(format!("chart '{name}' is not published here")))?;
    Ok(HttpResponse::Ok().json(versions))
}

/// One chart version as ChartMuseum describes it.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/helm/api/charts/{name}/{version}",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Chart name"),
        ("version" = String, Path, description = "Chart version"),
    ),
    responses(
        (status = 200, description = "The chart version", body = UpstreamDocument),
        (status = 400, description = "Not a chart coordinate"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not published here, unknown registry, or mode = proxy"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/helm/api/charts/{name}/{version}",
    method = "GET",
    method = "HEAD",
    guard = "is_helm"
)]
pub async fn helm_api_version(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    validate_chart_name(&name).map_err(AppError::from)?;
    validate_version(&version).map_err(AppError::from)?;
    let entries = api_entries(&registry, &identity, &svc, &local_svc, &modes).await?;
    let entry = entries
        .get(&name)
        .and_then(Value::as_array)
        .and_then(|l| {
            l.iter()
                .find(|e| e["version"].as_str() == Some(version.as_str()))
        })
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("{name} {version} is not published here")))?;
    Ok(HttpResponse::Ok().json(entry))
}
