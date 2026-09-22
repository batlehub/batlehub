//! Devfile registries — Eclipse Che's *Get Started* page and `registry-library`
//! (so `odo`) (RFC 0035).
//!
//! Three families of route, and every one reaches a gate this proxy already has:
//!
//! - **The index documents** (`index`, `v2index`, each with `/sample`, `/stack`
//!   and `/all`) go through `ProxyService::multi_package_document`, which
//!   removes blocked versions registry-wide. The query string is canonicalised
//!   into the listing's address first ([`index_address`]).
//! - **The REST devfile and the starter projects** resolve a version from the
//!   *filtered* v2 index — a versionless request takes the stack's default,
//!   which the filter has already moved off a blocked version — and then fetch
//!   an artifact of that coordinate through `ProxyService::handle`.
//! - **The OCI routes** turn a tag or a digest into the same coordinates. A
//!   digest is looked up only among the versions the filtered index still
//!   lists, so a blocked version's digests are not reachable at all; and the
//!   artifact fetch re-checks the coordinate on every request (RFC 0035 §5.2).
//!
//! `registry-library` asks for `/v2/…` and `/devfiles/…/starter-projects/…` at
//! the **host root**, dropping any path prefix, so those routes only work
//! behind an RFC 0001 host binding; Che keeps the prefix (§5.3).
//!
//! Every route carries [`is_devfile`] as a guard, so it matches only on a
//! devfile registry: without it the literal `index` route would take an npm
//! package called `index` away from the npm packument route below it.

use std::sync::Arc;

