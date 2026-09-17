//! The Nix binary-cache (substituter) protocol — `nix`, `nix-serve`-shaped
//! tooling, `cachix`-style `substituters` entries (RFC 0028).
//!
//! Seven read routes, and the whole design is in what happens to **one line**
//! of one of them.
//!
//! A narinfo is the document every substitution resolves through: Nix asks for
//! `{storeHash}.narinfo` before it asks for anything else, always
//! (`BinaryCacheStore::narInfoFileFor`). It carries a `Sig:` that covers
//! `1;{StorePath};{NarHash};{NarSize};{References}` — and **not** `URL:`. So
//! this proxy rewrites `URL:` to `nar/{storeHash}/{basename}`, which gives the
//! NAR request a coordinate to be blocked, cached and counted under, and
//! relays every other byte including the signature. The client then verifies
//! exactly what it would have verified against the upstream, with the
//! upstream's own key. Every other registry kind in this tree has had to choose
//! between rewriting and provenance; this one gets both, and
//! `rewriting_the_url_does_not_change_the_fingerprint` in
//! `batlehub_core::services::nix` is what holds that to the wire.
//!
//! **The block lands on the narinfo**, not on the NAR, because a `404` there is
//! the protocol's own "this cache does not have it": `queryPathInfoUncached`
//! records a negative result for `narinfo-cache-negative-ttl` and Nix moves to
//! the next substituter, or builds. The NAR route re-derives the coordinate
//! from the store hash in its own path and asks again, so a client holding a
//! narinfo from *before* the block is refused there too — never a `403`
//! mid-transfer (RFC 0028 §5.3).
//!
//! **The honest limit, stated here because the registry page states it too:**
//! blocking a binary does not block the software. A machine that has the
//! derivation — every NixOS machine does — builds `hello-1.0.0.2` from source
//! when no cache serves it. The lever for that is `max-jobs = 0` in the
//! client's own `nix.conf`, and it is not this proxy's.
//!
//! Every path segment reaches a storage key, so each is validated here for a
//! clean `400`; `validate_coordinate` in `ProxyService::handle` and
//! `ensure_safe_key` in the storage backends remain the deeper guards.

use std::sync::Arc;

use actix_web::{get, put, route, web, HttpRequest, HttpResponse, Responder};
use batlehub_config::schema::RegistryMode;

use batlehub_adapters::registry::nix::{lookup_nar_url, nar_artifact, remember_nar_url};
use batlehub_core::{
    entities::{Action, PackageId, RegistryKind},
    ports::{DocumentKind, VersionDocument},
    services::{
        nix::{self, NarInfo},
        validate_path_safe, LocalRegistryService, ProxyService,
    },
};

use super::common::{
    collect_payload, document_response, fetch_proxy_document, proxy_document, proxy_stream,
    registry_public_base, require_local_mode, require_registry_type,
};
use crate::handlers::schemas::{ArtifactBytes, MessageResponse, ProtocolDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};

/// The package a registry-wide document is addressed under.
///
/// `nix-cache-info` describes the *cache*, not a path: there is no coordinate
/// in it and no version. A fixed literal keeps it one cache entry per registry
/// and keeps it out of the way of any real store name — no store name is
/// `nix-cache-info`, because a store name is always preceded by a 32-character
/// hash in every address the protocol uses.
const CACHE_INFO_PACKAGE: &str = "nix-cache-info";

/// How long an entry in the upstream-NAR-URL index lives.
///
/// The index is a derived fact of a narinfo and is meaningless once that
/// narinfo has expired, so it takes the negative TTL the proxy already has
/// rather than introducing a second expiry rule. A lost entry is a `404` on the
/// upstream-shape route, which makes Nix refetch the narinfo and succeed — the
/// protocol's own recoverable answer.
const NAR_INDEX_TTL: std::time::Duration = std::time::Duration::from_secs(
    batlehub_core::services::hot_config::DEFAULT_UPSTREAM_NEGATIVE_TTL_SECS,
);

