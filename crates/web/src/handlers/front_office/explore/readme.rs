//! `GET /api/v1/explore/packages/{registry}/{name}/readme` (RFC 0007 §4.2).
//!
//! Separate from the detail response on purpose: the README is fetched by the
//! panel rather than embedded, so the catalogue cache's TTL never holds a stale
//! document and the detail payload does not grow by a megabyte per package
//! (§5.4).

use super::{
    get, newest_first, web, AdminService, AppError, Arc, AuthIdentity, Deserialize, IntoParams,
    Responder, Serialize, ToSchema,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{PackageReadme, PackageStatus, ReadmeSource, RegistryKind},
    services::{
        proxy::Freshness,
        readme::{render::RenderOptions, ReadmeAnswer},
        LocalRegistryService, ProxyService,
    },
};

use crate::{RegistryMap, RegistryModeMap};

/// Nothing is stored for this package, but the registry type could carry one.
///
/// The panel says *"the README arrives when this version is first downloaded"*
/// for the archive-borne kinds, and nothing more dramatic: it is a limit, not a
/// failure.
pub const README_NONE_STORED: &str = "readme.none-stored";

/// This registry type has no README to give, ever.
///
/// Rendered as a statement rather than an error. `RegistryKind::readme_support`
/// carries the reason, and this endpoint quotes it — so the published support
/// table, the config warning and this message cannot disagree.
pub const README_UNSUPPORTED_TYPE: &str = "readme.unsupported-type";

/// The version exists and an administrator has blocked it.
pub const README_BLOCKED: &str = "readme.blocked";

#[derive(Deserialize, IntoParams)]
pub struct ReadmePath {
    pub registry: String,
    pub name: String,
}

/// What to return: the rendered HTML, the source, or both.
///
/// `source` exists for the CLI, which prints markdown to a terminal — and for
/// an operator checking what the package actually said against what the panel
/// rendered, which is the whole reason the store keeps the source (§5.3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReadmeFormatParam {
    #[default]
    Html,
    Source,
    Both,
}

#[derive(Deserialize, IntoParams)]
pub struct ReadmeQuery {
    /// The version to show. Absent means the newest version that has one.
    #[serde(default)]
    pub version: Option<String>,
    /// Inlined into the parameter rather than referenced: nothing returns this
    /// enum in a body, so utoipa emits no component for it and the generated
    /// TypeScript client would carry a dangling `$ref` — which reaches the
    /// console as `unknown`, the exact outcome `openapi_contract.rs` exists to
    /// prevent for response bodies.
    #[serde(default)]
    #[param(inline)]
    pub format: ReadmeFormatParam,
    /// Whether the derived-from-upstream fallback may run.
    ///
    /// The same key, the same values and the same meaning as the detail
    /// endpoint's — a caller who asks one for a local-only answer means it of
    /// both, and the CLI's `--no-upstream` sends it to whichever endpoint the
    /// subcommand happens to call. `serde_urlencoded` drops unknown keys
    /// silently, so leaving this undeclared did not reject `upstream=skip`; it
    /// accepted it, ignored it, and went to the network anyway.
    #[serde(default)]
    #[param(inline)]
    pub upstream: super::detail::UpstreamMode,
}

#[derive(Serialize, ToSchema)]
pub struct ReadmeResponse {
    pub registry: String,
    pub name: String,
    /// The coordinate the returned text belongs to.
    pub version: String,
    /// What the caller asked for, when they named a version.
    pub requested_version: Option<String>,
    /// `version != requested_version` — the panel labels it.
    pub is_fallback: bool,
    /// `markdown` | `html` | `rst` | `plain` — what the source *is*.
    pub format: String,
    /// `upstream-metadata` | `archive` | `local-publish` — where it came from.
    pub source: String,
    /// The text is the package's, not this version's: npm's document-root
    /// `readme`, attributed to the version `dist-tags.latest` names. The panel
    /// says so rather than presenting a package-level document as this
    /// version's.
    pub package_level: bool,
    /// `true` for a durable record. `false` when it was derived from a cached
    /// upstream document for a version this instance holds no bytes for.
    pub stored: bool,
    /// `cached` | `fresh` | `stale`. Only meaningful when `stored` is `false`.
    pub freshness: Option<String>,
    /// The source hit the registry's `max_bytes` and what is shown is a prefix.
    pub truncated: bool,
    /// Sanitised HTML, present unless `format=source`.
    pub rendered_html: Option<String>,
    /// The stored source, present unless `format=html`.
    pub source_text: Option<String>,
    /// When *this instance* read the text, not when upstream published it.
    pub extracted_at: String,
}

