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
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{Action, PackageId, RegistryKind},
    error::CoreError,
    ports::{DocumentBody, DocumentKind},
    services::helm::{
        chart_file_name, coordinates_of, index_metadata_from_chart_yaml, render_index, rewrite,
        validate_chart_name, validate_version, ChartFile, HybridMerge, INDEX_ADDRESS,
        PROV_SIGNATURE_TYPE,
    },
    services::{local_registry::PublishRequest, LocalRegistryService, ProxyRequest, ProxyService},
};

use super::common::{
    collect_form_fields, collect_payload, proxy_stream, publish_and_respond, registry_public_base,
    require_local_mode, synthesised_headers,
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
        svc.authorize_listing(&pkg, &identity.0, Action::ReleasesList)
            .await
            .map_err(AppError::from)?;
        let local = local_svc
            .get_helm_index(registry, &identity.0)
            .await
            .map_err(AppError::from)?;
        return Ok((local.index, None));
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
    let DocumentBody::Json(mut index) = doc.body else {
        return Err(AppError::bad_gateway(
            "the upstream index.yaml did not parse",
        ));
    };
    if mode == RegistryMode::Hybrid {
        let local = local_svc
            .get_helm_index(registry, &identity.0)
            .await
            .map_err(AppError::from)?;
        batlehub_core::services::helm::merge(
            &mut index,
            &local.index,
            &local.hosted,
            svc.helm_hybrid_merge(registry).await,
        );
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
    let repo_url = format!("{}/helm", registry_public_base(&req, &registry));
    let chart_base = format!("{repo_url}/charts");
    let context_path = repo_url
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
        .unwrap_or_default();
    let oci_entries = svc.helm_oci_entries(&registry).await;
    // A large index (Bitnami's is 27 MB) takes seconds to rewrite and render,
    // so both run off the runtime, as its parse does.
    // ponytail: rendered per request; cache the YAML under the blocked set's
    // fingerprint for `metadata_ttl` (RFC 0029 §4.4) when a fleet's `repo
    // update` shows up in the profile.
    let (r, body) = web::block(move || {
        let r = rewrite(&mut index, &chart_base, oci_entries);
        // `helm cm-push` posts to `{serverInfo.contextPath}/api/charts` and
        // reads the context path from here; without it the plugin builds
        // `/api/proxy/{registry}/helm/charts`. Set on every index, replacing an
        // upstream ChartMuseum's own, which names a path on another host.
        if let Some(obj) = index.as_object_mut() {
            obj.insert(
                "serverInfo".to_owned(),
                serde_json::json!({ "contextPath": context_path }),
            );
        }
        render_index(&index).map(|body| (r, body))
    })
    .await
    .map_err(|e| AppError::internal(format!("helm: rendering index.yaml: {e}")))?
    .map_err(AppError::from)?;
    if r.oci_dropped > 0 || r.multi_url > 0 {
        tracing::debug!(
            registry = %registry,
            oci_dropped = r.oci_dropped,
            multi_url = r.multi_url,
            "helm: index entries dropped or narrowed to one URL"
        );
    }
    let mut builder = HttpResponse::Ok();
    builder.content_type(YAML);
    synthesised_headers(&mut builder, synthesised);
    Ok(builder.body(body))
}

/// The coordinate `file` names. A file name that reads more than one way
/// (`app-1.0.0-2.0.0.tgz`) is settled by which reading this registry holds —
/// a local row, else an upstream index entry — and falls back to the leftmost
/// split, which then answers `404` like any absent chart.
async fn chart_of(
    file: &str,
    registry: &str,
    mode: &RegistryMode,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
) -> Result<ChartFile, AppError> {
    let mut all = coordinates_of(file).map_err(AppError::from)?;
    if all.len() > 1 {
        let upstream = match mode {
            RegistryMode::Local => None,
            _ => svc.hot.read().await.registries.get(registry).cloned(),
        };
        for i in 0..all.len() {
            let c = &all[i];
            let local = *mode != RegistryMode::Proxy
                && local_svc
                    .get_version_meta(registry, &c.name, &c.version)
                    .await
                    .is_some();
            let held = local
                || match &upstream {
                    Some(client) => client
                        .resolve_metadata(&PackageId::new(registry, &c.name, &c.version))
                        .await
                        .is_ok(),
                    None => false,
                };
            if held {
                return Ok(all.swap_remove(i));
            }
        }
    }
    Ok(all.swap_remove(0))
}

/// Helm compares versions, not strings: `v1.2.3`, `1.2.3` and `1.2.3+b` are
/// one release, and a block is recorded in whichever spelling the operator
/// typed. The index filter compares them normalised (`blocking::normalize`);
/// the download gate below matches the stored coordinate exactly, so the
/// normalised comparison is repeated here — after the grants, so a caller who
/// may not read the registry learns nothing of its blocks.
async fn refuse_blocked_in_another_spelling(
    svc: &ProxyService,
    registry: &str,
    chart: &ChartFile,
    identity: &AuthIdentity,
) -> Result<(), AppError> {
    let pkg = PackageId::new(registry, &chart.name, &chart.version);
    batlehub_core::services::authz::authorize_grants_public(
        &svc.hot,
        &pkg,
        &identity.0,
        Action::ReleasesRead,
    )
    .await
    .map_err(AppError::from)?;
    if svc
        .blocked_in_registry_snapshot_public(registry, RegistryKind::Helm)
        .await
        .contains(&chart.name, &chart.version)
    {
        return Err(AppError::from(CoreError::AccessDenied(format!(
            "{} {} is blocked",
            chart.name, chart.version
        ))));
    }
    Ok(())
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
    let mut mode = modes.get(&registry);
    let chart = chart_of(&file, &registry, &mode, &svc, &local_svc).await?;
    refuse_blocked_in_another_spelling(&svc, &registry, &chart, &identity).await?;
    // Under `hybrid_merge = "chart"` a chart name hosted here is answered from
    // here alone: the index never listed an upstream version of it, and a miss
    // must not fetch the public chart of the same name either.
    if mode == RegistryMode::Hybrid
        && svc.helm_hybrid_merge(&registry).await == HybridMerge::Chart
        && local_svc
            .hosts(&registry, &chart.name)
            .await
            .map_err(AppError::from)?
    {
        mode = RegistryMode::Local;
    }
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        local_svc
            .check_prerelease_access(&registry, &chart.version, &identity)
            .await
            .map_err(AppError::from)?;
        match local_chart_file(&local_svc, &registry, &chart, &identity).await {
            Ok(resp) => return Ok(resp),
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }
    let content_type = if chart.prov { PROV } else { GZIP };
    let pkg =
        PackageId::new(&registry, &chart.name, &chart.version).with_artifact(chart.artifact());
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, Some(content_type)).await
}

