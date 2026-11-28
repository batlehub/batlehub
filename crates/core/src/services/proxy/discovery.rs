//! The console's discovery read: asking upstream about a package this instance
//! holds nothing of (RFC 0007 §5.5).
//!
//! The one path in this crate a **page view** can start, and it is deliberately
//! not on the write side of anything. It records no `AccessEvent`, writes no
//! `package_statuses` row, increments no download count, touches no
//! `last_accessed`, consumes no quota, creates no storage entry, and does not
//! make the package appear in the catalogue. A page view must not be able to
//! change what the catalogue claims this instance has — otherwise browsing the
//! console silently rewrites the inventory an operator reads to make decisions
//! (§4.4).
//!
//! Nothing new leaves that the proxy would not otherwise fetch: the request is
//! the same version document a package manager pointed at this registry causes
//! on its first resolve, to the same host, through the same client, with the
//! same timeouts, TLS settings, upstream auth and SSRF guards.

use std::time::Duration;

use crate::entities::{Identity, PackageId, RegistryKind, UpstreamDetailSupport};
use crate::error::CoreError;
use crate::ports::DocumentKind;
use crate::services::upstream_detail::{self, UpstreamDetail, UpstreamVersion};

use super::{Freshness, ProxyRequest, ProxyService};
use crate::entities::Action;

/// How long a reader waits for another reader's in-flight fetch before giving
/// up and doing its own.
///
/// Longer than a healthy upstream fetch and shorter than a page load anybody
/// would sit through: the point is to collapse concurrent readers, not to make
/// one reader's slow upstream everybody's problem.
pub(super) const SINGLE_FLIGHT_WAIT: Duration = Duration::from_secs(10);

/// What one discovery read did, so the page can say which rung answered.
pub struct DiscoveryOutcome {
    pub detail: UpstreamDetail,
    pub freshness: Freshness,
    /// `max_versions` shortened the list. Surfaced, because a silently
    /// shortened list is a lie about the registry.
    pub truncated: bool,
}

