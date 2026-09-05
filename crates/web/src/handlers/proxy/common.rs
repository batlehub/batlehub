use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{NotificationEvent, NotificationEventType, PackageId},
    error::CoreError,
    ports::{ByteStream, DocumentBody, DocumentKind, VersionDocument},
    services::{
        proxy::proxy_artifact_key, LocalRegistryService, ProxyRequest, ProxyResponse, ProxyService,
        PublishRequest,
    },
};

use crate::{
    error::AppError,
    extractors::AuthIdentity,
    middleware::{host_routing::host_routed_registry, proxy_trust::trusted_origin},
    services::NotificationService,
    RegistryHostMap, RegistryMap, RegistryModeMap,
};
use batlehub_core::entities::Action;

/// The public base URL of `registry` **as seen by this client** — the prefix every
/// self-referencing URL the server generates should be built on.
///
/// ```text
/// https://npm.acme.io                     on a request that arrived on npm1's host
/// https://hub.example.com/proxy/npm1      otherwise
/// ```
///
/// Callers format `{base}/…` with no literal `/proxy/` anywhere: the shape of the
/// ingress is this function's business, not theirs. The same contract applies to
/// the `base_url` parameter of the `LocalRegistryService` methods that build
/// metadata documents — it is a *registry* base, not a server origin.
///
/// The scheme and host come from [`trusted_origin`], so this is also the single
/// place that decides whether a forwarded header may influence a generated URL.
pub fn registry_public_base(req: &HttpRequest, registry: &str) -> String {
    let (scheme, host) = trusted_origin(req);
    let routed = host_routed_registry(req);

    // The client reached this very registry at a host root, so its URLs are
    // rooted there too.
    if routed.as_deref() == Some(registry) {
        return format!("{scheme}://{host}");
    }

    // Two cases where `{host}/proxy/{registry}` would not resolve for the client:
    // the registry has no subpath ingress at all (`path_routing = false`), or the
    // request arrived on *another* registry's host, where every path already
    // belongs to that other registry. Both need this registry's own advertised
    // URL, which is only known when host routing is configured.
    if let Some(map) = req.app_data::<web::Data<RegistryHostMap>>() {
        if map.is_host_only(registry) || routed.is_some() {
            if let Some(public) = map.public_url_for(registry) {
                return public;
            }
        }
    }

    format!("{scheme}://{host}/proxy/{registry}")
}

/// A publisher-supplied artifact signature: the bytes **and** the type that says
/// how to verify them.
///
/// One value rather than two `Option`s, because either half alone is a state
/// nothing downstream can act on. `X-Artifact-Signature` and `X-Signature-Type`
/// are independent headers, so "bytes with no type" was reachable, satisfied
/// `signing.required`, skipped the `allowed_types` allow-list because there was
/// no type to check, and then read as *absent* on the download path — an
/// artifact stored as signed whose signature was never verified against
/// `trusted_keys` at any point in its life (survey finding 13).
pub struct ArtifactSignature {
    pub bytes: Vec<u8>,
    pub signature_type: String,
}

impl ArtifactSignature {
    /// Back to the two columns the stored row has.
    ///
    /// The single place the coherent pair becomes a pair of `Option`s, so
    /// "bytes without type" exists only on the far side of persistence — where
    /// [`LocalRegistryService::get_artifact`] meets rows written before this
    /// check existed, and refuses to serve them unverified.
    pub fn split(sig: Option<Self>) -> (Option<Vec<u8>>, Option<String>) {
        match sig {
            Some(s) => (Some(s.bytes), Some(s.signature_type)),
            None => (None, None),
        }
    }
}