/// `nix-cache-info` — the three lines a client reads once per substituter.
///
/// Relayed, not composed. `StoreDir` is what decides whether the cache is
/// usable at all (a mismatch is a hard client-side error naming both prefixes),
/// and `Priority` is the one value an operator would change — which they do on
/// their own `substituters` line with `?priority=`, not here. A second knob for
/// the same number in a second place is what decision 5 rejected.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/nix-cache-info",
    tag = "proxy/nix",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The cache's StoreDir, WantMassQuery and Priority", body = ProtocolDocument, content_type = "text/x-nix-cache-info"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nix/nix-cache-info")]
pub async fn nix_cache_info(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    proxy_document(
        svc,
        PackageId::new(&registry, CACHE_INFO_PACKAGE, "-"),
        identity,
        Action::ReleasesList,
        DocumentKind::CACHE_INFO,
        String::new(),
    )
    .await
}

/// One store path's metadata — the chokepoint every substitution goes through.
///
/// `HEAD` is answered from the same code path with no body, because `nix copy`
/// asks before it uploads (`fileExists`) and a `HEAD` that 405'd would make
/// every publish re-upload a NAR the cache already has.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/{hash}.narinfo",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("hash" = String, Path, description = "The 32-character Nix base32 store hash"),
    ),
    responses(
        (status = 200, description = "The narinfo, with only `URL:` rewritten and every `Sig:` relayed byte-exact", body = ProtocolDocument, content_type = "text/x-nix-narinfo"),
        (status = 400, description = "Not a Nix base32 store hash"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not in this cache — because upstream does not have it, or because the coordinate is blocked"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/nix/{hash}.narinfo",
    method = "GET",
    method = "HEAD"
)]
pub async fn nix_narinfo(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, hash) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    require_store_hash(&hash)?;

    // **Local first.** A store path published here is served from its stored
    // narinfo — signatures and all — and never recomposed: the document carries
    // this registry's signature, and re-deriving it would mean re-signing,
    // which produces different bytes every time the key rotates for a path that
    // never changed. A `local` registry stops here; a `hybrid` one falls
    // through to the upstream for a hash it does not hold, which is the same
    // ladder every other hybrid kind uses.
    if mode_map.get(&registry) != RegistryMode::Proxy {
        if let Some(held) = local_svc
            .get_nix_narinfo(&registry, &hash)
            .await
            .map_err(AppError::from)?
        {
            let is_head = req.method() == actix_web::http::Method::HEAD;
            let response = document_response(VersionDocument::text("text/x-nix-narinfo", held));
            return Ok(if is_head {
                response.drop_body().map_into_boxed_body()
            } else {
                response
            });
        }
        if mode_map.get(&registry) == RegistryMode::Local {
            return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
                format!("{hash}.narinfo is not in this cache"),
            )));
        }
    }

    let public_base = registry_public_base(&req, &registry);
    let doc = fetch_proxy_document(
        svc.clone(),
        // The *document* is addressed by store hash: the coordinate it resolves
        // to is only knowable by reading it, which is the chicken-and-egg this
        // protocol has and npm does not.
        PackageId::new(&registry, &hash, "-"),
        identity,
        Action::ReleasesList,
        DocumentKind::NARINFO,
        public_base,
    )
    .await?;

    let served = relay_narinfo(&svc, &registry, &hash, doc).await?;
    let is_head = req.method() == actix_web::http::Method::HEAD;
    let response = document_response(served);
    Ok(if is_head {
        // Same headers, no body — `fileExists` reads the status and nothing
        // else, and streaming a body it discards costs a narinfo per published
        // path on every `nix copy`.
        response.drop_body().map_into_boxed_body()
    } else {
        response
    })
}