/// The archive, or its `.prov`, from this registry's rows.
///
/// A `.prov` rides the version row's signature slot, and is readable exactly
/// when its chart is: `authorize_artifact_read` applies the rule chain,
/// visibility and the block without reading the archive or counting a
/// download, and hands back the row the signature is on.
async fn local_chart_file(
    local_svc: &LocalRegistryService,
    registry: &str,
    chart: &ChartFile,
    identity: &AuthIdentity,
) -> Result<HttpResponse, CoreError> {
    if !chart.prov {
        let bytes = local_svc
            .get_artifact(
                registry,
                &chart.name,
                &chart.version,
                Action::ReleasesRead,
                identity,
                &identity.1,
            )
            .await?;
        return Ok(HttpResponse::Ok().content_type(GZIP).body(bytes));
    }
    let row = local_svc
        .authorize_artifact_read(
            registry,
            &chart.name,
            &chart.version,
            Action::ReleasesRead,
            identity,
            &identity.1,
        )
        .await?
        .ok_or_else(|| {
            CoreError::NotFound(format!(
                "{}/{}@{} not found in local registry",
                registry, chart.name, chart.version
            ))
        })?;
    let prov = Some(row)
        .filter(|m| m.signature_type.as_deref() == Some(PROV_SIGNATURE_TYPE))
        .and_then(|m| m.signature_bytes)
        .ok_or_else(|| {
            // Not `NotFound`: the chart is here, so a hybrid registry must not
            // go looking for an upstream `.prov` of it.
            CoreError::NotFoundWithheld(format!(
                "{} {} was published without a provenance file",
                chart.name, chart.version
            ))
        })?;
    Ok(HttpResponse::Ok().content_type(PROV).body(prov))
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
    let mut fields = collect_form_fields(Multipart::new(req.headers(), payload)).await?;
    let (chart, prov) = (fields.remove("chart"), fields.remove("prov"));
    Ok(Upload {
        chart: chart
            .ok_or_else(|| AppError::bad_request("no 'chart' field in the multipart body"))?,
        prov,
    })
}

/// Publish a packaged chart — ChartMuseum's upload, the call `helm cm-push` makes.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/helm/api/charts",
    tag = "proxy/helm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("force" = Option<bool>, Query, description = "Accepted for `helm cm-push --force` and ignored: a published version is never replaced"),
    ),
    request_body(content = String, description = "The .tgz as the body, or multipart/form-data with `chart` and optional `prov`", content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Stored", body = HelmSaved),
        (status = 400, description = "Not a packaged chart, or its Chart.yaml names no valid name or version"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or mode = proxy"),
        (status = 409, description = "That version is already published, with or without `?force`"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/helm/api/charts", guard = "is_helm")]
pub async fn helm_publish(
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
    identity: AuthIdentity,
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

    // `?force` is read nowhere. The row store refuses every republish whatever
    // the caller holds (RFC 0016), so there is no replacement for
    // `releases:overwrite` to authorize, and asking for it would only refuse a
    // new version to a caller without it.

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
    let (mut index, _) =
        index_value(registry, identity, svc, local_svc, RegistryMode::Local).await?;
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