/// Decode `X-Artifact-Signature` (base64) and `X-Signature-Type` from a request.
///
/// `Ok(None)` when neither header is present. **Either header alone is a `400`**,
/// as is an `X-Artifact-Signature` that is not valid base64 — which previously
/// decoded to `None` and so was indistinguishable from sending no signature at
/// all, letting a malformed signature pass as an absent one under a policy that
/// merely required *a* signature.
///
/// A header whose bytes are not readable as a string is a `400` for the same
/// reason, and not `None`: base64 and a signature-type name are both ASCII, so
/// an unreadable value is a malformed signature, and `to_str().ok()` would
/// otherwise put it straight back in the "absent" bucket this exists to empty.
///
/// An **empty** value of either header is a `400` too, and for the third face of
/// the same defect: `""` is valid base64 for zero bytes, so `X-Artifact-Signature:`
/// with a permitted type decoded to `Some(vec![])` — present enough to satisfy
/// `signing.required` and to make the stored row read as signed, while carrying
/// nothing any key could ever verify.
pub fn extract_signature_headers(req: &HttpRequest) -> Result<Option<ArtifactSignature>, AppError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    fn header_str<'r>(req: &'r HttpRequest, name: &str) -> Result<Option<&'r str>, AppError> {
        req.headers()
            .get(name)
            .map(|v| {
                v.to_str().map_err(|_| {
                    AppError::bad_request(format!("{name} is not a readable header value"))
                })
            })
            .transpose()
    }
    let raw_sig = header_str(req, "X-Artifact-Signature")?.map(str::trim);
    let sig_type = header_str(req, "X-Signature-Type")?.map(|s| s.trim().to_owned());

    match (raw_sig, sig_type) {
        (None, None) => Ok(None),
        (Some(raw), Some(signature_type)) => {
            let bytes = STANDARD.decode(raw).map_err(|e| {
                AppError::bad_request(format!("X-Artifact-Signature is not valid base64: {e}"))
            })?;
            if bytes.is_empty() {
                return Err(AppError::bad_request(
                    "X-Artifact-Signature decoded to zero bytes; an empty signature can never be \
                     verified"
                        .to_owned(),
                ));
            }
            if signature_type.is_empty() {
                return Err(AppError::bad_request(
                    "X-Signature-Type is empty; a signature that names no algorithm can never be \
                     verified"
                        .to_owned(),
                ));
            }
            Ok(Some(ArtifactSignature {
                bytes,
                signature_type,
            }))
        }
        (Some(_), None) => Err(AppError::bad_request(
            "X-Artifact-Signature was sent without X-Signature-Type; a signature that names no \
             algorithm can never be verified"
                .to_owned(),
        )),
        (None, Some(ty)) => Err(AppError::bad_request(format!(
            "X-Signature-Type '{ty}' was sent without X-Artifact-Signature"
        ))),
    }
}

/// Append `X-Artifact-Signature` (base64) and `X-Signature-Type` headers to a response
/// if the package version has a stored signature.
pub async fn append_signature_headers(
    resp: &mut actix_web::HttpResponseBuilder,
    local_svc: &LocalRegistryService,
    registry: &str,
    name: &str,
    version: &str,
) {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    if let Some(meta) = local_svc.get_version_meta(registry, name, version).await {
        if let Some(ref sig) = meta.signature_bytes {
            resp.insert_header(("X-Artifact-Signature", STANDARD.encode(sig)));
        }
        if let Some(ref sig_type) = meta.signature_type {
            resp.insert_header(("X-Signature-Type", sig_type.as_str()));
        }
    }
}

/// Upload ceiling shared by every publish path (raw payloads and multipart
/// alike): prevents OOM from unbounded uploads before the service-layer size
/// check fires.
pub const MAX_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;

/// Drain an actix streaming body into a contiguous `Bytes` buffer, bounded by
/// [`MAX_UPLOAD_BYTES`].
pub async fn collect_payload(mut payload: web::Payload) -> Result<Bytes, AppError> {
    let mut raw = BytesMut::new();
    while let Some(chunk) = payload.next().await {
        let chunk = chunk.map_err(|e| AppError::bad_request(e.to_string()))?;
        if raw.len() as u64 + chunk.len() as u64 > MAX_UPLOAD_BYTES {
            return Err(AppError::from(CoreError::PayloadTooLarge(format!(
                "upload exceeds the {MAX_UPLOAD_BYTES}-byte limit"
            ))));
        }
        raw.extend_from_slice(&chunk);
    }
    Ok(raw.freeze())
}

/// Drain a storage `ByteStream` into contiguous `Bytes`.
pub async fn collect_storage_stream(mut stream: ByteStream) -> Result<Bytes, AppError> {
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
    }
    Ok(Bytes::from(buf))
}

/// Fire-and-forget notification dispatch.
///
/// Silently skips if notifications are not configured (`None`).
pub fn dispatch_notification(
    svc: &web::Data<Option<Arc<NotificationService>>>,
    event_type: NotificationEventType,
    registry: &str,
    package_name: &str,
    version: Option<String>,
    actor: &str,
) {
    if let Some(svc) = svc.as_ref().as_ref() {
        let event = NotificationEvent::new(event_type, registry, package_name, version, actor);
        svc.dispatch_event_background(event);
    }
}