impl ProxyService {
    /// Ask upstream what versions of `name` exist, for the console only.
    ///
    /// `Ok(None)` means the read was not attempted, and the four reasons are
    /// all legitimate answers rather than failures: the registry has it turned
    /// off, the kind has no upstream to ask, the registry is `local`-mode, or
    /// upstream is already known not to have this package.
    ///
    /// `Err` means it was attempted and every rung failed — the page then
    /// answers from local rows with the error reported, never an empty page
    /// presented as an answer.
    pub async fn upstream_detail(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<Option<DiscoveryOutcome>, CoreError> {
        let cfg = {
            let hot = self.hot.read().await;
            hot.upstream_detail
                .get(registry)
                .cloned()
                .unwrap_or_default()
        };
        if !cfg.enabled {
            return Ok(None);
        }

        // `request_prelude` rather than a second config read, so the access
        // checks, the client, the TTL and the stale policy are the ones the
        // proxy path already applies rather than a second copy that can drift.
        //
        // A listing document addresses a *package*, not a version, but
        // `validate_coordinate` refuses an empty one — so a placeholder stands
        // in, exactly as `rubygems/compact.rs` uses `__compact__`. It never
        // reaches a URL or a storage key: the cache key below is built from the
        // document kind, and nothing on this path fetches an artifact.
        let req = ProxyRequest {
            package_id: PackageId::listing(registry, name, "__listing__"),
            identity: identity.clone(),
            action: Action::ReleasesRead,
            ip_address: None,
            user_agent: None,
        };
        let prelude = self.request_prelude(&req).await?;
        // RFC 0051 §5.1 point 7 — an unlisted name is never asked of the
        // upstream: "not attempted", and the page answers from what it holds.
        if crate::services::authz::judge_name_in(
            prelude.policy.as_deref(),
            &req.package_id,
            identity,
            req.action,
        )
        .is_err()
        {
            return Ok(None);
        }

        let kind: RegistryKind = prelude
            .client
            .registry_type()
            .parse()
            .map_err(|_| CoreError::NotSupported("unknown registry type".to_owned()))?;

        // **Which upstream question this kind answers — and nothing else.**
        //
        // Everything below this line is shared by both, deliberately: the
        // `ListVersions` branch used to `return` from here, straight past the
        // negative cache, the single-flight claim and the freshness stamp. Every
        // page view of an Open VSX, VS Code, JetBrains or conda package was then
        // one upstream gallery query — and for conda, one `repodata.json` per
        // channel platform, five of them, per view — with a hard-coded
        // `Freshness::Fresh` that made the page say so. That is exactly the
        // amplification this module's doc comment says it prevents, and the way
        // to keep the two paths from drifting again is to have one path.
        let source = match kind.upstream_detail() {
            UpstreamDetailSupport::Document(name) => DiscoverySource::Document(document_kind(name)),
            UpstreamDetailSupport::ListVersions => DiscoverySource::Listing,
            UpstreamDetailSupport::None(_) => return Ok(None),
        };

        // The single-flight and negative-cache key is the metadata cache key,
        // so a page reload during a fetch collapses into the same flight and an
        // absence is remembered per document rather than per package.
        let key = source.cache_key(registry, name);
        if self.discovery.is_absent(&key) {
            return Ok(None);
        }

        let guard = self.discovery.claim(&key, SINGLE_FLIGHT_WAIT).await;
        if guard.is_none() {
            // Somebody else just finished, and wrote the answer before
            // releasing. Reading their cache entry is rung 1 by definition, and
            // is the whole point of coalescing: N readers, one request.
            let cached = self.cached_document_only(&key).await;
            return Ok(cached
                .and_then(|doc| source.decode(kind, &doc))
                .map(|detail| {
                    let (detail, truncated) = capped(detail, &cfg);
                    DiscoveryOutcome {
                        detail,
                        freshness: Freshness::Cached,
                        truncated,
                    }
                }));
        }

        // Whether the answer was already there decides which rung this is, and
        // it has to be asked *before* the fetch that would put it there.
        let fresh_before = self.cached_document_only(&key).await.is_some();
        let fetched = match &source {
            DiscoverySource::Document(doc_kind) => {
                self.cached_version_document(&prelude, &req, name, *doc_kind)
                    .await
            }
            DiscoverySource::Listing => {
                self.cached_version_listing(&prelude, &req, name, &key)
                    .await
            }
        };
        let doc = match fetched {
            Ok(doc) => doc,
            Err(CoreError::NotFound(_)) => {
                // Upstream *answered*, and the answer was "no such package".
                // That is a fact, so it is remembered — a bad URL or a crawler
                // must not turn every reload into an upstream request.
                self.discovery.record_absent(&key, cfg.negative_ttl);
                return Ok(None);
            }
            // A connection failure is not a fact about the package, so it is
            // not cached. The caller reports it and falls back to local rows.
            //
            // Nor is a `NotSupported`: a client that has no such document has
            // told us about *itself*, not about the package, and remembering it
            // as an absence would hide every package on that registry.
            Err(e) => return Err(e),
        };

        let Some(detail) = source.decode(kind, &doc) else {
            // Unreachable for a document this call just built. A cache entry
            // written by an older build under a different shape lands here, and
            // "no upstream answer" is the safe reading of one.
            tracing::debug!(key = %key, "discovery: undecodable upstream document");
            return Ok(None);
        };
        let (detail, truncated) = capped(detail, &cfg);
        Ok(Some(DiscoveryOutcome {
            detail,
            // Rung 1 when the entry was already there, rung 2 when this call
            // fetched it. `cached_version_document` serves a stale entry itself
            // when the registry allows it, and reports it as cached — the page
            // says how old from `extracted_at`, which is the honest granularity
            // available without changing that function's signature.
            freshness: if fresh_before {
                Freshness::Cached
            } else {
                Freshness::Fresh
            },
            truncated,
        }))
    }

    /// One version's README, from its own metadata document.
    ///
    /// The second half of the derived read, for the kinds whose *listing*
    /// document carries no README but whose per-version metadata does:
    ///
    /// - **PyPI** keeps the long description in `/pypi/{name}/{version}/json`,
    ///   so filling it for a whole version table would be N upstream requests
    ///   per page view. The panel asks on selection instead, which is the cost
    ///   RFC 0007 open question 7 accepts.
    /// - **OpenVSX** and the **VS Code Marketplace** answer with a *URL*, which
    ///   this follows through the client's own same-origin and SSRF guards
    ///   (§7.4).
    ///
    /// Cache-first, and it **writes nothing**: `resolve_metadata_uncaptured` is
    /// the same resolve the download path uses with the capture removed, so N
    /// readers of one version during a TTL produce one request and a later real
    /// download finds the entry warm — while no `package_readmes` row appears
    /// because somebody looked at a page (§5.6).
    pub async fn upstream_version_readme(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<Option<(String, crate::entities::ReadmeFormat, bool, Freshness)>, CoreError> {
        let cfg = {
            let hot = self.hot.read().await;
            hot.upstream_detail
                .get(registry)
                .cloned()
                .unwrap_or_default()
        };
        if !cfg.enabled {
            return Ok(None);
        }

        let req = ProxyRequest {
            package_id: PackageId::new(registry, name, version),
            identity: identity.clone(),
            action: Action::ReleasesRead,
            ip_address: None,
            user_agent: None,
        };
        let prelude = self.request_prelude(&req).await?;
        // RFC 0051 §5.1 point 7 — as `upstream_detail`.
        if crate::services::authz::judge_name_in(
            prelude.policy.as_deref(),
            &req.package_id,
            identity,
            req.action,
        )
        .is_err()
        {
            return Ok(None);
        }

        let kind: RegistryKind = prelude
            .client
            .registry_type()
            .parse()
            .map_err(|_| CoreError::NotSupported("unknown registry type".to_owned()))?;
        // Only the kinds that promise a README for an unheld version. Asking an
        // archive-borne kind would resolve a version whose text is inside bytes
        // we do not have, which is a request for nothing.
        if !kind.readme_support().answers_for_unheld_versions() {
            return Ok(None);
        }
        // And not the kinds whose listing document *is* their README source.
        // npm's packument is both; if the listing read found nothing there, a
        // per-version resolve re-fetches the same document to find the same
        // nothing. Verified against the real registry: `express` ships
        // `readme: ""`, and this is what stops that costing a second fetch per
        // version viewed.
        if upstream_detail::listing_carries_readmes(kind) {
            return Ok(None);
        }

        // Whether the answer was already cached decides the rung, and it has to
        // be asked before the resolve that would cache it.
        let was_cached = self
            .cache
            .get(&prelude.cache_key)
            .await
            .ok()
            .flatten()
            .is_some();
        let meta = self
            .resolve_metadata_uncaptured(
                &prelude.client,
                &prelude.policy,
                &req,
                &prelude.cache_key,
                prelude.ttl,
                &prelude.registry_label,
            )
            .await?;

        let Some(found) = crate::entities::MetadataReadme::from_extra(&meta.extra) else {
            return Ok(None);
        };
        let freshness = if was_cached {
            Freshness::Cached
        } else {
            Freshness::Fresh
        };

        if let Some(content) = found.content {
            return Ok(Some((
                content,
                found.format,
                found.package_level,
                freshness,
            )));
        }
        // A link, which is an outbound request in its own right — made here
        // rather than on any protocol path, and guarded by the client that owns
        // the origin check.
        let Some(url) = found.url.as_deref() else {
            return Ok(None);
        };
        let cfg = {
            let hot = self.hot.read().await;
            hot.readme.get(registry).cloned().unwrap_or_default()
        };
        Ok(prelude
            .client
            .fetch_linked_readme(url, cfg.max_bytes)
            .await?
            .map(|text| (text, found.format, found.package_level, freshness)))
    }

    /// `list_versions`, cached under `key` exactly as
    /// [`ProxyService::cached_version_document`] caches a real document.
    ///
    /// The kinds this serves have no listing document the proxy can read but
    /// can enumerate versions: the extension galleries, and conda — whose
    /// `repodata.json` describes a whole channel rather than a package, so
    /// answering one package's versions means fetching and parsing one file per
    /// channel platform. That cost is the reason this is cached and not the
    /// reason it is not: it used to run on *every page view*.
    ///
    /// The version list is stored as a JSON array in the same `doc:` namespace
    /// the real documents use, so it inherits the store's TTL, its stale
    /// handling and `cached_document_only` unchanged. It is not a document any
    /// protocol serves and nothing but [`DiscoverySource::decode`] reads it
    /// back.
    async fn cached_version_listing(
        &self,
        prelude: &super::handle::RequestPrelude,
        req: &ProxyRequest,
        name: &str,
        key: &str,
    ) -> Result<crate::ports::VersionDocument, CoreError> {
        if let Some(doc) = self.cached_document_only(key).await {
            return Ok(doc);
        }

        match prelude.client.list_versions(name).await {
            Ok(versions) => {
                let doc = crate::ports::VersionDocument::json(
                    serde_json::to_value(&versions).unwrap_or(serde_json::Value::Null),
                );
                let entry = crate::ports::CacheEntry {
                    metadata: crate::entities::PackageMetadata {
                        id: req.package_id.clone(),
                        published_at: None,
                        download_url: None,
                        checksum: None,
                        is_signed: None,
                        extra: serde_json::to_value(&doc).unwrap_or(serde_json::Value::Null),
                        cache_control: None,
                    },
                    cached_at: chrono::Utc::now(),
                    // The store owns expiry, exactly as it does for a real
                    // document — a second, independently clocked one could
                    // disagree with it.
                    expires_at: None,
                };
                if let Err(e) = self.cache.set(key, entry, prelude.ttl).await {
                    tracing::warn!(key = %key, error = %e, "caching version listing failed");
                }
                Ok(doc)
            }
            Err(e) => {
                // Same stale policy as a real document: an upstream that has
                // gone away should not empty a page that was answered a minute
                // ago, when the operator has said they would rather have old
                // than nothing.
                let serve_stale = prelude
                    .policy
                    .as_ref()
                    .map(|p| p.serve_stale_metadata)
                    .unwrap_or(false);
                if serve_stale {
                    if let Ok(Some(stale)) = self.cache.get_stale(key).await {
                        if let Ok(doc) = serde_json::from_value::<crate::ports::VersionDocument>(
                            stale.metadata.extra,
                        ) {
                            tracing::warn!(
                                key = %key,
                                error = %e,
                                "upstream version listing unavailable, serving stale"
                            );
                            return Ok(doc);
                        }
                    }
                }
                Err(e)
            }
        }
    }

    /// A cached listing document, without fetching.
    ///
    /// Used to tell rung 1 from rung 2, and to answer a caller that waited out
    /// somebody else's flight. `get` returns only what the store still
    /// considers fresh, so freshness stays the store's job here exactly as it
    /// is in `cached_version_document`.
    async fn cached_document_only(&self, key: &str) -> Option<crate::ports::VersionDocument> {
        let entry = self.cache.get(key).await.ok().flatten()?;
        serde_json::from_value(entry.metadata.extra).ok()
    }
}

/// Where one kind's discovery answer comes from.
///
/// Both arms share the whole scaffolding around them — the negative cache, the
/// single flight, the `doc:` cache namespace, the freshness stamp — and differ
/// only in what they ask upstream and how the answer decodes. Kept as one type
/// rather than two code paths because it was two code paths, and the second one
/// quietly had none of the scaffolding.
enum DiscoverySource {
    /// A real listing document the protocol serves.
    Document(DocumentKind),
    /// `RegistryClient::list_versions` — a query, not a document.
    Listing,
}

impl DiscoverySource {
    /// The cache key this answer lives under.
    ///
    /// `Document` must reproduce [`ProxyService::cached_version_document`]'s key
    /// exactly, or the freshness probe and the single flight guard a different
    /// entry than the fetch writes.
    fn cache_key(&self, registry: &str, name: &str) -> String {
        match self {
            Self::Document(doc_kind) => format!("doc:{registry}:{}:{name}", doc_kind.as_str()),
            // A discriminant no `DocumentKind::as_str()` returns, so a listing
            // answer can never collide with a real document's entry. Changing
            // it is a cache invalidation rather than a rename, same as
            // `DocumentKind::Secondary`.
            Self::Listing => format!("doc:{registry}:__list_versions__:{name}"),
        }
    }

    /// The detail a cached or freshly fetched document carries.
    ///
    /// `None` when the entry does not decode — an entry written by an older
    /// build under a different shape — which the caller treats as a miss.
    fn decode(
        &self,
        kind: RegistryKind,
        doc: &crate::ports::VersionDocument,
    ) -> Option<UpstreamDetail> {
        match self {
            Self::Document(_) => Some(upstream_detail::dispatch(kind, doc)),
            Self::Listing => {
                let crate::ports::DocumentBody::Json(value) = &doc.body else {
                    return None;
                };
                let versions: Vec<String> = serde_json::from_value(value.clone()).ok()?;
                Some(UpstreamDetail {
                    versions: versions.into_iter().map(UpstreamVersion::bare).collect(),
                    readmes: Default::default(),
                    // `list_versions` answers with version strings and nothing
                    // else, so these kinds keep answering the page's link from
                    // the metadata cache — which their README panel warms, one
                    // page view later.
                    links: None,
                })
            }
        }
    }
}

/// The `DocumentKind` a support string names.
///
/// The strings are checked against the real enum by the drift test in
/// `services::upstream_detail`, so an unrecognised one here is unreachable —
/// but it degrades to the primary listing rather than panicking on a page.
fn document_kind(name: &str) -> DocumentKind {
    match name {
        "simple-json" => DocumentKind::SIMPLE_JSON,
        "registration" => DocumentKind::REGISTRATION,
        "latest" => DocumentKind::LATEST,
        "p2-dev" => DocumentKind::P2_DEV,
        "gem" => DocumentKind::GEM,
        _ => DocumentKind::Versions,
    }
}

/// Apply `max_versions`, and say whether it bit.
///
/// The cap keeps the **first N rows of the table**, in the table's own order:
/// stable before pre-release, then newest first. Consistency with what the
/// reader sees matters more than a second ordering here — a truncated list
/// whose kept rows were not the ones at the top would be confusing in a way a
/// shorter list is not, and the reader deciding whether to adopt something is
/// looking at the top of the table either way.
fn capped(
    mut detail: UpstreamDetail,
    cfg: &crate::services::hot_config::UpstreamDetailConfig,
) -> (UpstreamDetail, bool) {
    if detail.versions.len() <= cfg.max_versions {
        return (detail, false);
    }
    // The same ordering the version table uses — stable before pre-release,
    // then newest first — so a row does not move depending on which list it
    // came from, **and so the rows thrown away here are the ones both orders
    // agree are oldest**. This was `b.version.cmp(&a.version)`: a descending
    // *string* compare, under which `5.9.0` outranks `5.19.0`. On a package
    // with more than `max_versions` upstream versions (`typescript`,
    // `aws-sdk`, `@babel/*`) that kept the single-digit-minor releases and
    // discarded the current one, so the page showed a table with the newest
    // version missing and `default_selection` opened on a stale one.
    detail.versions.sort_by(|a, b| {
        a.is_prerelease
            .cmp(&b.is_prerelease)
            .then_with(|| crate::services::newest_first(&a.version, &b.version))
    });
    detail.versions.truncate(cfg.max_versions);
    let kept: std::collections::HashSet<&str> =
        detail.versions.iter().map(|v| v.version.as_str()).collect();
    detail
        .readmes
        .retain(|version, _| kept.contains(version.as_str()));
    (detail, true)
}