use actix_web::{get, guard::GuardContext, route, web, HttpRequest, HttpResponse, Responder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use batlehub_core::{
    entities::{Action, PackageId},
    error::CoreError,
    ports::DocumentKind,
    services::devfile::{
        default_version, find_stack, index_address, layer_artifact, self_link, starter_artifact,
        validate_digest, validate_namespace, validate_stack, validate_starter, validate_version,
        version_entry, versions_of, DEVFILE_TITLE, MANIFEST_ARTIFACT, MANIFEST_MEDIA_TYPE,
    },
    services::{ProxyRequest, ProxyResponse, ProxyService},
};

use super::common::{
    attachment_disposition, collect_storage_stream, document_response, proxy_stream,
    require_registry_type,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use crate::middleware::extract_registry_from_path;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

const KIND: &str = "devfile";
const API_VERSION: (&str, &str) = ("Docker-Distribution-Api-Version", "registry/2.0");
/// What upstream serves a devfile and the index as. The body is YAML and JSON
/// respectively; neither client reads the type.
const TEXT: &str = "text/plain; charset=utf-8";

/// Match only on a devfile registry. Read from the path because a guard runs
/// before extraction; the host-routing middleware has already rewritten a
/// registry host's root onto `/proxy/{registry}/…` by then.
pub fn is_devfile(ctx: &GuardContext) -> bool {
    let Some(map) = ctx.app_data::<web::Data<RegistryMap>>() else {
        return false;
    };
    extract_registry_from_path(ctx.head().uri.path()).is_some_and(|r| map.is_type(r, KIND))
}

/// `GET v2/` — the OCI API version check.
#[derive(Debug, Serialize, ToSchema)]
pub struct OciPing {}

/// `GET v2/{ns}/{stack}/tags/list`.
#[derive(Debug, Serialize, ToSchema)]
pub struct OciTagList {
    /// `{ns}/{stack}`.
    pub name: String,
    /// The versions the filtered index lists for the stack.
    pub tags: Vec<String>,
}

/// An OCI distribution error, the shape containerd reads (`errors[].code`).
fn oci_error(status: actix_web::http::StatusCode, code: &str, message: &str) -> HttpResponse {
    HttpResponse::build(status)
        .insert_header(API_VERSION)
        .json(serde_json::json!({ "errors": [{ "code": code, "message": message }] }))
}

fn oci_not_found(code: &str, message: impl AsRef<str>) -> HttpResponse {
    oci_error(
        actix_web::http::StatusCode::NOT_FOUND,
        code,
        message.as_ref(),
    )
}

fn proxy_request(pkg: PackageId, identity: &AuthIdentity, action: Action) -> ProxyRequest {
    ProxyRequest {
        package_id: pkg,
        identity: identity.0.clone(),
        action: action.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    }
}

/// One index document, filtered.
async fn serve_index(
    req: &HttpRequest,
    registry: &str,
    path: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
) -> Result<HttpResponse, AppError> {
    let (address, kind) = index_address(path, req.query_string()).map_err(AppError::from)?;
    let preq = proxy_request(
        PackageId::new(registry, &address, "__index__"),
        identity,
        Action::ReleasesList,
    );
    let doc = svc
        .multi_package_document(&preq, kind, "")
        .await
        .map_err(AppError::from)?;
    Ok(document_response(doc))
}

/// The v2 index with blocked versions removed — the one place this handler
/// learns which versions of a stack exist and which is the default.
async fn filtered_v2(
    registry: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
) -> Result<serde_json::Value, AppError> {
    let preq = proxy_request(
        PackageId::new(registry, "v2index", "__index__"),
        identity,
        Action::ReleasesList,
    );
    let doc = svc
        .multi_package_document(&preq, DocumentKind::Versions, "")
        .await
        .map_err(AppError::from)?;
    doc.body
        .as_json()
        .cloned()
        .ok_or_else(|| AppError::bad_gateway("the devfile v2 index is not JSON"))
}

/// The version a REST request names, or the stack's default when it names none
/// — checked against the filtered index, so a blocked version is not found.
async fn resolve_version(
    registry: &str,
    stack: &str,
    version: Option<&str>,
    identity: &AuthIdentity,
    svc: &ProxyService,
) -> Result<String, AppError> {
    validate_stack(stack).map_err(AppError::from)?;
    if let Some(v) = version {
        validate_version(v).map_err(AppError::from)?;
    }
    let index = filtered_v2(registry, identity, svc).await?;
    let entry = find_stack(&index, stack)
        .ok_or_else(|| AppError::not_found(format!("stack '{stack}' is not in this registry")))?;
    let version = match version {
        Some(v) => v,
        None => default_version(entry)
            .ok_or_else(|| AppError::not_found(format!("stack '{stack}' has no versions")))?,
    };
    if version_entry(entry, version).is_none() {
        return Err(AppError::not_found(format!(
            "the requested version {version} for stack {stack} does not exist in the registry"
        )));
    }
    Ok(version.to_owned())
}

macro_rules! index_route {
    ($name:ident, $path:literal, $doc:literal, $desc:literal) => {
        #[doc = $desc]
        #[utoipa::path(
            get,
            path = $path,
            tag = "proxy/devfile",
            params(
                ("registry" = String, Path, description = "Registry name"),
                ("arch" = Option<Vec<String>>, Query, description = "Architecture filter, repeatable"),
                ("deprecated" = Option<bool>, Query, description = "Deprecated filter (v2 index)"),
                ("minSchemaVersion" = Option<String>, Query, description = "Lowest devfile schema version (v2 index)"),
                ("maxSchemaVersion" = Option<String>, Query, description = "Highest devfile schema version (v2 index)"),
            ),
            responses(
                (status = 200, description = "The index document, blocked versions removed", body = Vec<UpstreamDocument>),
                (status = 400, description = "A query parameter upstream would refuse"),
                (status = 403, description = "Access denied"),
                (status = 404, description = "Unknown registry"),
            ),
            security(("bearer_token" = [])),
        )]
        #[get($path, guard = "is_devfile")]
        pub async fn $name(
            req: HttpRequest,
            path: web::Path<String>,
            identity: AuthIdentity,
            svc: web::Data<Arc<ProxyService>>,
            map: web::Data<RegistryMap>,
        ) -> Result<impl Responder, AppError> {
            let registry = path.into_inner();
            require_registry_type(&registry, KIND, &map)?;
            serve_index(&req, &registry, $doc, &identity, &svc).await
        }
    };
}