/// Parse, enforce, rewrite and index one narinfo.
///
/// Separated from the route because it is the whole protocol-specific half and
/// it is what the tests want to drive directly.
async fn relay_narinfo(
    svc: &Arc<ProxyService>,
    registry: &str,
    hash: &str,
    doc: VersionDocument,
) -> Result<VersionDocument, AppError> {
    let text = doc.body.as_text().ok_or_else(|| {
        AppError::from(batlehub_core::error::CoreError::Registry(
            "a narinfo arrived as JSON; the protocol is `Name: value` lines".to_owned(),
        ))
    })?;
    let mut info = NarInfo::parse(text).map_err(AppError::from)?;
    info.require_fields().map_err(AppError::from)?;
    let path = info.store_path().map_err(AppError::from)?;

    // The document must be about the path that was asked for. A cache that
    // answered `{a}.narinfo` with `{b}`'s document would otherwise get `b`'s
    // bytes cached under `a`'s coordinate — and `a` may be blocked while `b` is
    // not. Upstream would have to be hostile or broken for this to fire, which
    // is exactly when it matters.
    if path.hash != hash {
        return Err(AppError::from(batlehub_core::error::CoreError::Registry(
            format!(
                "upstream answered {hash}.narinfo with a document for store path {} — refusing \
                 to serve it under a coordinate it does not name",
                path.to_full_path()
            ),
        )));
    }

    // The block, on the coordinate Nix itself parses out of the store name.
    // A `404` rather than a `403`: to Nix this is "not in this cache", and it
    // consults the next substituter or builds, saying so in its own words.
    let blocked = svc
        .blocked_versions_for(registry, &path.package, RegistryKind::Nix)
        .await;
    if blocked.contains(&path.version) {
        tracing::debug!(
            registry = %registry,
            package = %path.package,
            version = %path.version,
            store_hash = %path.hash,
            "refusing a blocked store path at its narinfo"
        );
        return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
            format!("{hash}.narinfo is not in this cache"),
        )));
    }

    // `require_upstream_sigs`, which is about *non*-CA paths only: a
    // content-addressed path legitimately carries none, and Nix's own
    // `isContentAddressed` short-circuits `checkSignatures` for it.
    if svc.nix_requires_upstream_sigs(registry).await
        && info.get_all("Sig").is_empty()
        && info.get("CA").is_none()
    {
        tracing::warn!(
            registry = %registry,
            store_path = %path.to_full_path(),
            "refusing to relay an unsigned narinfo: require_upstream_sigs is on and the path is \
             not content-addressed"
        );
        return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
            format!("{hash}.narinfo is not in this cache"),
        )));
    }

    // The one rewritten line. Everything else — order, spelling, unknown
    // fields, every `Sig:` — is upstream's.
    let basename = nix::rewrite_url(&mut info, &path.hash).map_err(AppError::from)?;

    // So a client still holding upstream's `nar/{fileHash}.nar.zst` (30-day
    // positive TTL) resolves to a coordinate rather than being refused forever.
    remember_nar_url(
        svc.cache.as_ref(),
        registry,
        &basename,
        PackageId::new(registry, &path.package, &path.version)
            .with_artifact(nar_artifact(&path.hash, &basename)),
        NAR_INDEX_TTL,
    )
    .await;

    Ok(VersionDocument {
        content_type: "text/x-nix-narinfo".to_owned(),
        body: batlehub_core::ports::DocumentBody::Text(info.to_wire()),
        synthesised: doc.synthesised,
    })
}

