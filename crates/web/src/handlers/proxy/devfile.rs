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
//! **Local and hybrid mode** (RFC 0039) change the source behind a coordinate
//! and nothing else. A stack with a local row — every stack, in `local` mode —
//! is answered from the rows alone ([`Reader::local`]): its index entry is
//! composed from what was published, its OCI objects are the stored manifest
//! and the members of the stored bundle, and a version not held here is `404`
//! with no upstream request. In `hybrid` the index is upstream's minus every
//! stack named here, plus the local ones (`merge_hybrid`).
//!
//! Every route carries [`is_devfile`] as a guard, so it matches only on a
//! devfile registry: without it the literal `index` route would take an npm
//! package called `index` away from the npm packument route below it. The guard
//! is also the registry-type check, so no handler repeats
//! `require_registry_type`: a request for another kind never reaches one.

use std::sync::Arc;

use std::collections::BTreeSet;

use actix_web::{
    delete, get, guard::GuardContext, post, route, web, HttpRequest, HttpResponse, Responder,
};
use bytes::Bytes;
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{Action, PackageId},
    error::CoreError,
    ports::{DocumentBody, DocumentKind},
    services::devfile::{
        default_version, find_stack, index_address, layer_artifact, merge_hybrid, published_facts,
        read_bundle, self_link, starter_artifact, starter_of, validate_digest, validate_namespace,
        validate_stack, validate_starter, validate_version, version_entry, versions_of,
        CONFIG_BLOB, DEVFILE_TITLE, MANIFEST_ARTIFACT, MANIFEST_MEDIA_TYPE,
    },
    services::{
        authz::authorize_unpinned, blocking::best_latest, local_registry::PublishRequest,
        LocalRegistryService, ProxyRequest, ProxyResponse, ProxyService,
    },
};

use super::common::{
    attachment_disposition, collect_payload, collect_storage_stream, document_response,
    proxy_stream, publish_and_respond, require_local_mode,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use crate::middleware::extract_registry_from_path;
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};

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

/// `POST api/stacks` — what was stored.
#[derive(Debug, Serialize, ToSchema)]
pub struct DevfilePublished {
    /// `metadata.name` of the devfile.
    pub stack: String,
    /// `metadata.version` of the devfile.
    pub version: String,
    /// The manifest digest a `HEAD` on the tag answers with, forever.
    pub digest: String,
}

/// `DELETE api/stacks/{stack}/{version}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DevfileDeleted {
    pub deleted: bool,
}

/// The artifact sub-coordinate of a published version's config blob — the
/// constant `{}`, never stored. Local only: the proxy path never names it.
const CONFIG_ARTIFACT: &str = "config";

/// Where one stack's versions are read from, decided once per request.
struct Reader<'a> {
    registry: &'a str,
    identity: &'a AuthIdentity,
    svc: &'a ProxyService,
    local_svc: &'a LocalRegistryService,
    /// The stack is answered from local rows only: always in `local` mode, and
    /// in `hybrid` once any row of its name exists, visible to this caller or
    /// not (RFC 0039 §4.5) — a stack hidden from a caller must not become the
    /// public stack of its name for them.
    local: bool,
}