index_route!(devfile_index, "/proxy/{registry}/index", "index", "The legacy stack index Che's *Get Started* page reads (`index/all`), blocked default versions removed with their stack.");
index_route!(
    devfile_index_sample,
    "/proxy/{registry}/index/sample",
    "index/sample",
    "The legacy index of samples."
);
index_route!(
    devfile_index_stack,
    "/proxy/{registry}/index/stack",
    "index/stack",
    "The legacy stack index (`index/stack` is upstream's alias for `index`)."
);
index_route!(
    devfile_index_all,
    "/proxy/{registry}/index/all",
    "index/all",
    "The legacy index of stacks and samples — the document Che's dashboard reads."
);
index_route!(devfile_v2index, "/proxy/{registry}/v2index", "v2index", "The v2 stack index `registry-library` resolves every pull through, blocked versions removed and `default` moved off a blocked one.");
index_route!(
    devfile_v2index_sample,
    "/proxy/{registry}/v2index/sample",
    "v2index/sample",
    "The v2 index of samples."
);
index_route!(
    devfile_v2index_stack,
    "/proxy/{registry}/v2index/stack",
    "v2index/stack",
    "The v2 stack index (`v2index/stack` is upstream's alias for `v2index`)."
);
index_route!(
    devfile_v2index_all,
    "/proxy/{registry}/v2index/all",
    "v2index/all",
    "The v2 index of stacks and samples."
);

/// Serve one artifact of a resolved coordinate through the proxy funnel.
async fn serve_artifact(
    svc: web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    identity: AuthIdentity,
    content_type: &'static str,
) -> Result<HttpResponse, AppError> {
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, Some(content_type)).await
}

/// A stack's devfile at its default version — the file Che's tile links to.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/devfiles/{stack}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("stack" = String, Path, description = "Stack name"),
    ),
    responses(
        (status = 200, description = "devfile.yaml of the default version, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Not a stack name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown stack, or every version blocked"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/devfiles/{stack}", guard = "is_devfile")]
pub async fn devfile_stack(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let version = resolve_version(&registry, &stack, None, &identity, &svc).await?;
    let pkg =
        PackageId::new(&registry, &stack, &version).with_artifact(layer_artifact(DEVFILE_TITLE));
    serve_artifact(svc, pkg, identity, TEXT).await
}

/// One version's devfile.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/devfiles/{stack}/{version}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("stack" = String, Path, description = "Stack name"),
        ("version" = String, Path, description = "Stack version"),
    ),
    responses(
        (status = 200, description = "devfile.yaml, byte-exact", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Not a stack name or version"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown or blocked version"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/devfiles/{stack}/{version}", guard = "is_devfile")]
pub async fn devfile_version(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, version) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let version = resolve_version(&registry, &stack, Some(&version), &identity, &svc).await?;
    let pkg =
        PackageId::new(&registry, &stack, &version).with_artifact(layer_artifact(DEVFILE_TITLE));
    serve_artifact(svc, pkg, identity, TEXT).await
}

async fn serve_starter(
    registry: String,
    stack: String,
    version: Option<String>,
    name: String,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<HttpResponse, AppError> {
    validate_starter(&name).map_err(AppError::from)?;
    let version = resolve_version(&registry, &stack, version.as_deref(), &identity, &svc).await?;
    let pkg = PackageId::new(&registry, &stack, &version).with_artifact(starter_artifact(&name));
    let mut resp = serve_artifact(svc, pkg, identity, "application/zip").await?;
    resp.headers_mut().insert(
        actix_web::http::header::CONTENT_DISPOSITION,
        attachment_disposition(&format!("{name}.zip"))?,
    );
    Ok(resp)
}

/// A starter project of the stack's default version. `registry-library` asks
/// for this one, at the host root, with no version in the URL.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/devfiles/{stack}/starter-projects/{name}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("stack" = String, Path, description = "Stack name"),
        ("name" = String, Path, description = "Starter project name"),
    ),
    responses(
        (status = 200, description = "The starter project as a zip", body = ArtifactBytes, content_type = "application/zip"),
        (status = 400, description = "Not a stack or starter project name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown stack or starter project"),
    ),
    security(("bearer_token" = [])),
)]
#[get(
    "/proxy/{registry}/devfiles/{stack}/starter-projects/{name}",
    guard = "is_devfile"
)]
pub async fn devfile_starter(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, name) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    serve_starter(registry, stack, None, name, identity, svc).await
}