/// The NAR, under the coordinate the rewritten `URL:` gave it.
///
/// **Answers `HEAD`, and that is not a nicety.** `nix copy --to` guards its
/// NAR upload with `if (repair || !fileExists(narInfo->url))`, and
/// `HttpBinaryCacheStore::fileExists` maps only `NotFound` and `Forbidden` to
/// "absent" — every other status calls `disable(e)`, which takes the whole
/// cache out of the client's substituter list for 60 seconds and fails the
/// copy. A `405` from a GET-only route is one of those, so a publish would die
/// on its first NAR (RFC 0028 §13).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/nar/{hash}/{file}",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("hash" = String, Path, description = "The 32-character Nix base32 store hash"),
        ("file" = String, Path, description = "The NAR file name, as upstream names it (e.g. 075lhsj….nar.zst)"),
    ),
    responses(
        (status = 200, description = "The NAR, streamed and cached under the coordinate", body = ArtifactBytes, content_type = "application/x-nix-nar"),
        (status = 400, description = "Not a Nix base32 store hash, or an unsafe file name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not in this cache, or the coordinate is blocked"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/nix/nar/{hash}/{file}",
    method = "GET",
    method = "HEAD"
)]
pub async fn nix_nar(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, hash, file) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    require_store_hash(&hash)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    // The coordinate is re-derived from the *hash* rather than trusted from the
    // URL, and that is what makes a client holding a pre-block narinfo get a
    // `404` here instead of the bytes (RFC 0028 §5.3). The narinfo read is a
    // document-cache hit on every request that followed one, which is every
    // request a real client makes.
    let path = coordinate_for(
        &svc, &local_svc, &mode_map, &registry, &hash, &identity, &req,
    )
    .await?;
    let pkg = PackageId::new(&registry, &path.package, &path.version)
        .with_artifact(nar_artifact(&hash, &file));

    // A published NAR lives under a four-level key, because a version holds
    // many store paths — so it is read through `get_artifact_at_key`, the same
    // hook Maven and Terraform use, rather than by touching storage directly.
    // Going through it is what keeps the visibility gate on the read.
    if mode_map.get(&registry) != RegistryMode::Proxy {
        let key = batlehub_core::services::nix_nar_storage_key(
            &registry,
            &path.package,
            &path.version,
            &hash,
            &file,
        );
        if let Some(bytes) = local_svc
            .get_artifact_at_key(&pkg, &key, Action::ReleasesRead, &identity.0, &identity.1)
            .await
            .map_err(AppError::from)?
        {
            return Ok(HttpResponse::Ok()
                .content_type(batlehub_adapters::registry::nix::nar_content_type())
                .body(bytes));
        }
        if mode_map.get(&registry) == RegistryMode::Local {
            return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
                format!("nar/{hash}/{file} is not in this cache"),
            )));
        }
    }

    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(batlehub_adapters::registry::nix::nar_content_type()),
    )
    .await
}

/// A blocked coordinate is **absent**, not forbidden — on every route that
/// serves a store path, not only on the narinfo.
///
/// `proxy_stream` refuses a blocked version with a `403`, which is the right
/// answer for a caller who may not read and the wrong one for a path this
/// cache is declining to have. Nix's `HttpBinaryCacheStore` maps both to
/// "absent" for `fileExists`, so the client behaves the same either way — but
/// a `403` says *you* may not have this, and the block says *nobody* gets it
/// here. Only the `404` is the protocol's own "this cache does not have it",
/// and it is what this kind's page, this module's header and RFC 0028 §5.3 all
/// promise on `{hash}.narinfo`, `{hash}.ls` and every `nar/` request alike.
async fn refuse_blocked_path(
    svc: &ProxyService,
    registry: &str,
    package: &str,
    version: &str,
    what: &str,
) -> Result<(), AppError> {
    let blocked = svc
        .blocked_versions_for(registry, package, RegistryKind::Nix)
        .await;
    if blocked.contains(version) {
        tracing::debug!(
            registry = %registry,
            package = %package,
            version = %version,
            route = %what,
            "refusing a blocked store path"
        );
        return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
            format!("{what} is not in this cache"),
        )));
    }
    Ok(())
}