/// Publish an artifact, fire a `PackagePublished` notification, and build a JSON
/// response carrying the publish quota headers.
///
/// Collapses the publish→notify→respond tail shared by every local/hybrid publish
/// handler. `status` is the success status (200/201) and `body` a serialisable
/// payload — a named DTO, so the schema the endpoint documents and the bytes it
/// sends come from the same type (RFC 0004 §6.1).
pub async fn publish_and_respond(
    local_svc: &LocalRegistryService,
    notification_svc: &web::Data<Option<Arc<NotificationService>>>,
    req: PublishRequest,
    status: actix_web::http::StatusCode,
    body: impl serde::Serialize,
) -> Result<HttpResponse, AppError> {
    let registry = req.registry.clone();
    let name = req.name.clone();
    let version = req.version.clone();
    let actor = req.publisher.user_id.clone().unwrap_or_default();

    let quota = local_svc.publish(req).await.map_err(AppError::from)?;
    dispatch_notification(
        notification_svc,
        NotificationEventType::PackagePublished,
        &registry,
        &name,
        Some(version),
        &actor,
    );

    let mut resp = HttpResponse::build(status);
    for (header, value) in quota.headers() {
        resp.insert_header((header, value));
    }
    Ok(resp.json(body))
}

/// Reject the request if `registry` is not of the expected type.
///
/// Returns `404 Not Found` with a descriptive message for both "wrong type" and
/// "registry does not exist" — the two cases are indistinguishable to the caller.
pub fn require_registry_type(
    registry: &str,
    expected: &str,
    map: &RegistryMap,
) -> Result<(), AppError> {
    match map.type_of(registry).as_deref() {
        Some(t) if t == expected => Ok(()),
        Some(_) => Err(AppError::not_found(format!(
            "registry '{registry}' is not a {expected} registry"
        ))),
        None => Err(AppError::not_found(format!(
            "unknown registry '{registry}'"
        ))),
    }
}

/// Reject registries that are not in local or hybrid mode.
pub fn require_local_mode(registry: &str, mode_map: &RegistryModeMap) -> Result<(), AppError> {
    match mode_map.get(registry) {
        RegistryMode::Local | RegistryMode::Hybrid => Ok(()),
        RegistryMode::Proxy => Err(AppError::not_found(format!(
            "registry '{registry}' is not a local registry (mode = proxy)"
        ))),
    }
}

/// Serve a RubyGems binary specs index (`specs`, `latest_specs`, or `prerelease_specs`).
///
/// Returns `404` for local-only registries — these compact indexes are only
/// meaningful in proxy/hybrid mode; local registries expose
/// `/api/v1/versions/{name}.json` instead.
pub async fn proxy_gem_specs(
    registry: &str,
    spec_type: &str,
    svc: web::Data<Arc<ProxyService>>,
    identity: AuthIdentity,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
) -> Result<HttpResponse, AppError> {
    require_registry_type(registry, "rubygems", map)?;

    if mode_map.get(registry) == RegistryMode::Local {
        return Err(AppError::not_found(
            "binary specs index is not available for local-only registries; use /api/v1/versions/{name}.json".to_owned(),
        ));
    }

    let pkg = PackageId::new(registry, "_index", spec_type);
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some("application/octet-stream"),
    )
    .await
}

/// `Content-Type` for a streamed artifact whose handler did not name one.
///
/// Not merely a tidy default — a security-relevant one. Artifacts are served
/// from the same origin as the admin SPA, which keeps bearer tokens in
/// `localStorage`. Several routes stream bytes an outsider controls: raw
/// repository files from GitHub / GitLab / Forgejo, npm tarballs, `.vsix`
/// bundles. With no `Content-Type` at all the browser falls back to MIME
/// sniffing, so a "raw file" containing HTML executes as a document on the
/// BatleHub origin. Declaring a non-renderable type (together with the
/// `X-Content-Type-Options: nosniff` sent by [`crate::security_headers`])
/// removes the sniffing step entirely.
const DEFAULT_ARTIFACT_CONTENT_TYPE: &str = "application/octet-stream";

/// The most release JSON this proxy will rewrite before serving it.
///
/// A release document is kilobytes — GitHub's own is a few hundred lines.
/// Above this the body is streamed through untouched rather than buffered:
/// an upstream that answers a release route with a hundred megabytes is not
/// something to hold in memory, and the URLs in it were never going to be
/// read by a client that asked for a release.
const MAX_REWRITTEN_RELEASE_BYTES: usize = 4 * 1024 * 1024;