/// A starter project of one version.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/devfiles/{stack}/{version}/starter-projects/{name}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("stack" = String, Path, description = "Stack name"),
        ("version" = String, Path, description = "Stack version"),
        ("name" = String, Path, description = "Starter project name"),
    ),
    responses(
        (status = 200, description = "The starter project as a zip", body = ArtifactBytes, content_type = "application/zip"),
        (status = 400, description = "Not a stack, version or starter project name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown or blocked version, or unknown starter project"),
    ),
    security(("bearer_token" = [])),
)]
#[get(
    "/proxy/{registry}/devfiles/{stack}/{version}/starter-projects/{name}",
    guard = "is_devfile"
)]
pub async fn devfile_version_starter(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, version, name) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    serve_starter(registry, stack, Some(version), name, identity, svc).await
}

/// The OCI API version check. `registry-library` does not send it; other OCI
/// tools do.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v2/",
    tag = "proxy/devfile",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "An empty object, with Docker-Distribution-Api-Version", body = OciPing),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/v2/", guard = "is_devfile")]
pub async fn devfile_oci_ping(
    path: web::Path<String>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    Ok(HttpResponse::Ok()
        .insert_header(API_VERSION)
        .json(OciPing {}))
}

/// The stack's entry in the filtered index, checked against the namespace the
/// request names — the index's `links.self` is the only source of it.
async fn oci_stack(
    registry: &str,
    ns: &str,
    stack: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
) -> Result<Result<Vec<(String, String)>, HttpResponse>, AppError> {
    if validate_namespace(ns).is_err() || validate_stack(stack).is_err() {
        return Ok(Err(oci_not_found(
            "NAME_INVALID",
            format!("'{ns}/{stack}' is not a repository name"),
        )));
    }
    let index = filtered_v2(registry, identity, svc).await?;
    let Some(entry) = find_stack(&index, stack) else {
        return Ok(Err(oci_not_found(
            "NAME_UNKNOWN",
            format!("repository '{ns}/{stack}' is not known to this registry"),
        )));
    };
    // `(version, tag)` for every version the filter left, in the stack's
    // namespace.
    let tags: Vec<(String, String)> = versions_of(entry)
        .into_iter()
        .filter_map(|v| {
            let link = version_entry(entry, v)?.pointer("/links/self")?.as_str()?;
            let (link_ns, tag) = self_link(link, stack).ok()?;
            (link_ns == ns).then(|| (v.to_owned(), tag))
        })
        .collect();
    if tags.is_empty() {
        return Ok(Err(oci_not_found(
            "NAME_UNKNOWN",
            format!("repository '{ns}/{stack}' is not known to this registry"),
        )));
    }
    Ok(Ok(tags))
}