/// The coordinate one store hash names, read from its own narinfo.
///
/// The chicken-and-egg this protocol has and npm does not: a request carries a
/// hash, and the package and version it belongs to are only knowable by reading
/// the document. Going through [`fetch_proxy_document`] rather than the client
/// directly is what puts that read behind the metadata cache, the rule chain
/// and the audit trail — so the second request for a path costs nothing, and
/// the first is charged to whoever made it.
///
/// **Two gates, in this order, and it is the right order.** This read is
/// authorized as `releases:list` — the narinfo *is* this kind's listing
/// document — and the bytes are then authorized as `releases:read` inside
/// `proxy_stream`. So a caller holding neither is refused here, before anything
/// is dialled, which is the grants-before-fetch property `config.authz.toml`'s
/// `example.invalid` rows exist to pin. A caller holding `releases:list` and
/// not `releases:read` does cost one upstream narinfo fetch before being
/// refused at the bytes — but that is a listing they are entitled to, and
/// refusing to read it would make the listing verb mean less than it says.
async fn coordinate_for(
    svc: &web::Data<Arc<ProxyService>>,
    local_svc: &web::Data<Arc<LocalRegistryService>>,
    mode_map: &RegistryModeMap,
    registry: &str,
    hash: &str,
    identity: &AuthIdentity,
    req: &HttpRequest,
) -> Result<nix::StorePath, AppError> {
    // A locally published path knows its own coordinate, and in `local` mode
    // there is no upstream to ask — so this is not an optimisation, it is the
    // only thing that works there.
    if mode_map.get(registry) != RegistryMode::Proxy {
        if let Some(held) = local_svc
            .get_nix_narinfo(registry, hash)
            .await
            .map_err(AppError::from)?
        {
            let path = NarInfo::parse(&held)
                .and_then(|i| i.store_path())
                .map_err(AppError::from)?;
            refuse_blocked_path(svc, registry, &path.package, &path.version, hash).await?;
            return Ok(path);
        }
        if mode_map.get(registry) == RegistryMode::Local {
            return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
                format!("{hash} is not in this cache"),
            )));
        }
    }
    let doc = fetch_proxy_document(
        svc.clone(),
        PackageId::new(registry, hash, "-"),
        identity.clone(),
        Action::ReleasesList,
        DocumentKind::NARINFO,
        registry_public_base(req, registry),
    )
    .await?;
    let text = doc.body.as_text().ok_or_else(|| {
        AppError::from(batlehub_core::error::CoreError::Registry(
            "a narinfo arrived as JSON; the protocol is `Name: value` lines".to_owned(),
        ))
    })?;
    let path = NarInfo::parse(text)
        .and_then(|i| i.store_path())
        .map_err(AppError::from)?;
    // Same guard as the narinfo route, for the same reason: a document that
    // names another path would put its bytes under this coordinate, and the
    // two may not be blocked alike.
    if path.hash != hash {
        return Err(AppError::from(batlehub_core::error::CoreError::Registry(
            format!(
                "upstream answered {hash}.narinfo with a document for store path {}",
                path.to_full_path()
            ),
        )));
    }
    // The block, on the coordinate the document just named — before any byte is
    // authorized, so the answer is the narinfo's own `404` rather than
    // `proxy_stream`'s `403`.
    refuse_blocked_path(svc, registry, &path.package, &path.version, hash).await?;
    Ok(path)
}