/// [`proxy_stream`] for a forge release document (RFC 0019 §4.2 *API
/// reads*): the JSON is collected, its download URLs repointed at this proxy,
/// and served.
///
/// Why this route buffers when nothing else does: `tarball_url`,
/// `zipball_url` and `browser_download_url` are absolute upstream URLs, and
/// `mise`, `gh` and every other client that reads the release document
/// instead of building a path follows them straight to the forge — no
/// policy, no cache, no audit row. Rewriting them needs the whole document,
/// and a release document is small. A body that is not JSON, or is over
/// [`MAX_REWRITTEN_RELEASE_BYTES`], is served exactly as it came.
pub async fn proxy_release_document(
    svc: web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    identity: AuthIdentity,
    action: Action,
    public_base: String,
) -> Result<HttpResponse, AppError> {
    let owner_repo = pkg.name.clone();
    let response = proxy_stream(svc, pkg, identity, action, Some("application/json")).await?;
    if !response.status().is_success() || public_base.is_empty() {
        return Ok(response);
    }
    let (parts, body) = response.into_parts();
    let bytes = match actix_web::body::to_bytes_limited(body, MAX_REWRITTEN_RELEASE_BYTES).await {
        Ok(Ok(b)) => b,
        // Over the cap, or a body that failed mid-collection: there is
        // nothing left to stream (the body was consumed), so this is an
        // error rather than a silent pass-through.
        _ => {
            return Err(AppError::bad_gateway(
                "the upstream release document could not be read",
            ))
        }
    };
    let Ok(mut doc) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(parts.set_body(actix_web::body::BoxBody::new(bytes)));
    };
    batlehub_core::services::blocking::forge::rewrite_release_urls(
        &mut doc,
        &public_base,
        &owner_repo,
    );
    let rewritten = serde_json::to_vec(&doc).unwrap_or_else(|_| bytes.to_vec());
    Ok(parts.set_body(actix_web::body::BoxBody::new(rewritten)))
}

/// Send a proxy request and stream the result back to the HTTP client.
///
/// Pass `content_type = Some("application/json")` to set an explicit
/// `Content-Type` header on the response; pass `None` to fall back to
/// [`DEFAULT_ARTIFACT_CONTENT_TYPE`]. `None` never means "send no
/// `Content-Type`" — see that constant for why.
/// Where the proxy says it keeps the bytes it just served.
///
/// RFC 0008 §4.2 wants a plan to be "a statement about BatleHub's storage,
/// rather than about a URL", and the CLI cannot derive that statement: the key
/// is a function of the *route*, not of the upstream URL — a GitHub asset by
/// name lands under `…/{tag}/filename/{file}`, the same asset by id under
/// `…/unknown/{id}`, a generic mirror under `…/repo/_/{path}`, and a forge
/// archive under its commit. Deriving it a second time in the CLI would be a
/// second routing table to keep in step; reporting it is one header.
///
/// It discloses nothing: every segment is already in the URL the caller asked
/// for, and the value is [`proxy_artifact_key`] — the one definition the cache
/// write, the eviction sweep and the bundle importer already share.
pub const STORAGE_KEY_HEADER: &str = "X-BatleHub-Storage-Key";

/// The coordinate those bytes are filed under, beside the key.
///
/// The key alone cannot be read back into a coordinate: it is
/// `{registry}/{name}/{version}[/{artifact}]` and a name may contain slashes
/// (`cli/cli`, `@scope/pkg`), so `gh/cli/cli/v2.60.0/filename/gh.tar.gz` splits
/// four plausible ways and only one of them is right. A bundle importing it
/// wrongly files the *verdict* against a package nobody will ask about, which
/// is the failure that would look like "the verdict was lost".
pub const PACKAGE_HEADER: &str = "X-BatleHub-Package";
pub const VERSION_HEADER: &str = "X-BatleHub-Version";

