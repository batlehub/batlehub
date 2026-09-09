//! The RubyGems compact index — `/versions`, `/info/{gem}`, `/names`.
//!
//! RFC 0009 §7.3, and the reason it is phase 2 rather than part of the long
//! tail: this is not a coverage gap, it is a live block leak.
//!
//! Bundler resolves from the compact index first, falls back to the dependency
//! API, and falls back again to `specs.4.8.gz`. This server served only the
//! last, and `RegistryKind::listing_filter()` marks it `Unsupported` — hiding a
//! version from a Ruby Marshal index would need a Marshal encoder in Rust. So
//! **every `bundle install` read the one index we do not filter**: a blocked gem
//! version was offered to the resolver, chosen, written to `Gemfile.lock`, and
//! only then refused at download. Exactly the mid-resolve failure RFC 0006
//! exists to prevent, for the default client of a whole ecosystem.
//!
//! All three documents are plain text, so `DocumentBody::Text` carries them and
//! the Marshal objection never arises.
//!
//! ## Which document filters where
//!
//! - **`/versions`** is the whole registry, one line per gem, so its blocked set
//!   is a registry's worth: `multi_package_document`, and the same up-to-30-second
//!   snapshot lag conda's `repodata.json` carries.
//! - **`/info/{gem}`** is one gem: `version_document`, filtered per request, so a
//!   block reaches it immediately.
//! - **`/names`** lists gem *names* and no versions. It names nothing to hide, so
//!   it is served unfiltered — a gem with one blocked version still exists.
//!
//! ## Which mode answers
//!
//! All three shipped proxy-only: whatever the registry's mode, they asked
//! upstream. So a gem published to a **local** registry was invisible to
//! `bundle install` — the one client that matters — while the JSON APIs, which
//! Bundler only falls back to, showed it. Worse, a `local` registry answered
//! `/versions` with rubygems.org's index, which is not a thing a local registry
//! should be able to do at all. Measured with Bundler 4.0.17 against a real
//! server: `Could not find gem 'e2eprobe'`, immediately after publishing it
//! (RFC 0009 §12.15).
//!
//! Local mode is now generated from the database, hybrid appends the local gems
//! to the upstream document, and proxy is unchanged.
//!
//! ## Incremental fetch
//!
//! All three documents answer `If-None-Match` and `Range` — see
//! [`super::range`]. Bundler keeps its copy and asks for the tail; this server
//! used to reply `200` with the whole thing every time, which is legal and
//! discards the reason the format exists.

use super::range::compact_response;
use super::{
    get, require_registry_type, web, AppError, Arc, AuthIdentity, HttpRequest, HttpResponse,
    PackageId, ProxyService, RegistryMap, Responder,
};
use crate::handlers::schemas::ProtocolDocument;
use crate::RegistryModeMap;
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::Action;
use batlehub_core::error::CoreError;
use batlehub_core::services::LocalRegistryService;

/// Render a text document as the compact index expects it.
fn text_response(req: &HttpRequest, doc: batlehub_core::ports::VersionDocument) -> HttpResponse {
    let synthesised = doc.synthesised;
    let mut resp = compact_response(req, document_text(doc));
    mark_synthesised(&mut resp, synthesised);
    resp
}

/// A compact document composed from the held set says so (RFC 0008-bis
/// §4.2), the way every other listing response does. On the response
/// rather than its builder, because a compact answer may be a `206` or a
/// `304` that `compact_response` decided on.
fn mark_synthesised(resp: &mut HttpResponse, synthesised: Option<u32>) {
    let Some(held) = synthesised else {
        return;
    };
    let headers = resp.headers_mut();
    headers.insert(
        actix_web::http::header::HeaderName::from_static("x-batlehub-listing"),
        actix_web::http::header::HeaderValue::from_static("synthesised"),
    );
    if let Ok(v) = actix_web::http::header::HeaderValue::from_str(&held.to_string()) {
        headers.insert(
            actix_web::http::header::HeaderName::from_static("x-batlehub-listing-held"),
            v,
        );
    }
}