/// The manifest-by-digest or blob-by-digest lookup: which allowed version of
/// the stack names this digest, and as which artifact.
async fn artifact_for_digest(
    registry: &str,
    stack: &str,
    tags: &[(String, String)],
    digest: &str,
    want_manifest: bool,
    identity: &AuthIdentity,
    svc: &ProxyService,
) -> Option<(String, String)> {
    for (version, _) in tags {
        let preq = proxy_request(
            PackageId::new(registry, stack, version).with_artifact(MANIFEST_ARTIFACT),
            identity,
            Action::ReleasesRead,
        );
        let Ok(meta) = svc.resolve_metadata_uncaptured_for(&preq).await else {
            continue;
        };
        if want_manifest {
            if meta
                .extra
                .pointer("/devfile/manifestDigest")
                .and_then(|d| d.as_str())
                == Some(digest)
            {
                return Some((version.clone(), MANIFEST_ARTIFACT.to_owned()));
            }
            continue;
        }
        let layers = meta
            .extra
            .pointer("/devfile/layers")
            .and_then(|l| l.as_array());
        for layer in layers.into_iter().flatten() {
            if layer.get("digest").and_then(|d| d.as_str()) == Some(digest) {
                if let Some(title) = layer.get("title").and_then(|t| t.as_str()) {
                    return Some((version.clone(), layer_artifact(title)));
                }
            }
        }
    }
    None
}

/// Fetch one OCI object through the funnel and answer it byte-exact, with the
/// digest and length a `HEAD` must carry. A refused coordinate is the OCI
/// `code` the client understands, with the reason as its message.
async fn serve_oci(
    svc: &web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    identity: &AuthIdentity,
    unknown_code: &str,
    content_type: &str,
) -> Result<HttpResponse, AppError> {
    let coordinate = pkg.clone();
    let response = svc
        .handle(proxy_request(pkg, identity, Action::ReleasesRead))
        .await;
    let response = match response {
        Ok(r) => r,
        Err(CoreError::NotFound(m)) => return Ok(oci_not_found(unknown_code, m)),
        Err(e) => return Err(AppError::from(e)),
    };
    let response = match response {
        ProxyResponse::Warned { response, .. } => *response,
        other => other,
    };
    let stream = match response {
        ProxyResponse::Stream(stream) => stream,
        ProxyResponse::Denied {
            verdict: Some(verdict),
            ..
        } => {
            return Ok(crate::handlers::security::hold_http_response(
                svc,
                &coordinate,
                &identity.0,
                &verdict,
            )
            .await)
        }
        ProxyResponse::Denied { reason, .. } => {
            return Ok(oci_error(
                actix_web::http::StatusCode::FORBIDDEN,
                "DENIED",
                &reason,
            ))
        }
        _ => return Err(AppError::internal("unexpected response for an OCI object")),
    };
    let body = collect_storage_stream(stream).await?;
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
    let mut builder = HttpResponse::Ok();
    // The key, package and version these bytes are filed under — what a
    // bundle export reads back, rather than guessing a key from the route.
    for header in super::common::served_identity(&coordinate) {
        builder.insert_header(header);
    }
    Ok(builder
        .content_type(content_type)
        .insert_header(API_VERSION)
        .insert_header(("Docker-Content-Digest", digest.clone()))
        .insert_header(("ETag", format!("\"{digest}\"")))
        .body(body))
}