/// The `(key, name, version)` triple a response reports about itself.
fn served_identity(pkg: &PackageId) -> [(&'static str, String); 3] {
    [
        (STORAGE_KEY_HEADER, proxy_artifact_key(pkg)),
        (PACKAGE_HEADER, pkg.name.clone()),
        (VERSION_HEADER, pkg.version.clone()),
    ]
}

pub async fn proxy_stream(
    svc: web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    identity: AuthIdentity,
    action: Action,
    content_type: Option<&str>,
) -> Result<HttpResponse, AppError> {
    let identity = identity.0;
    let coordinate = pkg.clone();
    let req = ProxyRequest {
        package_id: pkg,
        identity: identity.clone(),
        action: action.to_owned(),
        ip_address: None,
        user_agent: None,
    };
    let response = svc.handle(req).await.map_err(AppError::from)?;
    // RFC 0018 §4.2: a `warned` artifact is the same bytes with the verdict's
    // headers on them.
    let (response, warned) = match response {
        ProxyResponse::Warned { response, verdict } => (*response, Some(verdict)),
        other => (other, None),
    };
    let now = chrono::Utc::now();
    match response {
        // A security verdict answers in the registry's own shape, and as a
        // 404 to a caller who may not learn why (RFC 0018 §4.2).
        ProxyResponse::Denied {
            verdict: Some(verdict),
            ..
        } => Ok(crate::handlers::security::hold_http_response(
            &svc,
            &coordinate,
            &identity,
            &verdict,
        )
        .await),
        ProxyResponse::Denied { reason, .. } => Err(AppError::forbidden(reason)),
        ProxyResponse::Stream(stream) => {
            let body = stream
                .filter_map(|chunk| async move { chunk.ok().map(Ok::<Bytes, actix_web::Error>) });
            let mut builder = HttpResponse::Ok();
            builder.content_type(content_type.unwrap_or(DEFAULT_ARTIFACT_CONTENT_TYPE));
            for header in served_identity(&coordinate) {
                builder.insert_header(header);
            }
            if let Some(v) = &warned {
                crate::handlers::security::verdict_headers(&mut builder, v, now);
            }
            Ok(builder.streaming(body))
        }
        // RFC 0008-bis: a release composed from the held assets, on the
        // route that otherwise streams the forge's own JSON. Its own
        // content type, the listing header, and no storage-key header —
        // nothing is stored under this coordinate.
        ProxyResponse::Document(doc) => {
            let mut builder = HttpResponse::Ok();
            builder.content_type(doc.content_type.clone());
            listing_headers(&mut builder, &doc);
            if let Some(v) = &warned {
                crate::handlers::security::verdict_headers(&mut builder, v, now);
            }
            Ok(match doc.body {
                DocumentBody::Json(v) => builder.json(v),
                DocumentBody::Text(s) => builder.body(s),
            })
        }
        // RFC 0019 §4.2 *Response headers*: which kind of ref was asked for and
        // which commit answered. Spelled like the existing `X-BatleHub-Cache`.
        ProxyResponse::ForgeStream {
            stream,
            resolved,
            keyed,
        } => {
            let body = stream
                .filter_map(|chunk| async move { chunk.ok().map(Ok::<Bytes, actix_web::Error>) });
            let mut builder = HttpResponse::Ok();
            builder
                .content_type(content_type.unwrap_or(DEFAULT_ARTIFACT_CONTENT_TYPE))
                .insert_header(("X-BatleHub-Ref-Kind", resolved.kind.as_str()))
                .insert_header(("X-BatleHub-Resolved-Commit", resolved.sha.as_str()))
                // The ref as the *client* spelled it. On a commit-keyed
                // archive the coordinate below has already become the SHA,
                // so this is the only place the asked-for name survives —
                // and RFC 0008's bundle has to carry the pair, because a
                // disconnected instance cannot resolve a ref at all.
                .insert_header(("X-BatleHub-Ref-Requested", resolved.requested.as_str()));
            // The commit-keyed coordinate, not the one asked for.
            for header in served_identity(&keyed) {
                builder.insert_header(header);
            }
            // RFC 0019 phase 2 — the commit this ref answered with last time,
            // when it differs. On a registry with `[security]` the same fact
            // is `TAG_MOVED` in the verdict; on one without there is no
            // verdict to carry it, and this header is the whole of the `warn`
            // outcome §6.1 promises. Present for a branch too, where it is
            // the ordinary "the branch advanced".
            if let Some(previous) = resolved.previous.as_deref() {
                builder.insert_header(("X-BatleHub-Ref-Previous-Commit", previous));
            }
            if let Some(v) = &warned {
                crate::handlers::security::verdict_headers(&mut builder, v, now);
            }
            Ok(builder.streaming(body))
        }
        // A `Warned` never wraps a `Warned`; unwrapped above.
        ProxyResponse::Warned { .. } => Err(AppError::internal("nested verdict response")),
    }
}

/// [`proxy_stream`] for a *version listing* rather than an artifact.
///
/// The difference is not cosmetic. `proxy_stream` asks the registry client for
/// an **artifact** and hands the bytes through untouched — so on a listing route
/// it forwards the upstream's own document, blocked versions and all, and
/// labels it `application/octet-stream`. This calls
/// [`ProxyService::version_document`], which fetches the document, removes
/// administratively blocked versions, repairs whatever that protocol calls
/// "newest", and answers in the protocol's own content type.
///
/// For the routes that have already resolved their own local/hybrid branch;
/// [`serve_local_or_proxy_document`] is the version that handles both.
pub async fn proxy_document(
    svc: web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    identity: AuthIdentity,
    action: Action,
    doc_kind: DocumentKind,
    public_base: String,
) -> Result<HttpResponse, AppError> {
    Ok(document_response(
        fetch_proxy_document(svc, pkg, identity, action, doc_kind, public_base).await?,
    ))
}

/// [`proxy_document`] without the HTTP response, for the handlers that have to
/// compose two documents before answering — Go's `@latest` against its filtered
/// `@v/list`, RubyGems' gem document against its versions API.
pub async fn fetch_proxy_document(
    svc: web::Data<Arc<ProxyService>>,
    pkg: PackageId,
    identity: AuthIdentity,
    action: Action,
    doc_kind: DocumentKind,
    public_base: String,
) -> Result<VersionDocument, AppError> {
    let req = ProxyRequest {
        package_id: pkg,
        identity: identity.0,
        action: action.to_owned(),
        ip_address: None,
        user_agent: None,
    };
    svc.version_document(&req, doc_kind, &public_base)
        .await
        .map_err(AppError::from)
}

/// Name the file this response is, for the routes whose URL does not.
///
/// Three of the artifact routes end in a verb rather than a filename —
/// `…/{version}/tarball`, `…/{version}/download`, `…/{version}/vsix` — because
/// that is the address the package manager is specified to fetch. A browser
/// saving one of those has nothing else to go on and writes a file literally
/// called `tarball`, `download` or `vsix`, with no extension: the console's own
/// download link produced files nobody could identify, let alone `npm install`.
///
/// The package managers themselves are unaffected — none of them reads
/// `Content-Disposition`; they already know what they asked for. The same
/// device, for the same reason, as
/// [`jetbrains_marketplace::files::plugin_attachment_value`].
///
/// [`crate::handlers::sanitize_filename`] runs over the name because it is built
/// from a caller-supplied coordinate: the routes validate the coordinate, but
/// validation still admits `"` and `/`, and either would break out of the
/// quoted-string or make the name more than one path segment.
pub fn attachment_disposition(
    file_name: &str,
) -> Result<actix_web::http::header::HeaderValue, AppError> {
    let safe = crate::handlers::sanitize_filename(file_name);
    actix_web::http::header::HeaderValue::from_str(&format!("attachment; filename=\"{safe}\""))
        .map_err(|e| AppError::bad_request(format!("invalid artifact file name: {e}")))
}

/// Options controlling [`serve_local_or_proxy_artifact`]'s behaviour.
pub struct LocalOrProxyArtifactOpts<'a> {
    /// Suffix passed to `PackageId::with_artifact(...)` on the proxy fallback,
    /// e.g. `"gem"`, `"dl"`, `"tarball"`, or a full filename.
    pub artifact_suffix: &'a str,
    /// `Content-Type` set on a local/hybrid hit.
    pub local_content_type: &'static str,
    /// `Content-Type` passed to [`proxy_stream`] on the proxy fallback.
    pub proxy_content_type: Option<&'static str>,
    /// `resource_type` passed to [`proxy_stream`], e.g. `"releases:read"`, `"source:read"`.
    pub action: Action,
    /// Call `local_svc.check_prerelease_access(...)` before `get_artifact` in
    /// the Local/Hybrid branches.
    pub check_prerelease: bool,
    /// Call [`append_signature_headers`] on a local/hybrid hit.
    pub append_signature: bool,
}