/// A version's README, rendered and sanitised.
#[utoipa::path(
    get,
    path = "/api/v1/explore/packages/{registry}/{name}/readme",
    tag = "explore",
    params(ReadmePath, ReadmeQuery),
    responses(
        (status = 200, description = "The README for this version", body = ReadmeResponse),
        (status = 403, description = "The version is blocked; the body carries the reason"),
        (status = 404, description = "No README, or the registry is not browsable by this caller, \
                                     or the package is not visible to them"),
    ),
    security(("bearer_token" = [])),
)]
// Nine extractors, one per thing this endpoint has to consult before it can
// answer: the coordinate, the query, the caller, what that caller may browse,
// the blocked set, the local backend, the README store, the registry's type and
// its mode. Actix injects each by type, so collapsing them into a bag would hide
// what the handler reads — and every one of them gates or shapes the answer.
#[allow(clippy::too_many_arguments)]
#[get("/api/v1/explore/packages/{registry}/{name}/readme")]
pub async fn explore_package_readme(
    req: actix_web::HttpRequest,
    path: web::Path<ReadmePath>,
    query: web::Query<ReadmeQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    registry_map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = &path.registry;
    let name = &path.name;
    let requested = query.version.clone();

    let Resolved { answer, derived } = resolve_readme(ResolveInput {
        registry,
        name,
        version: requested.as_deref(),
        upstream: query.upstream,
        identity: &identity,
        hot: &hot,
        admin_svc: &admin_svc,
        local_svc: &local_svc,
        proxy_svc: &proxy_svc,
        registry_map: &registry_map,
        mode_map: &mode_map,
    })
    .await?;

    // `resolve_readme` already refused a request this service cannot answer, so
    // by here there is one.
    let readme_svc = proxy_svc
        .readme
        .as_ref()
        .ok_or_else(|| not_found(registry, name))?;

    let opts = render_options(&req, &local_svc, registry, name, &answer.readme.version).await;
    let rendered_html = match query.format {
        ReadmeFormatParam::Source => None,
        _ => Some(readme_svc.render_cached(&answer.readme, &opts).await),
    };
    let source_text = match query.format {
        ReadmeFormatParam::Html => None,
        _ => Some(answer.readme.content.clone()),
    };

    Ok(web::Json(ReadmeResponse {
        registry: registry.clone(),
        name: name.clone(),
        version: answer.readme.version.clone(),
        requested_version: requested,
        is_fallback: answer.is_fallback,
        format: answer.readme.format.as_str().to_owned(),
        source: answer.readme.source.as_str().to_owned(),
        package_level: answer.readme.package_level,
        // A row this instance wrote, or a rendering derived from a cached
        // upstream document. The difference matters: a derived answer is bounded
        // by the metadata cache's TTL rather than being a durable record, and
        // nothing writes it to `package_readmes` — a row created because
        // somebody looked at a page would have nothing that ever deletes it.
        stored: derived.is_none(),
        freshness: derived.map(|f| freshness_word(f).to_owned()),
        truncated: answer.readme.truncated,
        rendered_html,
        source_text,
        extracted_at: answer.readme.extracted_at.to_rfc3339(),
    }))
}

/// Everything [`resolve_readme`] has to consult.
///
/// A struct rather than nine positional parameters: the two callers pass the
/// same nine things, and a positional list of that length is how a `registry`
/// ends up in the `name` slot.
pub(super) struct ResolveInput<'a> {
    pub registry: &'a str,
    pub name: &'a str,
    pub version: Option<&'a str>,
    /// `Skip` stops the derived-from-upstream fallback: a caller who asked for
    /// no egress gets the `404` this instance can answer on its own rather than
    /// an outbound resolve they did not sanction.
    pub upstream: super::detail::UpstreamMode,
    pub identity: &'a AuthIdentity,
    /// Read for one question: may this caller browse this registry at all.
    /// See `enforce_detail_gates` for why this is passed rather than taken
    /// from `local_svc.hot`.
    pub hot: &'a batlehub_core::services::hot_config::HotConfigLock,
    pub admin_svc: &'a AdminService,
    pub local_svc: &'a LocalRegistryService,
    pub proxy_svc: &'a ProxyService,
    pub registry_map: &'a RegistryMap,
    pub mode_map: &'a RegistryModeMap,
}