/// A NAR asked for in the **upstream's** own shape, resolved through the reverse index.
///
/// Two callers reach this route, and the second is the reason it answers
/// `HEAD`:
///
/// - a client whose cached narinfo predates this registry and still holds
///   upstream's `nar/{fileHash}.nar.zst` (30-day positive TTL);
/// - **`nix copy --to`**, which asks `HEAD nar/{fileHash}.nar.{ext}` before
///   uploading, because that is the path `narInfo->url` names. `fileExists`
///   treats anything but `404`/`403` as a hard error and disables the cache
///   for 60 seconds, so a GET-only route would fail every publish.
///
/// Nix keeps a positive narinfo entry for 30 days, so for a month after a
/// migration these are real requests. Resolved through the reverse index a
/// served narinfo wrote; a miss is a `404`, which makes Nix drop its cached
/// narinfo, refetch it, get the rewritten `URL:` and succeed. One extra round
/// trip, once per path — and never a NAR served outside a coordinate, which is
/// the property the whole rewrite exists for.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/nar/{file}",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "The upstream NAR file name"),
    ),
    responses(
        (status = 200, description = "The NAR, resolved through the reverse index and served under its coordinate", body = ArtifactBytes, content_type = "application/x-nix-nar"),
        (status = 400, description = "Unsafe file name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "No narinfo served by this instance names this NAR (refetch the narinfo), or the coordinate is blocked"),
    ),
    security(("bearer_token" = [])),
)]
#[route("/proxy/{registry}/nix/nar/{file}", method = "GET", method = "HEAD")]
pub async fn nix_nar_upstream_shape(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    validate_path_safe("file", &file).map_err(AppError::from)?;

    let Some(pkg) = lookup_nar_url(svc.cache.as_ref(), &registry, &file).await else {
        return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
            format!(
                "nar/{file} is not in this cache under a coordinate this instance knows; refetch \
                 the narinfo that names it"
            ),
        )));
    };

    // The reverse index is a second spelling of the same coordinate, so it is a
    // second way to the same bytes: without this, a client whose cached narinfo
    // predates the registry would be refused with a `403` where every other
    // route says `404`, and the block would read differently depending on which
    // URL shape the client happened to hold.
    refuse_blocked_path(
        &svc,
        &registry,
        &pkg.name,
        &pkg.version,
        &format!("nar/{file}"),
    )
    .await?;

    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(batlehub_adapters::registry::nix::nar_content_type()),
    )
    .await
}

/// `{hash}.ls` — a JSON listing of a NAR's contents, for `nix store ls` alone.
///
/// Never read by an install, which is why it is a read rather than a listing:
/// nothing resolves through it. Answers `HEAD` for the same reason the NAR
/// routes do — a client that probes before reading must get a status it
/// understands.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/{hash}.ls",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("hash" = String, Path, description = "The 32-character Nix base32 store hash"),
    ),
    responses(
        (status = 200, description = "The NAR listing", body = ArtifactBytes, content_type = "application/json"),
        (status = 400, description = "Not a Nix base32 store hash"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not in this cache, or the coordinate is blocked"),
    ),
    security(("bearer_token" = [])),
)]
#[route("/proxy/{registry}/nix/{hash}.ls", method = "GET", method = "HEAD")]
pub async fn nix_ls(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, hash) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    require_store_hash(&hash)?;

    let path = coordinate_for(
        &svc, &local_svc, &mode_map, &registry, &hash, &identity, &req,
    )
    .await?;
    let pkg =
        PackageId::new(&registry, &path.package, &path.version).with_artifact(format!("{hash}.ls"));

    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some("application/json"),
    )
    .await
}

/// `realisations/{id}.doi` — the derivation-to-output mapping of a content-addressed derivation.
///
/// **The id is a tail, because the protocol has two spellings.** Nix 2.24
/// builds `realisations/{drvHash}!{output}.doi` — one segment — while master's
/// `makeRealisationPath` builds `realisations/{drvPath}/{outputName}.doi` —
/// two. A single-segment pattern serves one client generation and `404`s the
/// other, so this matches either and validates the whole tail
/// (RFC 0028 §13).
///
/// A passthrough document: the policy lives on the narinfo of the `outPath` it
/// names, not here. Blocking it as well would refuse the *mapping* while the
/// path it maps to is still served, which is the wrong half.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/realisations/{id}.doi",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "The realisation id, `sha256:{drvHash}!{output}`"),
    ),
    responses(
        (status = 200, description = "The realisation document", body = ProtocolDocument, content_type = "application/json"),
        (status = 400, description = "Unsafe realisation id"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not in this cache"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nix/realisations/{id:.*}.doi")]