/// Serve an artifact from local storage (Local/Hybrid mode) or fall back to
/// streaming it from the upstream registry (Proxy mode, or a Hybrid miss).
///
/// This is the shared shape behind `gem_download`, `download_crate`,
/// `download_tarball`, and similar registry-artifact download handlers.
#[allow(clippy::too_many_arguments)]
pub async fn serve_local_or_proxy_artifact(
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    mode_map: &RegistryModeMap,
    registry: &str,
    name: &str,
    version: &str,
    identity: AuthIdentity,
    opts: LocalOrProxyArtifactOpts<'_>,
) -> Result<HttpResponse, AppError> {
    let mode = mode_map.get(registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // No `authorize_read` here any more: `get_artifact` runs the registry's
        // rule chain itself, against the `resource_type` passed to it, so a
        // local hit is gated by construction rather than by every handler
        // remembering to ask first. This helper used to be the compensation for
        // that — and the eight handlers that did not use it are survey findings
        // 4 to 10.
        if opts.check_prerelease {
            local_svc
                .check_prerelease_access(registry, version, &identity)
                .await
                .map_err(AppError::from)?;
        }
        match local_svc
            .get_artifact(registry, name, version, opts.action, &identity)
            .await
        {
            Ok(bytes) => {
                let mut resp = HttpResponse::Ok();
                resp.content_type(opts.local_content_type);
                if opts.append_signature {
                    append_signature_headers(&mut resp, &local_svc, registry, name, version).await;
                }
                return Ok(resp.body(bytes));
            }
            // Not found locally in hybrid mode; fall through to upstream.
            Err(CoreError::NotFound(_)) if matches!(mode, RegistryMode::Hybrid) => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }

    let pkg = PackageId::new(registry, name, version).with_artifact(opts.artifact_suffix);
    proxy_stream(svc, pkg, identity, opts.action, opts.proxy_content_type).await
}

/// Serve JSON metadata from local storage (Local/Hybrid mode) or fall back to
/// streaming it from the upstream registry (Proxy mode, or a Hybrid miss).
///
/// This is the shared shape behind `get_packument`, `get_version`, `gem_info`,
/// The local half of the mode ladder: `Ok(None)` means "fall through upstream".
///
/// Local and hybrid both try the local store first; what differs is what a miss
/// means. In hybrid a miss is a fall-through, in local it is the answer — and
/// getting that backwards either hides a published package or turns a proxy
/// read into a `404`. Proxy mode never looks.
///
/// RBAC is enforced here rather than left to the local fetch: that fetch checks
/// per-package `Visibility` only, not the registry rule chain the proxy
/// fall-through would run.
async fn local_first<T, F, Fut>(
    svc: &ProxyService,
    mode: RegistryMode,
    identity: &AuthIdentity,
    local_fetch: F,
    not_found_msg: String,
    pkg: &PackageId,
    action: Action,
) -> Result<Option<T>, AppError>
where
    F: FnOnce(batlehub_core::entities::Identity) -> Fut,
    Fut: std::future::Future<Output = Result<T, CoreError>>,
{
    if !matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        return Ok(None);
    }
    svc.authorize_read(pkg, &identity.0, action)
        .await
        .map_err(AppError::from)?;
    match local_fetch(identity.0.clone()).await {
        Ok(x) => Ok(Some(x)),
        Err(CoreError::NotFound(_)) if matches!(mode, RegistryMode::Hybrid) => Ok(None),
        Err(CoreError::NotFound(_)) => Err(AppError::not_found(not_found_msg)),
        Err(e) => Err(AppError::from(e)),
    }
}