/// Which whole-registry compact document is being served.
///
/// `/versions` and `/names` are one procedure with three substitutions, and
/// were written out twice. This names the substitutions.
#[derive(Clone, Copy)]
enum Compact {
    Versions,
    Names,
}

impl Compact {
    /// The synthetic package name the coordinate carries. The document is
    /// scoped to the registry rather than to a gem, so it needs one.
    fn coordinate(self) -> &'static str {
        match self {
            Compact::Versions => "_versions",
            Compact::Names => "_names",
        }
    }

    fn document_kind(self) -> batlehub_core::ports::DocumentKind {
        match self {
            Compact::Versions => batlehub_core::ports::DocumentKind::COMPACT_VERSIONS,
            Compact::Names => batlehub_core::ports::DocumentKind::COMPACT_NAMES,
        }
    }

    /// The half generated from what this registry has published.
    async fn local(
        self,
        local_svc: &LocalRegistryService,
        registry: &str,
        identity: &batlehub_core::entities::Identity,
    ) -> Result<String, batlehub_core::error::CoreError> {
        match self {
            Compact::Versions => {
                local_svc
                    .get_rubygems_compact_versions(registry, identity)
                    .await
            }
            Compact::Names => {
                local_svc
                    .get_rubygems_compact_names(registry, identity)
                    .await
            }
        }
    }
}