pub async fn nix_realisation(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, id) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    validate_path_safe("realisation id", &id).map_err(AppError::from)?;
    proxy_document(
        svc,
        PackageId::new(&registry, &id, "-"),
        identity,
        Action::ReleasesRead,
        DocumentKind::REALISATION,
        String::new(),
    )
    .await
}

/// `log/{drv}` — a build log, read by `nix log`.
///
/// A passthrough with no coordinate in it for a policy to act on, which is why
/// §3 makes it a non-goal to treat as anything else.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/log/{drv}",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("drv" = String, Path, description = "The derivation file name"),
    ),
    responses(
        (status = 200, description = "The build log", body = ProtocolDocument, content_type = "text/plain"),
        (status = 400, description = "Unsafe derivation name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not in this cache"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nix/log/{drv}")]
pub async fn nix_build_log(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, drv) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    validate_path_safe("derivation", &drv).map_err(AppError::from)?;
    proxy_document(
        svc,
        PackageId::new(&registry, &drv, "-"),
        identity,
        Action::ReleasesRead,
        DocumentKind::BUILD_LOG,
        String::new(),
    )
    .await
}

// ── the publish half (RFC 0028 §4.4, phase 4) ────────────────────────────────

/// `PUT nar/{file}` — the NAR, arriving before anything knows what it is.
///
/// This is the first byte of a `nix copy --to` and it names **no coordinate**:
/// the client uploads `nar/{fileHash}.nar.{ext}` and only later sends the
/// narinfo that says which store path those bytes are. So the upload is parked
/// under a pending key owned by this publisher, and
/// [`nix_put_narinfo`] is what claims it.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/nix/nar/{file}",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("file" = String, Path, description = "The NAR file name the client chose, `{fileHash}.nar.{ext}`"),
    ),
    responses(
        (status = 200, description = "The NAR is held, unclaimed, until its narinfo arrives", body = MessageResponse),
        (status = 400, description = "Unsafe file name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or not a local/hybrid registry"),
        (status = 413, description = "Larger than limits.max_artifact_size_bytes"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/nix/nar/{file}")]
pub async fn nix_put_nar(
    path: web::Path<(String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    require_local_mode(&registry, &mode_map)?;
    validate_path_safe("NAR file name", &file).map_err(AppError::from)?;

    let bytes = collect_payload(payload).await?;
    local_svc
        .publish_nix_nar(&registry, &file, bytes, &identity.0)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "message": format!("nar/{file} is held until its narinfo claims it")
    })))
}

/// `PUT {hash}.narinfo` — the document that names the coordinate, and where this registry signs.
///
/// Everything happens here because this is the first moment anything is known:
/// the NAR is verified against the claims, the coordinate is parsed out of
/// `StorePath:`, `URL:` is rewritten to this instance's layout, any `Sig:`
/// forging *our* key name is dropped, the publisher's own signatures are kept,
/// and ours is appended over a fingerprint computed from bytes we hold.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/nix/{hash}.narinfo",
    tag = "proxy/nix",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("hash" = String, Path, description = "The 32-character Nix base32 store hash"),
    ),
    responses(
        (status = 200, description = "The store path is published and signed", body = MessageResponse),
        (status = 400, description = "Not a store hash, an unverifiable codec, or a field the bytes disagree with"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or not a local/hybrid registry"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/nix/{hash}.narinfo")]