/// `gem_versions`, `goproxy_latest`, and `composer_p2_metadata`: check the
/// registry mode, try `local_fetch` in Local/Hybrid mode, fall through to
/// `proxy_stream` on a Hybrid miss (or directly in Proxy mode).
#[allow(clippy::too_many_arguments)]
pub async fn serve_local_or_proxy_json<T, F, Fut>(
    svc: web::Data<Arc<ProxyService>>,
    mode_map: &RegistryModeMap,
    registry: &str,
    identity: AuthIdentity,
    local_fetch: F,
    not_found_msg: String,
    pkg: PackageId,
    action: Action,
    proxy_content_type: Option<&str>,
) -> Result<HttpResponse, AppError>
where
    T: serde::Serialize,
    F: FnOnce(batlehub_core::entities::Identity) -> Fut,
    Fut: std::future::Future<Output = Result<T, CoreError>>,
{
    let local = local_first(
        &svc,
        mode_map.get(registry),
        &identity,
        local_fetch,
        not_found_msg,
        &pkg,
        action,
    )
    .await?;
    if let Some(x) = local {
        return Ok(HttpResponse::Ok().content_type("application/json").json(x));
    }
    proxy_stream(svc, pkg, identity, action, proxy_content_type).await
}

/// [`serve_local_or_proxy_json`] for routes whose proxy fall-through serves a
/// *document* the proxy composes, not an artifact it streams.
///
/// The difference matters for version listings. `serve_local_or_proxy_json`
/// falls through to `proxy_stream`, which asks the registry client for an
/// artifact — for an npm packument route that yields the `latest` tarball,
/// served as `application/octet-stream` under a URL npm expects JSON from. This
/// helper calls [`ProxyService::version_document`] instead, which fetches the
/// upstream document, removes administratively blocked versions and repoints
/// artifact URLs at this proxy.
#[allow(clippy::too_many_arguments)]
pub async fn serve_local_or_proxy_document<T, F, Fut>(
    svc: web::Data<Arc<ProxyService>>,
    mode_map: &RegistryModeMap,
    registry: &str,
    identity: AuthIdentity,
    local_fetch: F,
    not_found_msg: String,
    pkg: PackageId,
    action: Action,
    doc_kind: DocumentKind,
    local_content_type: &str,
    public_base: String,
) -> Result<HttpResponse, AppError>
where
    T: serde::Serialize,
    F: FnOnce(batlehub_core::entities::Identity) -> Fut,
    Fut: std::future::Future<Output = Result<T, CoreError>>,
{
    let local = local_first(
        &svc,
        mode_map.get(registry),
        &identity,
        local_fetch,
        not_found_msg,
        &pkg,
        action,
    )
    .await?;
    if let Some(x) = local {
        return Ok(HttpResponse::Ok().content_type(local_content_type).json(x));
    }

    let doc = fetch_proxy_document(svc, pkg, identity, action, doc_kind, public_base).await?;
    Ok(document_response(doc))
}