/// The README to show, and whether it was derived rather than stored.
pub(super) struct Resolved {
    pub answer: ReadmeAnswer,
    /// `Some` when the text came from a cached upstream document rather than
    /// from a row, carrying which rung answered.
    pub derived: Option<Freshness>,
}

/// Which README a coordinate resolves to, through every gate.
///
/// Shared by this endpoint and the image one, which is the point: an image is
/// *part of* a README, so it must be reachable exactly when the README is and
/// never otherwise. A second implementation of the visibility check, the block
/// check and the fallback rule would be a second set of answers to drift apart —
/// and the one that drifted would be the side channel (RFC 0007-bis §5.1).
pub(super) async fn resolve_readme(input: ResolveInput<'_>) -> Result<Resolved, AppError> {
    let ResolveInput {
        registry,
        name,
        version,
        upstream,
        identity,
        hot,
        admin_svc,
        local_svc,
        proxy_svc,
        registry_map,
        mode_map,
    } = input;

    // Gate: the catalogue's own registry list, checked first because it is the
    // widest of the three and the cheapest — one read lock, no I/O.
    //
    // `browsable_registries` is the set `list.rs` filters its rows by and
    // `stats.rs` counts over, so a registry it refuses to admit has no rows, no
    // counts and no page. (It resolves `catalogue:browse`, which RFC 0015 §10
    // rule 2 translated from the older `proxy access ∩ rbac.explore`
    // intersection — same answer, different owner. The `AccessConfig` method
    // that used to compute it has no caller left.) Serving its README anyway
    // made this endpoint the way round that refusal: the text names the
    // package, and usually its homepage, its dependencies and its build. That
    // is the same side channel `check_visibility` closes one level down, one
    // level up — a registry an operator took out of `rbac.explore` is not
    // browsable *by any door*.
    //
    // The proxy path is untouched and is meant to be: `rbac.explore.user =
    // false` is "this registry is for package managers, not for reading", and a
    // caller who can still `GET` an artifact through the proxy is exercising the
    // access they were granted. This gate is about the catalogue only.
    //
    // `404`, and the package's own `404` at that, for the reason the visibility
    // gate below gives: denied and absent look identical from outside.
    if !batlehub_core::services::authz::browsable_registries(hot, identity)
        .await
        .contains(registry)
    {
        tracing::debug!(
            registry = %registry, package = %name,
            "explore readme: registry not browsable by this caller"
        );
        return Err(not_found(registry, name));
    }

    // `404` rather than `403` on a visibility refusal, exactly as `detail.rs`
    // does and for the reason stated there: a `403` confirms the package
    // exists, which is the fact a non-public package is trying not to disclose.
    // A README must not be a side channel around the gate that hides its name.
    if let Err(e) = local_svc.check_visibility(registry, name, identity).await {
        tracing::debug!(
            registry = %registry, package = %name, error = %e,
            "explore readme: hidden by package visibility"
        );
        return Err(not_found(registry, name));
    }

    // A registry type with no README says so as a statement, before anything is
    // looked up: there is nothing to find, and "not found" would read as a gap
    // rather than as a decision.
    let kind = registry_map
        .type_of(registry)
        .and_then(|t| t.parse::<RegistryKind>().ok());
    if let Some(kind) = kind {
        if let batlehub_core::entities::ReadmeSupport::None(reason) = kind.readme_support() {
            return Err(
                AppError::not_found(format!("{kind} packages carry no README — {reason}"))
                    .coded(README_UNSUPPORTED_TYPE),
            );
        }
    }

    let Some(readme_svc) = proxy_svc.readme.as_ref() else {
        return Err(not_found(registry, name));
    };

    // Blocked and unlisted versions may not be substituted in as a fallback: a
    // blocked version serves no README at all, and silently showing its text
    // under another version's heading would route round the block (§4.4).
    let blocked = blocked_versions(admin_svc, registry, name).await;
    let unlisted = unlisted_versions(local_svc, registry, name).await;
    let ineligible: Vec<String> = blocked
        .iter()
        .map(|(v, _)| v.clone())
        .chain(unlisted.iter().cloned())
        .collect();

    // A blocked version is refused with its reason before anything is read —
    // the same answer the download path gives, so the operator sees that it
    // exists and why it is refused rather than being told it has no README.
    if let Some(version) = version {
        if let Some((_, reason)) = blocked.iter().find(|(v, _)| v == version) {
            return Err(AppError::forbidden(format!(
                "version '{version}' of '{name}' is blocked: {reason}"
            ))
            .coded(README_BLOCKED));
        }
    }

    let answer = match version {
        Some(version) => readme_svc
            .get_for_version(registry, name, version, &ineligible)
            .await
            .map_err(AppError::from)?,
        // No version named: the newest that has one, which is not a fallback
        // because nothing specific was asked for.
        None => readme_svc
            .repo
            .get_latest_with_readme(registry, name, &ineligible)
            .await
            .map_err(AppError::from)?
            .map(|readme| ReadmeAnswer {
                readme,
                is_fallback: false,
            }),
    };

    // A miss in the store is not the end of the path. For a version this
    // instance holds no bytes for there was never a row to find, so the answer
    // is *derived* from the cached upstream document — same renderer, same
    // digest key, same cache entry. The only difference the caller sees is
    // `stored: false` and a `freshness` (RFC 0007 §5.6).
    match answer {
        Some(answer) => Ok(Resolved {
            answer,
            derived: None,
        }),
        // …unless the caller said not to. `Skip` is the whole of `--no-upstream`:
        // the store had nothing, and asking upstream is exactly the request they
        // declined to make.
        None if upstream == super::detail::UpstreamMode::Skip => {
            Err(AppError::not_found(missing_message(kind, name)).coded(README_NONE_STORED))
        }
        None => match derived_readme(
            proxy_svc, local_svc, mode_map, registry, name, version, identity,
        )
        .await
        {
            Some((answer, freshness)) => Ok(Resolved {
                answer,
                derived: Some(freshness),
            }),
            None => Err(AppError::not_found(missing_message(kind, name)).coded(README_NONE_STORED)),
        },
    }
}