pub async fn nix_put_narinfo(
    path: web::Path<(String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, hash) = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    require_local_mode(&registry, &mode_map)?;
    require_store_hash(&hash)?;

    let body = collect_payload(payload).await?;
    let text = String::from_utf8(body.to_vec()).map_err(|_| {
        AppError::bad_request(
            "a narinfo is `Name: value` lines in UTF-8. If `narinfo-compression` is set on the \
             store URL the body arrives compressed, which this server does not decode — unset it"
                .to_owned(),
        )
    })?;
    let info = NarInfo::parse(&text).map_err(AppError::from)?;
    info.require_fields().map_err(AppError::from)?;

    // The NAR this document claims, read back from where `nix_put_nar` parked
    // it, and checked against every claim before anything is signed.
    let file = info
        .get("URL")
        .map(|u| u.rsplit('/').next().unwrap_or(u).to_owned())
        .ok_or_else(|| AppError::bad_request("corrupt NAR info file: missing 'URL'".to_owned()))?;
    let bytes = local_svc
        .take_pending_nix_nar(&registry, &file, &identity.0)
        .await
        .map_err(AppError::from)?;
    let facts =
        batlehub_adapters::registry::nix::check_nar(&bytes, &info).map_err(AppError::from)?;

    let key = svc.nix_signing_key(&registry).await;
    let published = local_svc
        .publish_nix_narinfo(batlehub_core::services::NixPublishRequest {
            registry: &registry,
            store_hash: &hash,
            info,
            bytes,
            facts: &facts,
            publisher: &identity.0,
            signing_key: key.as_deref(),
        })
        .await
        .map_err(AppError::from)?;

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "message": format!(
            "published {}@{} ({})",
            published.package, published.version, published.artifact
        )
    })))
}

/// `GET public-key` — the line an operator pastes into `trusted-public-keys`.
///
/// Nix reads no such endpoint; the console snippet and provisioning scripts do,
/// which is exactly what RFC 0020 serves as an Open VSX asset for the same
/// reason. A registry that signs nothing answers `404` rather than an empty
/// body, because "no key" and "a key I could not read" must not look alike.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nix/public-key",
    tag = "proxy/nix",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "One `name:base64` line for trusted-public-keys", body = ProtocolDocument, content_type = "text/plain"),
        (status = 404, description = "Unknown registry, or this registry signs nothing"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nix/public-key")]
pub async fn nix_public_key(
    path: web::Path<String>,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "nix", &map)?;
    let Some(key) = svc.nix_signing_key(&registry).await else {
        return Err(AppError::from(batlehub_core::error::CoreError::NotFound(
            format!(
                "registry '{registry}' has no [registries.nix_signing] key, so it signs nothing \
                 and there is no public key to publish"
            ),
        )));
    };
    Ok(HttpResponse::Ok()
        .content_type("text/plain; charset=utf-8")
        .body(format!("{}\n", key.public_key_line())))
}

/// A `400` naming Nix's own rule, rather than a `404` from three layers down.
///
/// The Nix32 alphabet has no `/`, no `.` and no whitespace, so a hash that
/// passes this cannot be a traversal — but the storage backends'
/// `ensure_safe_key` stays the deeper guard regardless.
fn require_store_hash(hash: &str) -> Result<(), AppError> {
    if nix::is_store_hash(hash) {
        return Ok(());
    }
    Err(AppError::bad_request(format!(
        "'{hash}' is not a store hash: Nix addresses a path by 32 characters of its own base32 \
         alphabet ({}), which omits e, o, u and t",
        String::from_utf8_lossy(nix::NIX32_ALPHABET)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_outside_the_alphabet_is_a_400_naming_nixs_rule() {
        let err = require_store_hash("eeee1npbf2n4z3pjy6vm2mw8ywkqixxs").expect_err("refused");
        let msg = format!("{err}");
        assert!(msg.contains("base32"), "{msg}");
        assert!(msg.contains("omits e, o, u and t"), "{msg}");
    }

    #[test]
    fn a_real_store_hash_passes() {
        assert!(require_store_hash("0001npbf2n4z3pjy6vm2mw8ywkqixxs6").is_ok());
    }

    #[test]
    fn the_registry_wide_package_cannot_collide_with_a_store_name() {
        // Every address in the protocol puts a 32-character hash before a store
        // name, so no request can produce this package string by accident.
        assert!(nix::StorePath::parse(CACHE_INFO_PACKAGE).is_err());
    }
}