/// [`serve_local_or_proxy_document`] for a handler that reads *from* the
/// document instead of returning it.
///
/// `npm dist-tag ls` is the caller: it answers with one field of the packument,
/// and the packument is the only place that field exists. Taking it from a
/// second source would be a second rendering of the same facts, which is the
/// drift `npm_dist_tags` is documented to avoid — but the first version of that
/// handler avoided it by calling [`ProxyService::version_document`] directly,
/// which skips the mode ladder entirely. On a local registry that asks upstream
/// for a package upstream has never heard of, so `npm dist-tag ls` answered
/// `404` for a package `npm view` had just described (the RFC 0009 §12.15
/// failure, in another route; found by tests/heavy/npm.sh).
///
/// JSON only: the document must be one the caller can read fields out of, and a
/// text document (a compact index, a simple page) is not that.
#[allow(clippy::too_many_arguments)]
pub async fn local_or_proxy_document_value<T, F, Fut>(
    svc: &web::Data<Arc<ProxyService>>,
    mode_map: &RegistryModeMap,
    registry: &str,
    identity: AuthIdentity,
    local_fetch: F,
    not_found_msg: String,
    pkg: PackageId,
    action: Action,
    doc_kind: DocumentKind,
    public_base: String,
) -> Result<serde_json::Value, AppError>
where
    T: serde::Serialize,
    F: FnOnce(batlehub_core::entities::Identity) -> Fut,
    Fut: std::future::Future<Output = Result<T, CoreError>>,
{
    let local = local_first(
        svc,
        mode_map.get(registry),
        &identity,
        local_fetch,
        not_found_msg,
        &pkg,
        action,
    )
    .await?;
    if let Some(x) = local {
        return serde_json::to_value(x)
            .map_err(|e| AppError::internal(format!("could not render the local document: {e}")));
    }

    let doc =
        fetch_proxy_document(svc.clone(), pkg, identity, action, doc_kind, public_base).await?;
    doc.body.as_json().cloned().ok_or_else(|| {
        AppError::internal("upstream document is not JSON and cannot be read as one".to_owned())
    })
}

/// Turn a filtered [`VersionDocument`] into an HTTP response in its own
/// encoding.
///
/// The content type comes from the document rather than being hard-coded: these
/// routes carry XML (`maven-metadata.xml`), HTML (a PyPI simple page) and NDJSON
/// (cargo's sparse index) as well as JSON, and serving any of them as
/// `application/json` — or, as the pre-`serve_local_or_proxy_document` packument
/// route did, as `application/octet-stream` — breaks the client that asked.
/// `X-BatleHub-Listing: synthesised` on a listing composed from the held
/// set, with the count beside it (RFC 0008-bis §4.2). A held document —
/// cached from upstream — carries neither.
pub const LISTING_HEADER: &str = "X-BatleHub-Listing";
pub const LISTING_HELD_HEADER: &str = "X-BatleHub-Listing-Held";

/// Mark a listing response that was composed rather than fetched. Every
/// handler that builds its own response from a `fetch_proxy_document`
/// result — rather than through [`document_response`] — has to call this
/// for the kinds RFC 0008-bis synthesises for it; the PyPI simple page
/// does, the rest render kinds that are not composed yet (its phase 3).
pub fn listing_headers(builder: &mut actix_web::HttpResponseBuilder, doc: &VersionDocument) {
    if let Some(held) = doc.synthesised {
        builder.insert_header((LISTING_HEADER, "synthesised"));
        builder.insert_header((LISTING_HELD_HEADER, held.to_string()));
    }
}

pub fn document_response(doc: VersionDocument) -> HttpResponse {
    let mut builder = HttpResponse::Ok();
    builder.content_type(doc.content_type.clone());
    listing_headers(&mut builder, &doc);
    match doc.body {
        DocumentBody::Json(v) => builder.json(v),
        DocumentBody::Text(s) => builder.body(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map_with(registry: &str, type_: &str) -> RegistryMap {
        let mut m = HashMap::new();
        m.insert(registry.to_owned(), type_.to_owned());
        RegistryMap::from(m)
    }

    #[test]
    fn require_registry_type_ok() {
        let map = map_with("r1", "nuget");
        assert!(require_registry_type("r1", "nuget", &map).is_ok());
    }

    #[test]
    fn require_registry_type_wrong_type() {
        let map = map_with("r1", "cargo");
        assert!(require_registry_type("r1", "nuget", &map).is_err());
    }

    #[test]
    fn require_registry_type_unknown_registry() {
        let map = RegistryMap::from(HashMap::new());
        assert!(require_registry_type("nonexistent", "nuget", &map).is_err());
    }
}