impl<'a> Reader<'a> {
    async fn new(
        registry: &'a str,
        stack: &str,
        identity: &'a AuthIdentity,
        svc: &'a ProxyService,
        local_svc: &'a LocalRegistryService,
        modes: &RegistryModeMap,
    ) -> Result<Reader<'a>, AppError> {
        let local = match modes.get(registry) {
            RegistryMode::Proxy => false,
            RegistryMode::Local => true,
            // An invalid name is refused by the route's own validation next;
            // it never becomes a row lookup.
            RegistryMode::Hybrid => {
                validate_stack(stack).is_ok()
                    && local_svc
                        .hosts(registry, stack)
                        .await
                        .map_err(AppError::from)?
            }
        };
        Ok(Reader {
            registry,
            identity,
            svc,
            local_svc,
            local,
        })
    }

    /// The v2 index this stack is resolved through: the composed local one, or
    /// upstream's with blocked versions removed.
    async fn index(&self) -> Result<serde_json::Value, AppError> {
        if !self.local {
            return filtered_v2(self.registry, self.identity, self.svc).await;
        }
        self.svc
            .authorize_listing(
                &PackageId::new(self.registry, "v2index", "__index__"),
                &self.identity.0,
                Action::ReleasesList,
            )
            .await
            .map_err(AppError::from)?;
        Ok(self
            .local_svc
            .get_devfile_index(self.registry, "v2index", false, &self.identity.0)
            .await
            .map_err(AppError::from)?
            .index)
    }

    /// The bytes of one object of a published version, through the local read
    /// gates (rules, visibility, the block, prerelease access).
    async fn local_object(&self, pkg: &PackageId) -> Result<Bytes, CoreError> {
        let artifact = pkg.artifact.as_deref().unwrap_or_default();
        let (registry, name, version) = (self.registry, &pkg.name, &pkg.version);
        let (identity, net) = (&self.identity.0, &self.identity.1);
        if artifact == MANIFEST_ARTIFACT || artifact == CONFIG_ARTIFACT {
            let row = self
                .local_svc
                .authorize_artifact_read(
                    registry,
                    name,
                    version,
                    Action::ReleasesRead,
                    identity,
                    net,
                )
                .await?
                .ok_or_else(|| {
                    CoreError::NotFound(format!("{name} {version} is not published here"))
                })?;
            if artifact == CONFIG_ARTIFACT {
                return Ok(Bytes::from_static(CONFIG_BLOB));
            }
            // The stored bytes, never re-serialised (RFC 0039 §7).
            return row
                .index_metadata
                .pointer("/devfile/manifest")
                .and_then(serde_json::Value::as_str)
                .map(|m| Bytes::from(m.to_owned()))
                .ok_or_else(|| {
                    CoreError::Registry(format!("{name} {version} has no stored manifest"))
                });
        }
        let stored = self
            .local_svc
            .get_artifact(registry, name, version, Action::ReleasesRead, identity, net)
            .await?;
        let bundle = read_bundle(&stored).map_err(|e| {
            CoreError::Registry(format!(
                "the stored bundle of {name} {version} does not read: {e}"
            ))
        })?;
        match bundle.member(&stored, artifact) {
            Some(member) => Ok(stored.slice_ref(member)),
            None => Err(CoreError::NotFound(match starter_of(artifact) {
                Some(starter) => format!(
                    "{name} {version} was published without the zip of starter project                      '{starter}'"
                ),
                None => format!("{name} {version} has no {artifact}"),
            })),
        }
    }
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

/// One index document: upstream's filtered, the composed local one, or in
/// `hybrid` the two merged with a local stack owning its whole name.
async fn serve_index(
    req: &HttpRequest,
    registry: &str,
    path: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
    mode: RegistryMode,
) -> Result<HttpResponse, AppError> {
    let (address, kind) = index_address(path, req.query_string()).map_err(AppError::from)?;
    let legacy = kind == DocumentKind::LEGACY_INDEX;
    let pkg = PackageId::new(registry, &address, "__index__");
    if mode == RegistryMode::Local {
        svc.authorize_listing(&pkg, &identity.0, Action::ReleasesList)
            .await
            .map_err(AppError::from)?;
        let local = local_svc
            .get_devfile_index(registry, &address, legacy, &identity.0)
            .await
            .map_err(AppError::from)?;
        return Ok(HttpResponse::Ok().json(local.index));
    }
    let preq = proxy_request(pkg, identity, Action::ReleasesList);
    let mut doc = svc
        .multi_package_document(&preq, kind, "")
        .await
        .map_err(AppError::from)?;
    if mode == RegistryMode::Hybrid {
        let local = local_svc
            .get_devfile_index(registry, &address, legacy, &identity.0)
            .await
            .map_err(AppError::from)?;
        if let DocumentBody::Json(upstream) = &mut doc.body {
            let dropped = merge_hybrid(upstream, local.index, &local.hosted);
            report_shadowed(registry, &address, &dropped);
        }
    }
    Ok(document_response(doc))
}