/// A stack version's OCI manifest, by tag or by digest — the chokepoint every
/// `registry-library pull` goes through first (RFC 0035 §5.2).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v2/{ns}/{stack}/manifests/{reference}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("ns" = String, Path, description = "OCI namespace, from the index's links.self"),
        ("stack" = String, Path, description = "Stack name"),
        ("reference" = String, Path, description = "Tag (the stack version) or sha256 digest"),
    ),
    responses(
        (status = 200, description = "The OCI manifest, byte-exact, with Docker-Content-Digest", body = ProtocolDocument, content_type = "application/vnd.oci.image.manifest.v1+json"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown repository, or a blocked or unknown tag or digest (OCI error body)"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/v2/{ns}/{stack}/manifests/{reference}",
    method = "GET",
    method = "HEAD",
    guard = "is_devfile"
)]
pub async fn devfile_oci_manifest(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, ns, stack, reference) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let tags = match oci_stack(&registry, &ns, &stack, &identity, &svc).await? {
        Ok(t) => t,
        Err(resp) => return Ok(resp),
    };
    let version = if reference.starts_with("sha256:") {
        if validate_digest(&reference).is_err() {
            return Ok(oci_not_found(
                "DIGEST_INVALID",
                format!("'{reference}' is not a digest"),
            ));
        }
        match artifact_for_digest(&registry, &stack, &tags, &reference, true, &identity, &svc).await
        {
            Some((version, _)) => version,
            None => {
                return Ok(oci_not_found(
                    "MANIFEST_UNKNOWN",
                    format!("no allowed version of {stack} has manifest {reference}"),
                ))
            }
        }
    } else {
        match tags.iter().find(|(_, tag)| *tag == reference) {
            Some((version, _)) => version.clone(),
            None => {
                return Ok(oci_not_found(
                    "MANIFEST_UNKNOWN",
                    format!("the requested version {reference} for stack {stack} is not served by this registry"),
                ))
            }
        }
    };
    let pkg = PackageId::new(&registry, &stack, &version).with_artifact(MANIFEST_ARTIFACT);
    serve_oci(
        &svc,
        pkg,
        &identity,
        "MANIFEST_UNKNOWN",
        MANIFEST_MEDIA_TYPE,
    )
    .await
}

/// One layer of a stack version, by digest — served only when an allowed
/// version's manifest names it.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v2/{ns}/{stack}/blobs/{digest}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("ns" = String, Path, description = "OCI namespace"),
        ("stack" = String, Path, description = "Stack name"),
        ("digest" = String, Path, description = "sha256 digest"),
    ),
    responses(
        (status = 200, description = "The layer, verified against its digest", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown repository, or a digest no allowed version names (OCI error body)"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/v2/{ns}/{stack}/blobs/{digest}",
    method = "GET",
    method = "HEAD",
    guard = "is_devfile"
)]
pub async fn devfile_oci_blob(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, ns, stack, digest) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    if validate_digest(&digest).is_err() {
        return Ok(oci_not_found(
            "DIGEST_INVALID",
            format!("'{digest}' is not a digest"),
        ));
    }
    let tags = match oci_stack(&registry, &ns, &stack, &identity, &svc).await? {
        Ok(t) => t,
        Err(resp) => return Ok(resp),
    };
    let Some((version, artifact)) =
        artifact_for_digest(&registry, &stack, &tags, &digest, false, &identity, &svc).await
    else {
        return Ok(oci_not_found(
            "BLOB_UNKNOWN",
            format!("no allowed version of {stack} names blob {digest}"),
        ));
    };
    let pkg = PackageId::new(&registry, &stack, &version).with_artifact(artifact);
    serve_oci(
        &svc,
        pkg,
        &identity,
        "BLOB_UNKNOWN",
        "application/octet-stream",
    )
    .await
}

/// The tags of a stack — the versions the filtered index lists.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v2/{ns}/{stack}/tags/list",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("ns" = String, Path, description = "OCI namespace"),
        ("stack" = String, Path, description = "Stack name"),
    ),
    responses(
        (status = 200, description = "The allowed tags", body = OciTagList),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown repository (OCI error body)"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/v2/{ns}/{stack}/tags/list", guard = "is_devfile")]
pub async fn devfile_oci_tags(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, ns, stack) = path.into_inner();
    require_registry_type(&registry, KIND, &map)?;
    let tags = match oci_stack(&registry, &ns, &stack, &identity, &svc).await? {
        Ok(t) => t,
        Err(resp) => return Ok(resp),
    };
    Ok(HttpResponse::Ok()
        .insert_header(API_VERSION)
        .json(OciTagList {
            name: format!("{ns}/{stack}"),
            tags: tags.into_iter().map(|(_, tag)| tag).collect(),
        }))
}