/// The same `404` a hidden package gets, so denied and absent look identical
/// from outside.
fn not_found(registry: &str, name: &str) -> AppError {
    AppError::not_found(format!(
        "package '{name}' not found in registry '{registry}'"
    ))
}

/// What to say when the registry type could carry a README and this package has
/// none stored.
///
/// The two cases read differently to an operator: an archive-borne kind has one
/// *inside bytes this instance does not hold*, which is a limit that resolves
/// itself the first time somebody downloads the version. A metadata-borne kind
/// with nothing stored genuinely has none.
fn missing_message(kind: Option<RegistryKind>, name: &str) -> String {
    match kind.map(|k| k.readme_support()) {
        Some(support) if support.reads_the_archive() && !support.answers_for_unheld_versions() => {
            format!(
                "no README stored for '{name}'. This registry type carries its README inside the \
                 artifact, so it arrives when a version is first downloaded through this proxy."
            )
        }
        _ => format!("no README stored for '{name}'"),
    }
}

/// The blocked versions of this package, with their reasons.
///
/// A lookup failure reads as "nothing blocked" rather than propagating, for the
/// same reason `license_for` does in `detail.rs`: a catalogue outage should not
/// turn the README panel into an error. The gate that matters is the download
/// path's, and it re-checks the concrete coordinate as it always has.
async fn blocked_versions(
    admin_svc: &AdminService,
    registry: &str,
    name: &str,
) -> Vec<(String, String)> {
    let filter = batlehub_core::entities::PackageFilter {
        registry: Some(registry.to_owned()),
        registries: vec![],
        name_exact: Some(name.to_owned()),
        name_contains: None,
        blocked_only: true,
        limit: 500,
        offset: 0,
        viewer: None,
    };
    admin_svc
        .list_packages(filter)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|summary| match summary.status {
            PackageStatus::Blocked { reason, .. } => Some((summary.package_id.version, reason)),
            PackageStatus::Available => None,
        })
        .collect()
}