/// RFC 0039 §4.5's collision report: a stack published here that upstream
/// also lists. Not refused — the local stack keeps its name — but said, once
/// per name per process, and counted on a gauge an alert can read.
///
/// The gauge is set from the unfiltered documents only, so an `?arch=` query
/// that happens to leave a stack out of upstream's half does not move it.
fn report_shadowed(registry: &str, address: &str, dropped: &BTreeSet<String>) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    if !address.contains('?') {
        metrics::gauge!(
            "batlehub_devfile_shadowed_upstream_stacks",
            "registry" => registry.to_owned()
        )
        .set(dropped.len() as f64);
    }
    let Ok(mut seen) = SEEN.get_or_init(Default::default).lock() else {
        return;
    };
    for stack in dropped {
        if seen.insert(format!("{registry}/{stack}")) {
            tracing::warn!(
                registry = %registry,
                stack = %stack,
                "devfile stack '{stack}' is published here and upstream lists one of the same \
                 name; the local stack is served whole and upstream's is hidden"
            );
        }
    }
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
    r: &Reader<'_>,
    stack: &str,
    version: Option<&str>,
) -> Result<String, AppError> {
    let (registry, identity, svc) = (r.registry, r.identity, r.svc);
    validate_stack(stack).map_err(AppError::from)?;
    if let Some(v) = version {
        validate_version(v).map_err(AppError::from)?;
    }
    let index = r.index().await?;
    let entry = find_stack(&index, stack)
        .ok_or_else(|| AppError::not_found(format!("stack '{stack}' is not in this registry")))?;
    let version = match version {
        // Upstream's REST route also takes the two keywords its version map
        // carries: `latest` (the highest version) and `default`.
        Some(v) if v != "latest" && v != "default" => v.to_owned(),
        keyword => {
            // The default is this registry's "latest" (RFC 0035 §6.7).
            authorize_unpinned(
                &svc.hot,
                &PackageId::new(registry, stack, "latest"),
                &identity.0,
                Action::ReleasesRead,
            )
            .await
            .map_err(AppError::from)?;
            let picked = if keyword == Some("latest") {
                let all: Vec<String> = versions_of(entry).into_iter().map(str::to_owned).collect();
                best_latest(&all)
            } else {
                default_version(entry).map(str::to_owned)
            };
            picked.ok_or_else(|| AppError::not_found(format!("stack '{stack}' has no versions")))?
        }
    };
    if version_entry(entry, &version).is_none() {
        return Err(AppError::not_found(format!(
            "the requested version {version} for stack {stack} does not exist in the registry"
        )));
    }
    Ok(version)
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
            local_svc: web::Data<Arc<LocalRegistryService>>,
            modes: web::Data<RegistryModeMap>,
        ) -> Result<impl Responder, AppError> {
            let registry = path.into_inner();
            let mode = modes.get(&registry);
            serve_index(&req, &registry, $doc, &identity, &svc, &local_svc, mode).await
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

/// Serve one artifact of a resolved coordinate: through the proxy funnel, or
/// as a member of the stored bundle.
async fn serve_artifact(
    r: &Reader<'_>,
    svc: web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    content_type: &'static str,
) -> Result<HttpResponse, AppError> {
    if r.local {
        let bytes = r.local_object(&pkg).await.map_err(AppError::from)?;
        return Ok(HttpResponse::Ok().content_type(content_type).body(bytes));
    }
    proxy_stream(
        svc,
        pkg,
        r.identity.clone(),
        Action::ReleasesRead,
        Some(content_type),
    )
    .await
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack) = path.into_inner();
    let r = Reader::new(&registry, &stack, &identity, &svc, &local_svc, &modes).await?;
    let version = resolve_version(&r, &stack, None).await?;
    let pkg =
        PackageId::new(&registry, &stack, &version).with_artifact(layer_artifact(DEVFILE_TITLE));
    serve_artifact(&r, svc.clone(), pkg, TEXT).await
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, version) = path.into_inner();
    let r = Reader::new(&registry, &stack, &identity, &svc, &local_svc, &modes).await?;
    let version = resolve_version(&r, &stack, Some(&version)).await?;
    let pkg =
        PackageId::new(&registry, &stack, &version).with_artifact(layer_artifact(DEVFILE_TITLE));
    serve_artifact(&r, svc.clone(), pkg, TEXT).await
}