/// Serve one of the two whole-registry compact documents.
#[allow(clippy::too_many_arguments)]
async fn serve_compact(
    which: Compact,
    http_req: &HttpRequest,
    registry: String,
    identity: AuthIdentity,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
) -> Result<HttpResponse, AppError> {
    require_registry_type(&registry, "rubygems", map)?;
    let mode = mode_map.get(&registry);

    let local = if mode == RegistryMode::Local || mode == RegistryMode::Hybrid {
        which
            .local(local_svc, &registry, &identity.0)
            .await
            .map_err(AppError::from)?
    } else {
        String::new()
    };
    if mode == RegistryMode::Local {
        return Ok(text_body(http_req, local));
    }

    // The registry itself is the coordinate: this document is scoped to the
    // whole channel, not to a gem — the same shape conda's repodata uses.
    let req = batlehub_core::services::ProxyRequest {
        package_id: PackageId::new(&registry, which.coordinate(), "__compact__"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let doc = svc
        .multi_package_document(&req, which.document_kind(), "")
        .await
        .map_err(AppError::from)?;
    let synthesised = doc.synthesised;
    let mut resp = text_body(http_req, merge_compact(document_text(doc), &local));
    mark_synthesised(&mut resp, synthesised);
    Ok(resp)
}

/// The whole-registry version list Bundler fetches first.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/versions",
    tag = "proxy/rubygems",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Compact index version list", body = ProtocolDocument, content_type = "text/plain"),
        (status = 206, description = "The tail of the document, for a client that already holds the rest", body = ProtocolDocument, content_type = "text/plain"),
        (status = 304, description = "The client's copy is current (`If-None-Match` matched)"),
        (status = 416, description = "The requested range starts past the end of the document"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown or non-rubygems registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/versions")]
pub async fn gem_compact_versions(
    http_req: HttpRequest,
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    serve_compact(
        Compact::Versions,
        &http_req,
        path.into_inner(),
        identity,
        &svc,
        &local_svc,
        &map,
        &mode_map,
    )
    .await
}

/// Render a compact-index document from text we generated ourselves.
fn text_body(req: &HttpRequest, body: String) -> HttpResponse {
    compact_response(req, body)
}

fn document_text(doc: batlehub_core::ports::VersionDocument) -> String {
    match doc.body {
        batlehub_core::ports::DocumentBody::Text(t) => t,
        batlehub_core::ports::DocumentBody::Json(_) => String::new(),
    }
}

/// Append the locally published entries to an upstream compact document.
///
/// Hybrid only. The `---` separator and any header stay with the upstream
/// document; the local lines follow, because a compact document is a list and
/// order carries no meaning to Bundler. Local wins nothing here — a gem name
/// present in both simply appears twice, which is what a hybrid registry is.
fn merge_compact(upstream: String, local: &str) -> String {
    let local_lines: Vec<&str> = local
        .lines()
        .filter(|l| !l.is_empty() && *l != "---" && !l.starts_with("created_at:"))
        .collect();
    if local_lines.is_empty() {
        return upstream;
    }
    let mut out = upstream;
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("---\n");
    }
    out.push_str(&local_lines.join("\n"));
    out.push('\n');
    out
}

/// Every gem name in the registry.
///
/// Unfiltered by design: it names no version, so there is nothing in it a block
/// applies to. A gem with one blocked version is still a gem that exists, and
/// removing its name would make `bundle install` report it as nonexistent
/// rather than as partly restricted.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/names",
    tag = "proxy/rubygems",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Compact index gem names", body = ProtocolDocument, content_type = "text/plain"),
        (status = 206, description = "The tail of the document, for a client that already holds the rest", body = ProtocolDocument, content_type = "text/plain"),
        (status = 304, description = "The client's copy is current (`If-None-Match` matched)"),
        (status = 416, description = "The requested range starts past the end of the document"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown or non-rubygems registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/names")]
pub async fn gem_compact_names(
    http_req: HttpRequest,
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    serve_compact(
        Compact::Names,
        &http_req,
        path.into_inner(),
        identity,
        &svc,
        &local_svc,
        &map,
        &mode_map,
    )
    .await
}

/// One gem's versions and dependencies — what Bundler resolves against.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/info/{gem}",
    tag = "proxy/rubygems",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("gem"      = String, Path, description = "Gem name"),
    ),
    responses(
        (status = 200, description = "Compact index gem info", body = ProtocolDocument, content_type = "text/plain"),
        (status = 206, description = "The tail of the document, for a client that already holds the rest", body = ProtocolDocument, content_type = "text/plain"),
        (status = 304, description = "The client's copy is current (`If-None-Match` matched)"),
        (status = 416, description = "The requested range starts past the end of the document"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or gem"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/info/{gem}")]
pub async fn gem_compact_info(
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, gem) = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;
    let mode = mode_map.get(&registry);

    // The gem name reaches an upstream URL, so it is validated at the edge for
    // a clean 400 rather than relying on the deeper funnels (CLAUDE.md, rule 3).
    batlehub_core::services::validate_package_name(&gem)
        .map_err(|e| AppError::bad_request(format!("invalid gem name: {e}")))?;

    if mode == RegistryMode::Local || mode == RegistryMode::Hybrid {
        match local_svc
            .get_rubygems_compact_info(&registry, &gem, &identity.0)
            .await
        {
            Ok(info) => return Ok(text_body(&http_req, info)),
            // Hybrid falls through to upstream for a gem it does not host;
            // local has nowhere else to look, so the error stands.
            Err(e) if mode == RegistryMode::Local => return Err(AppError::from(e)),
            // **`NotFound` alone.** This read `Err(_) => {}`, which fell through
            // on *every* error — including the `AccessDenied` that
            // `load_visible_versions_or_not_found` returns for a gem whose every
            // version is administratively blocked, and the `NotFoundWithheld`
            // RFC 0017 added for one filtered away by grants. Both name a gem
            // this instance hosts, so falling through answers with rubygems.org's
            // gem of the same name: the dependency-confusion substitution those
            // two errors exist to prevent, served by the one arm that did not
            // look at which error it had.
            Err(CoreError::NotFound(_)) => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }

    let req = batlehub_core::services::ProxyRequest {
        package_id: PackageId::new(&registry, &gem, "__compact__"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let doc = svc
        .version_document(&req, batlehub_core::ports::DocumentKind::COMPACT_INFO, "")
        .await
        .map_err(AppError::from)?;
    Ok(text_response(&http_req, doc))
}