/// Versions hidden from registry-protocol listings.
///
/// Still downloadable by exact coordinate, so they serve their *own* README —
/// they are only excluded from being substituted in as somebody else's.
async fn unlisted_versions(
    local_svc: &LocalRegistryService,
    registry: &str,
    name: &str,
) -> Vec<String> {
    local_svc
        .backend
        .get_versions(registry, name)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.unlisted)
        .map(|p| p.version)
        .collect()
}

/// The registry's image policy, read out of hot config, with the prefix a
/// proxied image's `src` is rewritten to.
///
/// Snapshotted out of the lock before the render, per the hot-reload convention.
/// An absent entry means enabled with the default policy — the README block's
/// absence means *on* (§4.1).
///
/// The prefix is **absolute**, built from the origin this very request arrived
/// on, because the sanitiser's `url_relative(Deny)` applies to the attribute
/// filter's output and would drop a relative one — leaving every image with no
/// `src` and nothing said about it. `trusted_origin` is what decides whether a
/// forwarded header may influence a generated URL, so the same rule that governs
/// every other URL this server emits governs this one.
///
/// The coordinate in the prefix is the **answering** README's, not the requested
/// version's: under the fallback rule the panel may be showing 1.4.2's text for
/// 2.0.0-rc1, and the images belong to the document actually rendered.
async fn render_options(
    req: &actix_web::HttpRequest,
    local_svc: &LocalRegistryService,
    registry: &str,
    name: &str,
    version: &str,
) -> RenderOptions {
    let cfg = local_svc
        .hot
        .read()
        .await
        .readme
        .get(registry)
        .cloned()
        .unwrap_or_default();
    RenderOptions {
        remote_images: cfg.remote_images,
        image_proxy_prefix: Some(image_prefix(req, registry, name, version)),
        // **The registry's own list, not an empty one.** `image_at` resolves an
        // incoming index with `image_urls(.., &cfg.allowed_hosts)`, and an empty
        // list here made the two walks disagree: the rendering numbered *every*
        // image, the resolution numbered only the allowed ones, so on a document
        // mixing allowed and disallowed hosts index N in the page and index N at
        // fetch time were different images — the page's first badge served the
        // bytes of the first allowed image instead. `image_urls`' own contract
        // says the list must be the one the rendering used; this is that.
        //
        // It also makes the setting do what it is documented to do at render
        // time: a disallowed host becomes the chip `strip` produces, rather than
        // an `<img>` pointing here that can only ever 404.
        image_hosts: cfg.remote_image_hosts.clone(),
    }
}

/// Where `…/readme-image/{n}` lives for one coordinate.
///
/// Every segment is percent-encoded: a scoped npm name carries a `/` and an `@`,
/// and an unencoded one would produce a URL pointing at a different route
/// entirely.
pub(super) fn image_prefix(
    req: &actix_web::HttpRequest,
    registry: &str,
    name: &str,
    version: &str,
) -> String {
    use batlehub_adapters::registry::percent_encode;
    let (scheme, host) = crate::middleware::proxy_trust::trusted_origin(req);
    format!(
        "{scheme}://{host}/api/v1/explore/packages/{}/{}/{}/readme-image/",
        percent_encode(registry),
        percent_encode(name),
        percent_encode(version)
    )
}