/// The extractors every stack route takes beyond its path.
type Services = (
    web::Data<Arc<ProxyService>>,
    web::Data<Arc<LocalRegistryService>>,
    web::Data<RegistryModeMap>,
);

async fn serve_starter(
    registry: String,
    stack: String,
    version: Option<String>,
    name: String,
    identity: AuthIdentity,
    (svc, local_svc, modes): Services,
) -> Result<HttpResponse, AppError> {
    validate_starter(&name).map_err(AppError::from)?;
    let r = Reader::new(&registry, &stack, &identity, &svc, &local_svc, &modes).await?;
    let version = resolve_version(&r, &stack, version.as_deref()).await?;
    let pkg = PackageId::new(&registry, &stack, &version).with_artifact(starter_artifact(&name));
    let mut resp = serve_artifact(&r, svc.clone(), pkg, "application/zip").await?;
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, name) = path.into_inner();
    serve_starter(
        registry,
        stack,
        None,
        name,
        identity,
        (svc, local_svc, modes),
    )
    .await
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, version, name) = path.into_inner();
    serve_starter(
        registry,
        stack,
        Some(version),
        name,
        identity,
        (svc, local_svc, modes),
    )
    .await
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
pub async fn devfile_oci_ping() -> impl Responder {
    HttpResponse::Ok()
        .insert_header(API_VERSION)
        .json(OciPing {})
}

/// The stack's entry in the filtered index, checked against the namespace the
/// request names — the index's `links.self` is the only source of it.
async fn oci_stack(
    r: &Reader<'_>,
    ns: &str,
    stack: &str,
) -> Result<Result<Vec<(String, String)>, HttpResponse>, AppError> {
    if validate_namespace(ns).is_err() || validate_stack(stack).is_err() {
        return Ok(Err(oci_not_found(
            "NAME_INVALID",
            format!("'{ns}/{stack}' is not a repository name"),
        )));
    }
    let index = r.index().await?;
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
    r: &Reader<'_>,
    stack: &str,
    tags: &[(String, String)],
    digest: &str,
    want_manifest: bool,
) -> Option<(String, String)> {
    for (version, _) in tags {
        // The same facts either way: `extra.devfile` on a proxied version,
        // `index_metadata.devfile` on a published one.
        let facts = if r.local {
            let Some(row) = r
                .local_svc
                .get_version_meta(r.registry, stack, version)
                .await
            else {
                continue;
            };
            if !want_manifest
                && digest == format!("sha256:{}", hex::encode(Sha256::digest(CONFIG_BLOB)))
            {
                return Some((version.clone(), CONFIG_ARTIFACT.to_owned()));
            }
            row.index_metadata
        } else {
            let preq = proxy_request(
                PackageId::new(r.registry, stack, version).with_artifact(MANIFEST_ARTIFACT),
                r.identity,
                Action::ReleasesRead,
            );
            let Ok(meta) = r.svc.resolve_metadata_uncaptured_for(&preq).await else {
                continue;
            };
            meta.extra
        };
        if want_manifest {
            if facts
                .pointer("/devfile/manifestDigest")
                .and_then(|d| d.as_str())
                == Some(digest)
            {
                return Some((version.clone(), MANIFEST_ARTIFACT.to_owned()));
            }
            continue;
        }
        let layers = facts.pointer("/devfile/layers").and_then(|l| l.as_array());
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
    r: &Reader<'_>,
    svc: &web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    unknown_code: &str,
    content_type: &str,
) -> Result<HttpResponse, AppError> {
    let identity = r.identity;
    if r.local {
        let body = match r.local_object(&pkg).await {
            Ok(b) => b,
            Err(CoreError::NotFound(m) | CoreError::NotFoundWithheld(m)) => {
                return Ok(oci_not_found(unknown_code, m))
            }
            Err(CoreError::AccessDenied(m)) => {
                return Ok(oci_error(
                    actix_web::http::StatusCode::FORBIDDEN,
                    "DENIED",
                    &m,
                ))
            }
            Err(e) => return Err(AppError::from(e)),
        };
        // No identity headers: they name a proxy cache key, which is what a
        // bundle export reads, and a published version is not exported
        // (RFC 0039 §6.7).
        return Ok(oci_body(HttpResponse::Ok(), content_type, body));
    }
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
    let mut builder = HttpResponse::Ok();
    // The key, package and version these bytes are filed under — what a
    // bundle export reads back, rather than guessing a key from the route.
    for header in super::common::served_identity(&coordinate) {
        builder.insert_header(header);
    }
    Ok(oci_body(builder, content_type, body))
}

/// An OCI object, byte-exact, with the digest a `HEAD` must carry.
fn oci_body(
    mut builder: actix_web::HttpResponseBuilder,
    content_type: &str,
    body: impl Into<Bytes>,
) -> HttpResponse {
    let body = body.into();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
    builder
        .content_type(content_type)
        .insert_header(API_VERSION)
        .insert_header(("Docker-Content-Digest", digest.clone()))
        .insert_header(("ETag", format!("\"{digest}\"")))
        .body(body)
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, ns, stack, reference) = path.into_inner();
    let r = Reader::new(&registry, &stack, &identity, &svc, &local_svc, &modes).await?;
    let tags = match oci_stack(&r, &ns, &stack).await? {
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
        match artifact_for_digest(&r, &stack, &tags, &reference, true).await {
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
    serve_oci(&r, &svc, pkg, "MANIFEST_UNKNOWN", MANIFEST_MEDIA_TYPE).await
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, ns, stack, digest) = path.into_inner();
    if validate_digest(&digest).is_err() {
        return Ok(oci_not_found(
            "DIGEST_INVALID",
            format!("'{digest}' is not a digest"),
        ));
    }
    let r = Reader::new(&registry, &stack, &identity, &svc, &local_svc, &modes).await?;
    let tags = match oci_stack(&r, &ns, &stack).await? {
        Ok(t) => t,
        Err(resp) => return Ok(resp),
    };
    let Some((version, artifact)) = artifact_for_digest(&r, &stack, &tags, &digest, false).await
    else {
        return Ok(oci_not_found(
            "BLOB_UNKNOWN",
            format!("no allowed version of {stack} names blob {digest}"),
        ));
    };
    let pkg = PackageId::new(&registry, &stack, &version).with_artifact(artifact);
    serve_oci(&r, &svc, pkg, "BLOB_UNKNOWN", "application/octet-stream").await
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
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, ns, stack) = path.into_inner();
    let r = Reader::new(&registry, &stack, &identity, &svc, &local_svc, &modes).await?;
    let tags = match oci_stack(&r, &ns, &stack).await? {
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

/// RFC 0039 §4.5: the first publish of a name upstream lists needs
/// `devfile:stack:claim`, and an undecidable answer refuses rather than lets
/// through — a cold cache with upstream down is exactly a squat's window.
async fn check_claim(
    registry: &str,
    stack: &str,
    identity: &AuthIdentity,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
) -> Result<(), AppError> {
    if local_svc
        .holds_devfile_claim(registry, stack, &identity.0)
        .await
        .map_err(AppError::from)?
    {
        return Ok(());
    }
    let preq = proxy_request(
        PackageId::new(registry, "v2index", "__index__"),
        identity,
        Action::ReleasesList,
    );
    let index = match svc
        .multi_package_document(&preq, DocumentKind::Versions, "")
        .await
    {
        Ok(doc) => doc.body.as_json().cloned(),
        Err(e @ CoreError::AccessDenied(_)) => return Err(AppError::from(e)),
        Err(_) => None,
    };
    let Some(index) = index else {
        return Err(AppError::service_unavailable(format!(
            "cannot decide whether upstream lists a stack named '{stack}': its index is \
             unreachable and none is cached. Publishing a name upstream lists needs \
             'devfile:stack:claim', so this publish is refused until the index can be read"
        )));
    };
    if find_stack(&index, stack).is_some() {
        return Err(AppError::forbidden(format!(
            "upstream lists a stack named '{stack}'; publishing it here replaces upstream's \
             whole stack for every user of this registry, which needs 'devfile:stack:claim'"
        )));
    }
    Ok(())
}

/// Publish one stack version — the bundle `batlehub publish` packs from a stack directory (RFC 0039 §4.3).
#[utoipa::path(
    post,
    path = "/proxy/{registry}/api/stacks",
    tag = "proxy/devfile",
    params(("registry" = String, Path, description = "Registry name")),
    request_body(content = String, description = "An uncompressed tar of devfile.yaml, an optional archive.tar and optional starter-projects/{name}.zip", content_type = "application/x-tar"),
    responses(
        (status = 201, description = "Stored; the digest is the manifest's", body = DevfilePublished),
        (status = 400, description = "A member that is not one of the three, or a devfile with no valid metadata.name and metadata.version"),
        (status = 403, description = "Access denied, or a first publish of a name upstream lists without devfile:stack:claim"),
        (status = 404, description = "Unknown registry, or mode = proxy"),
        (status = 409, description = "That version is already published"),
        (status = 503, description = "Whether upstream lists the name could not be decided"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/api/stacks", guard = "is_devfile")]
pub async fn devfile_publish(
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
    let body = collect_payload(payload).await?;
    // The tar walk and the devfile read are CPU work over an upload, so off
    // the worker thread. Name and version come from inside the devfile and
    // are validated there before either reaches a storage key.
    let bytes = body.clone();
    let (bundle, index_metadata) = web::block(move || {
        read_bundle(&bytes).map(|b| {
            let facts = published_facts(&b, &bytes);
            (b, facts)
        })
    })
    .await
    .map_err(|e| AppError::internal(format!("reading the devfile bundle: {e}")))?
    .map_err(AppError::from)?;
    if modes.get(&registry) == RegistryMode::Hybrid
        && !local_svc
            .hosts(&registry, &bundle.name)
            .await
            .map_err(AppError::from)?
    {
        check_claim(&registry, &bundle.name, &identity, &svc, &local_svc).await?;
    }
    let digest = index_metadata
        .pointer("/devfile/manifestDigest")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let publish = PublishRequest {
        registry,
        name: bundle.name.clone(),
        version: bundle.version.clone(),
        checksum: hex::encode(Sha256::digest(&body)),
        artifact: body,
        index_metadata,
        unlisted: false,
        publisher: identity.0.clone(),
        signature_type: None,
        signature_bytes: None,
    };
    publish_and_respond(
        &local_svc,
        &notifications,
        publish,
        actix_web::http::StatusCode::CREATED,
        DevfilePublished {
            stack: bundle.name,
            version: bundle.version,
            digest,
        },
    )
    .await
}

/// Delete a published stack version (`releases:delete`); deleting the last one releases the name.
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/api/stacks/{stack}/{version}",
    tag = "proxy/devfile",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("stack" = String, Path, description = "Stack name"),
        ("version" = String, Path, description = "Stack version"),
    ),
    responses(
        (status = 200, description = "Deleted", body = DevfileDeleted),
        (status = 400, description = "Not a stack name or version"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not published here, unknown registry, or mode = proxy"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/proxy/{registry}/api/stacks/{stack}/{version}", guard = "is_devfile")]
pub async fn devfile_delete(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, stack, version) = path.into_inner();
    require_local_mode(&registry, &modes)?;
    validate_stack(&stack).map_err(AppError::from)?;
    validate_version(&version).map_err(AppError::from)?;
    if !local_svc
        .delete_version(&registry, &stack, &version, &identity.0)
        .await
        .map_err(AppError::from)?
    {
        return Err(AppError::not_found(format!(
            "{stack} {version} is not published here"
        )));
    }
    Ok(HttpResponse::Ok().json(DevfileDeleted { deleted: true }))
}