/// The README for a version this instance holds no bytes for, from the cached
/// upstream document.
///
/// Only the metadata-borne kinds can answer: for an archive-borne one the text
/// is inside bytes we do not have, and fetching them would make browsing a
/// download — which is the non-goal §3 is most explicit about.
///
/// Suppressed for a locally published package, on any mode: sending a private
/// name to a public index on a page view would leak the existence of internal
/// software to a third party (§4.4, §7.7). `discovery_read`'s copy of this rule
/// is the version-list half; this is the README half, and both have to hold or
/// the suppression is decorative.
async fn derived_readme(
    proxy_svc: &ProxyService,
    local_svc: &LocalRegistryService,
    mode_map: &RegistryModeMap,
    registry: &str,
    name: &str,
    version: Option<&str>,
    identity: &AuthIdentity,
) -> Option<(ReadmeAnswer, Freshness)> {
    if mode_map.get(registry) == RegistryMode::Local {
        return None;
    }
    let published_locally = !local_svc
        .backend
        .get_versions(registry, name)
        .await
        .unwrap_or_default()
        .is_empty();
    if published_locally {
        return None;
    }

    let outcome = proxy_svc
        .upstream_detail(registry, name, &identity.0)
        .await
        .ok()
        .flatten()?;

    // The version asked for, or — when nothing was named — the one the document
    // itself attributes a README to. Not "the newest version": a packument's
    // root README belongs to `dist-tags.latest`, and the reader dispatched on
    // that already.
    let from_listing = match version {
        Some(version) => outcome
            .detail
            .readmes
            .get(version)
            .cloned()
            .map(|found| (version.to_owned(), found)),
        // `readmes` is a `HashMap`, and npm's reader puts one entry in it per
        // version that carries a `readme` field *plus* one for
        // `dist-tags.latest` — so `iter().next()` is whichever version the
        // hasher's per-process seed happens to put first, and the same request
        // answers with a different version's text on the next boot. Pick the
        // highest version instead, which is the one a reader who named none
        // means, with a lexicographic tiebreak so the answer is total.
        //
        // The tiebreak is spelled out rather than left to `newest_first`, which
        // only falls back to a string compare when *neither* side parses:
        // `1.0` and `1.0.0`, `v2.1.0` and `2.1.0`, or two versions differing
        // only in build metadata all compare `Equal` under semver, and
        // `max_by` on a `HashMap` would then return whichever the seed put
        // last — the same per-boot coin flip, one case narrower.
        None => outcome
            .detail
            .readmes
            .iter()
            .max_by(|(a, _), (b, _)| {
                newest_first(a, b)
                    .reverse()
                    .then_with(|| a.as_str().cmp(b.as_str()))
            })
            .map(|(v, r)| (v.clone(), r.clone())),
    };

    // npm's packument carries the text and answers here. PyPI's simple page does
    // not — its description lives in a per-version document — and OpenVSX and
    // the VS Code Marketplace answer with a URL. For those, the panel asks about
    // the *one version selected*, which is the cost open question 7 accepts and
    // the reason the version table reports `unknown` until a row is picked.
    let (found_version, content, format, package_level, freshness) = match from_listing {
        Some((found_version, found)) => {
            let content = found.content?;
            (
                found_version,
                content,
                found.format,
                found.package_level,
                outcome.freshness,
            )
        }
        None => {
            // Only for a named version: without one there is nothing to resolve,
            // and guessing which version to ask about would be a request made on
            // the reader's behalf that they did not make.
            let version = version?;
            let (content, format, package_level, freshness) = match proxy_svc
                .upstream_version_readme(registry, name, version, &identity.0)
                .await
            {
                Ok(found) => found?,
                Err(e) => {
                    tracing::debug!(
                        registry, name, version, error = %e,
                        "explore readme: the per-version upstream read failed"
                    );
                    return None;
                }
            };
            (
                version.to_owned(),
                content,
                format,
                package_level,
                freshness,
            )
        }
    };

    Some((
        ReadmeAnswer {
            readme: PackageReadme {
                registry: registry.to_owned(),
                name: name.to_owned(),
                version: found_version,
                digest: batlehub_core::entities::readme_digest(&content),
                content,
                format,
                source: ReadmeSource::UpstreamMetadata,
                // The document's own text, whole: the registry's `max_bytes`
                // caps what is *stored*, and nothing is stored here.
                truncated: false,
                package_level,
                // When the document this came from was cached, which for a
                // derived answer is the closest thing to "when we read it".
                extracted_at: chrono::Utc::now(),
            },
            // Never a fallback: a derived answer is for the version asked for
            // or it is nothing. Substituting another version's text from a
            // document we do not store would be a guess nobody could check.
            is_fallback: false,
        },
        freshness,
    ))
}

/// The vocabulary `Freshness::header_value` already uses.
fn freshness_word(freshness: Freshness) -> &'static str {
    match freshness {
        Freshness::Cached => "cached",
        Freshness::Fresh => "fresh",
        Freshness::Stale => "stale",
    }
}
