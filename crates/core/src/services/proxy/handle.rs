use std::sync::Arc;
use std::time::Instant;

use crate::entities::AccessEvent;
use crate::entities::MissKind;
use crate::error::CoreError;
use crate::ports::{DocumentKind, VersionDocument};
use crate::rules::{evaluate_rules, RuleContext, RuleDecision};

use super::{ProxyRequest, ProxyResponse, ProxyService, RequestTiming};
use crate::entities::Action;

/// Largest artifact that re-serve verification (`verify_on_serve`) will retain in
/// memory so it can be hashed and served from the same buffer in a single read.
/// Artifacts above this size are hashed by streaming (memory stays bounded) and
/// then re-opened from storage to serve. 32 MiB comfortably covers typical
/// package artifacts (npm tarballs, wheels, crates) while capping per-request
/// memory for pathologically large ones.
pub(crate) const REVERIFY_BUFFER_LIMIT: usize = 32 * 1024 * 1024;

/// Everything `handle` (and the metadata-only entry point) derives from a
/// `ProxyRequest` before any I/O: the hot-config snapshot for the registry plus
/// the metadata cache key/TTL. Extracted so `resolve_metadata_for` shares the
/// exact same validation, lock discipline, and key derivation as `handle`.
pub(super) struct RequestPrelude {
    pub(super) client: Arc<dyn crate::ports::RegistryClient>,
    pub(super) policy: Option<Arc<crate::services::hot_config::RegistryPolicy>>,
    pub(super) integrity: crate::services::hot_config::IntegrityPolicy,
    pub(super) limit: u64,
    pub(super) cache_key: String,
    pub(super) ttl: Option<std::time::Duration>,
    pub(super) registry_label: Arc<str>,
}

/// Whether the coordinate names a raw file — the one forge kind with its own
/// size ceiling and its own script policy.
fn is_raw_coordinate(pkg: &crate::entities::PackageId) -> bool {
    matches!(
        crate::entities::ForgeCoordinate::from_package_id(pkg).map(|c| c.kind),
        Some(crate::entities::ForgeKind::Raw { .. })
    )
}

/// Merge a ref resolution into the metadata the rules and the cache see
/// (RFC 0019 §4.2 *Metadata contract*), under `extra.forge`.
///
/// `published_at` is filled only when the client left it empty — a release
/// keeps its release date, which is the better answer — from the tagger or
/// committer date the resolution carried. Keys the client already wrote under
/// `forge` (a commit's `committed_at`, its `committer`) are kept; the
/// resolution adds the ref kind and the two SHAs beside them.
fn overlay_forge_metadata(
    mut metadata: crate::entities::PackageMetadata,
    resolved: &crate::entities::ResolvedRef,
) -> crate::entities::PackageMetadata {
    use crate::entities::FORGE_EXTRA_KEY;
    if metadata.published_at.is_none() {
        metadata.published_at = resolved.object_date;
    }
    let mut forge = match metadata.extra.get(FORGE_EXTRA_KEY) {
        Some(serde_json::Value::Object(existing)) => existing.clone(),
        _ => serde_json::Map::new(),
    };
    forge.insert("ref_kind".into(), resolved.kind.as_str().into());
    forge.insert("requested_ref".into(), resolved.requested.clone().into());
    forge.insert("resolved_commit".into(), resolved.sha.clone().into());
    forge.insert(
        "previous_commit".into(),
        resolved
            .previous
            .clone()
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    if let Some(p) = &resolved.publisher {
        forge.entry("publisher").or_insert_with(|| p.clone().into());
    }
    match &mut metadata.extra {
        serde_json::Value::Object(map) => {
            map.insert(FORGE_EXTRA_KEY.into(), serde_json::Value::Object(forge));
        }
        other => {
            // `minimal()` metadata carries `Null`; a listing carries an array.
            // Neither has room for a key, so the object is built around it
            // rather than lost: the previous value moves under `upstream`.
            let previous = std::mem::take(other);
            let mut map = serde_json::Map::new();
            if !previous.is_null() {
                map.insert("upstream".into(), previous);
            }
            map.insert(FORGE_EXTRA_KEY.into(), serde_json::Value::Object(forge));
            *other = serde_json::Value::Object(map);
        }
    }
    metadata
}

impl ProxyService {
    /// Validate the coordinate, snapshot the registry's hot config (one brief
    /// read lock, released before any async I/O), and derive the metadata
    /// cache key + TTL.
    pub(super) async fn request_prelude(
        &self,
        req: &ProxyRequest,
    ) -> Result<RequestPrelude, CoreError> {
        // Edge chokepoint: reject any package coordinate that would escape the
        // storage root once interpolated into the cache key, before it reaches the
        // metadata cache or the storage backend. Covers every registry that proxies
        // through here, regardless of per-adapter input validation.
        crate::services::validate_coordinate(
            &req.package_id.name,
            &req.package_id.version,
            req.package_id.artifact.as_deref(),
        )?;

        let registry_name: &str = req.package_id.registry.as_str();
        // Arc<str> instead of String: every downstream metrics call clones this
        // cheaply (atomic refcount bump) instead of copying the registry name's
        // bytes on every `counter!`/`histogram!` invocation.
        let registry_label: Arc<str> = Arc::from(registry_name);

        let (client, policy, integrity, limit, raw_limit) = {
            let hot = self.hot.read().await;
            let client = hot
                .registries
                .get(registry_name)
                .ok_or_else(|| CoreError::UnknownRegistry(registry_name.to_owned()))?
                .clone();
            let policy = hot.policies.get(registry_name).cloned();
            // Registries without an explicit `[registries.integrity]` block get the
            // default policy: verify against any advertised checksum, block on mismatch.
            let integrity = hot
                .integrity
                .get(registry_name)
                .cloned()
                .unwrap_or_default();
            let limit = hot.max_artifact_size_bytes.unwrap_or(500 * 1024 * 1024);
            // RFC 0019 §4.2 *Raw content*: a raw file has its own, lower
            // ceiling. Applied here rather than in the client so it is the
            // *stream* that stops — the file is refused, never truncated —
            // and so every forge gets it from one place. `min` because the
            // global limit still wins if it is the smaller of the two, which
            // validation makes impossible for a written policy and possible
            // for the default one.
            let raw_limit = hot
                .forge_raw
                .get(registry_name)
                .filter(|p| p.enabled)
                .map(|p| p.max_size_bytes);
            (client, policy, integrity, limit, raw_limit)
        };
        let limit = match (raw_limit, is_raw_coordinate(&req.package_id)) {
            (Some(raw), true) => limit.min(raw),
            _ => limit,
        };

        let cache_key = super::proxy_meta_key(&req.package_id);
        let ttl = policy.as_ref().and_then(|p| p.metadata_ttl);

        Ok(RequestPrelude {
            client,
            policy,
            integrity,
            limit,
            cache_key,
            ttl,
            registry_label,
        })
    }

    /// The metadata for a coordinate **if it is already cached**, never fetching.
    ///
    /// The sibling of [`Self::resolve_metadata_for`] for callers that must not
    /// cause egress. The console's package page is the motivating one: it renders
    /// on a page view, and `explore_upstream_detail.rs` asserts it performs
    /// **zero** per-version resolves — "filling it for every row would be N
    /// upstream requests per page view". A cache-*first* read still fetches on a
    /// cold cache, which is the same defect one request at a time.
    ///
    /// So this answers only from what a legitimate resolve already put there: a
    /// download, a README read, a package manager's request. `None` means *we
    /// have not looked*, which the caller renders as absence rather than as a
    /// claim.
    ///
    /// No rule evaluation, deliberately. There is no upstream call to authorise
    /// and nothing is served from it but display metadata the caller has already
    /// gated by its own visibility check — this returns what is in the cache, and
    /// deciding who may see the page is the caller's job, as it is for every
    /// other field on it.
    pub async fn cached_metadata_for(
        &self,
        package_id: &crate::entities::PackageId,
    ) -> Option<crate::entities::PackageMetadata> {
        // The same edge chokepoint `request_prelude` applies, for the same
        // reason: this interpolates the coordinate into a cache key.
        crate::services::validate_coordinate(
            &package_id.name,
            &package_id.version,
            package_id.artifact.as_deref(),
        )
        .ok()?;
        // The same helper `request_prelude` writes through — the two must not
        // drift, or this reads a key nothing ever writes and silently answers
        // `None`.
        let cache_key = super::proxy_meta_key(package_id);
        Some(self.cache.get(&cache_key).await.ok()??.metadata)
    }

    /// Resolve a package's metadata through the cache-first / stale-on-error
    /// pipeline **without** streaming an artifact, enforcing the registry's
    /// policy rules against the resolved metadata (`AccessDenied` on deny).
    ///
    /// This is the metadata-only sibling of [`Self::handle`] — same coordinate
    /// validation, same hot-config snapshot, same `meta:` cache key and TTL —
    /// for handlers that render responses from `PackageMetadata.extra` (e.g.
    /// the JetBrains Marketplace per-plugin endpoints). Because it goes through
    /// `resolve_metadata_cached`, anything resolved once keeps resolving from
    /// cache (or stale cache, when `serve_stale` allows) after upstream loss.
    pub async fn resolve_metadata_for(
        &self,
        req: &ProxyRequest,
    ) -> Result<crate::entities::PackageMetadata, CoreError> {
        self.resolve_metadata_for_inner(req, true).await
    }

    /// [`Self::resolve_metadata_for`] without the README capture.
    ///
    /// For the console: a **page view** must write nothing. `resolve_metadata_for`
    /// runs `maybe_record_readme`, which stores whatever README the upstream
    /// document carried without checking that this instance holds the version —
    /// so the package page's homepage/repository lookup left a `package_readmes`
    /// row for a version nothing here has bytes for, which nothing ever deletes
    /// (deletion keys on a version being deleted, and a version never held here
    /// is never deleted). It also flipped that version's `readme_state` from
    /// `unknown` to `available` on the next load, claiming a stored README for
    /// bytes we never had.
    ///
    /// Everything else is identical, the cache included: N readers of the same
    /// version during one TTL still produce one upstream request.
    pub async fn resolve_metadata_uncaptured_for(
        &self,
        req: &ProxyRequest,
    ) -> Result<crate::entities::PackageMetadata, CoreError> {
        self.resolve_metadata_for_inner(req, false).await
    }

    async fn resolve_metadata_for_inner(
        &self,
        req: &ProxyRequest,
        capture_readme: bool,
    ) -> Result<crate::entities::PackageMetadata, CoreError> {
        let prelude = self.request_prelude(req).await?;
        let metadata = if capture_readme {
            self.resolve_metadata_cached(
                &prelude.client,
                &prelude.policy,
                req,
                &prelude.cache_key,
                prelude.ttl,
                &prelude.registry_label,
            )
            .await?
        } else {
            self.resolve_metadata_uncaptured(
                &prelude.client,
                &prelude.policy,
                req,
                &prelude.cache_key,
                prelude.ttl,
                &prelude.registry_label,
            )
            .await?
        };

        // Grants first, then the gates — RFC 0015 §5.1 and §5.2. The proxy path
        // has to resolve them itself: `RbacRule` is no longer in the chain, so
        // the rules below judge only the artifact, and a route that reaches this
        // without going through a `chain::*` funnel would otherwise be
        // ungated. Two were, until the authorization matrix said so — the
        // RubyGems gemspec route and the `generic` path mirror, both of which
        // have no local branch at all.
        crate::services::authz::authorize_grants_public(
            &self.hot,
            &req.package_id,
            &req.identity,
            req.action,
        )
        .await?;

        let empty: Vec<Box<dyn crate::rules::Rule>> = vec![];
        let rules = prelude
            .policy
            .as_ref()
            .map(|p| p.rules.as_slice())
            .unwrap_or(empty.as_slice());
        let ctx = RuleContext {
            identity: &req.identity,
            package: &metadata,
            action: req.action,
            cache_entry: None,
            requested_version: Some(&req.package_id.version),
        };
        if let RuleDecision::Deny { reason } = evaluate_rules(rules, &ctx).await {
            return Err(CoreError::AccessDenied(reason));
        }

        Ok(metadata)
    }

    pub async fn handle(&self, mut req: ProxyRequest) -> Result<ProxyResponse, CoreError> {
        // RFC 0019 §4.2: on a forge, the ref is resolved to a commit *before*
        // anything else, and an archive or raw coordinate is rewritten onto
        // that commit so the cache, the metadata and the rules all see the
        // SHA. Everything below this line is unchanged for every other kind.
        let coordinate = req.package_id.clone();
        let resolved = match self.resolve_forge_ref(&mut req).await {
            Ok(r) => r,
            Err(e) => return Err(self.record_if_missing(e, &coordinate, MissKind::Ref).await),
        };
        // The coordinate the bytes are stored under, captured after the ref
        // rewrite and before `req` is consumed. RFC 0008's export reads it
        // back off the response so a bundle names the key this instance
        // serves from rather than one the client derived from a URL.
        let served = req.package_id.clone();
        let response = match self.handle_resolved(req, resolved.as_ref()).await {
            Ok(r) => r,
            // RFC 0008 §5.3: the record is written *here*, above the rule
            // chain's own exits, so a coordinate a rule denied is never
            // proposed for the next bundle — a blocked package is not a gap
            // in the mirror. Only the offline client's refusal reaches this
            // arm; every other error passes through untouched.
            Err(e) => {
                return Err(self
                    .record_if_missing(e, &coordinate, MissKind::Artifact)
                    .await)
            }
        };
        Ok(match (response, resolved) {
            (ProxyResponse::Stream(stream), Some(resolved)) => ProxyResponse::ForgeStream {
                stream,
                resolved: Box::new(resolved),
                keyed: Box::new(served),
            },
            // A warned forge artifact carries both: the ref it resolved to
            // and the verdict it was served under.
            (ProxyResponse::Warned { response, verdict }, Some(resolved)) => {
                ProxyResponse::Warned {
                    response: Box::new(match *response {
                        ProxyResponse::Stream(stream) => ProxyResponse::ForgeStream {
                            stream,
                            resolved: Box::new(resolved),
                            keyed: Box::new(served),
                        },
                        other => other,
                    }),
                    verdict,
                }
            }
            (other, _) => other,
        })
    }

    /// Record a `ContentUnavailable` and hand the error back unchanged.
    ///
    /// Fire-and-forget by design (RFC 0008 §6.2): a recorder that cannot
    /// write must never turn a `503` into a `500`. The estate loses one line
    /// of its next bundle list, which is worth strictly less than the
    /// request it would otherwise break.
    pub(super) async fn record_if_missing(
        &self,
        error: CoreError,
        coordinate: &crate::entities::PackageId,
        kind: MissKind,
    ) -> CoreError {
        let CoreError::ContentUnavailable { registry, key } = &error else {
            return error;
        };
        // RFC 0008 §5.3 and decision 7: *a blocked package is not a gap in
        // the mirror.* The RFC put this after the rule chain, and on an
        // air-gapped instance the chain never runs: the offline client
        // refuses metadata resolution first, so the block list is never
        // consulted and an administrator's own refusal would arrive as
        // "the next bundle needs this". The check is therefore made here,
        // on the miss path only, where it costs one lookup on a request
        // that has already failed — and it changes the *answer* too, which
        // is the half that matters to the operator reading the `503`.
        if let Some(reason) = self.blocked_reason(coordinate).await {
            return CoreError::AccessDenied(reason);
        }
        let recorder = {
            let hot = self.hot.read().await;
            hot.air_gap
                .record_misses
                .then(|| hot.miss_recorder.clone())
                .flatten()
        };
        let Some(recorder) = recorder else {
            return error;
        };
        let miss = crate::entities::ContentMiss {
            registry: registry.clone(),
            storage_key: key.clone(),
            kind,
            coordinate: Some(coordinate.cache_key()),
        };
        if let Err(e) = recorder.record(&miss, chrono::Utc::now()).await {
            tracing::warn!(key = %key, error = %e, "air gap: could not record the miss");
        }
        error
    }

    /// The administrator's reason for blocking this coordinate, if they did.
    ///
    /// The same widening `BlockListRule` does — the requested coordinate,
    /// then the bare version — because a block on a version covers every
    /// file of it, and a download addresses a file.
    ///
    /// Fails **open**, as the rule does: an unreadable store must not turn a
    /// miss into a refusal that names a block nobody wrote.
    async fn blocked_reason(&self, id: &crate::entities::PackageId) -> Option<String> {
        use crate::entities::PackageStatus;
        for candidate in [
            Some(id.clone()),
            id.artifact.as_ref().map(|_| crate::entities::PackageId {
                artifact: None,
                ..id.clone()
            }),
        ]
        .into_iter()
        .flatten()
        {
            if let Ok(PackageStatus::Blocked { reason, .. }) =
                self.repo.get_status(&candidate).await
            {
                return Some(reason);
            }
        }
        None
    }

    /// Resolve a forge coordinate's ref and rewrite the request onto the
    /// commit (RFC 0019 §4.2 *Cache key*). `None` for anything that is not a
    /// forge read of a ref — every package registry, a release listing, the
    /// Forgejo packages passthrough — and for a forge client that does not
    /// answer [`crate::ports::RegistryClient::forge`] (a fan-out over several
    /// upstreams, today).
    ///
    /// Releases and assets are resolved but **not** rewritten: their bytes are
    /// identified by the upload, not the commit, so the tag stays the key. The
    /// resolution is still recorded, which is what makes a moved tag
    /// detectable.
    async fn resolve_forge_ref(
        &self,
        req: &mut ProxyRequest,
    ) -> Result<Option<crate::entities::ResolvedRef>, CoreError> {
        let Some(coord) = crate::entities::ForgeCoordinate::from_package_id(&req.package_id) else {
            return Ok(None);
        };
        let Some(git_ref) = coord.git_ref().map(str::to_owned) else {
            return Ok(None);
        };
        let registry = req.package_id.registry.clone();
        let (client, store, policy) = {
            let hot = self.hot.read().await;
            let Some(client) = hot.registries.get(&registry) else {
                // Unknown registry: the prelude answers that with the right
                // error, so nothing is said here.
                return Ok(None);
            };
            (Arc::clone(client), hot.ref_resolutions.clone(), {
                let mut p = hot.forge_refs.get(&registry).copied().unwrap_or_default();
                // RFC 0008 §13.3: an air-gapped instance re-resolves
                // nothing, because there is nothing to re-resolve
                // against.
                p.frozen = hot.air_gap.enabled;
                p
            })
        };
        let Some(forge) = client.forge() else {
            return Ok(None);
        };
        let resolved = crate::services::forge_refs::resolve_ref(
            &registry,
            forge,
            store.as_ref(),
            policy,
            &coord.owner_repo,
            &git_ref,
        )
        .await?;
        if coord.keyed_by_commit() {
            req.package_id = coord.rewrite_onto(&req.package_id, &resolved.sha);
        }
        Ok(Some(resolved))
    }

    async fn handle_resolved(
        &self,
        req: ProxyRequest,
        resolved: Option<&crate::entities::ResolvedRef>,
    ) -> Result<ProxyResponse, CoreError> {
        let start = Instant::now();
        let RequestPrelude {
            client,
            policy,
            integrity,
            limit,
            cache_key,
            ttl,
            registry_label,
        } = self.request_prelude(&req).await?;

        // ── 1. Resolve metadata (cache-first) ─────────────────────────────────
        let metadata = self
            .resolve_metadata_cached(&client, &policy, &req, &cache_key, ttl, &registry_label)
            .await?;
        // RFC 0019 §4.2 *Metadata contract*: what the ref resolved to rides in
        // `extra.forge`, and a coordinate the client could not date takes the
        // object's date from the resolution. Per request rather than cached:
        // the same commit reached through a tag and through a branch is one
        // cached entry and two ref kinds.
        let metadata = match resolved {
            Some(r) => overlay_forge_metadata(metadata, r),
            None => metadata,
        };

        // ── 2. Evaluate grants, then rules ─────────────────────────────────────
        //
        // A grant denial is a denial, so it takes the same exit as a rule
        // denial: the audit record, the `denied` metric, and `ProxyResponse::Denied`
        // rather than an error. Returning `?` here instead skipped all three —
        // the caller still got a 403, and the access log had no row for it,
        // which is precisely the state `audit:read` exists to make readable.
        if let Err(e) = crate::services::authz::authorize_grants_public(
            &self.hot,
            &req.package_id,
            &req.identity,
            req.action,
        )
        .await
        {
            let reason = e.to_string();
            super::warn_if_audit_failed(
                self.repo
                    .record_access(AccessEvent::denied_download(
                        req.package_id,
                        req.identity.user_id,
                        req.identity.role,
                        reason.clone(),
                    ))
                    .await,
                "denied download",
            );
            super::finish_request(&registry_label, "denied", start);
            return Ok(ProxyResponse::Denied {
                reason,
                verdict: None,
            });
        }

        let empty: Vec<Box<dyn crate::rules::Rule>> = vec![];
        let rules = policy
            .as_ref()
            .map(|p| p.rules.as_slice())
            .unwrap_or(empty.as_slice());

        let ctx = RuleContext {
            identity: &req.identity,
            package: &metadata,
            action: req.action,
            cache_entry: None,
            requested_version: Some(&req.package_id.version),
        };

        // The verdict gate leaves the verdict it judged under beside its
        // decision (RFC 0018 §4.2), so a refusal can carry the reason codes
        // and a `warned` stream its headers, without the chain's other rules
        // knowing anything about it.
        let (decision, verdict) =
            crate::services::verdict::with_request_verdict(evaluate_rules(rules, &ctx)).await;
        if let RuleDecision::Deny { reason } = decision {
            super::warn_if_audit_failed(
                self.repo
                    .record_access(AccessEvent::denied_download(
                        req.package_id,
                        req.identity.user_id,
                        req.identity.role,
                        reason.clone(),
                    ))
                    .await,
                "denied download",
            );
            super::finish_request(&registry_label, "denied", start);
            return Ok(ProxyResponse::Denied {
                reason,
                verdict: verdict.map(Box::new),
            });
        }
        let warned = verdict
            .filter(|v| v.state == crate::entities::VerdictState::Warned)
            .map(Box::new);

        let response = self
            .serve_after_rules(
                req,
                client,
                policy,
                metadata,
                integrity,
                limit,
                registry_label,
                start,
            )
            .await?;
        Ok(match warned {
            Some(verdict) => ProxyResponse::Warned {
                response: Box::new(response),
                verdict,
            },
            None => response,
        })
    }

    /// Everything after the rules have allowed the request: the firewall
    /// stream, the cache hit, or the fetch-and-cache.
    #[allow(clippy::too_many_arguments)]
    async fn serve_after_rules(
        &self,
        req: ProxyRequest,
        client: Arc<dyn crate::ports::RegistryClient>,
        policy: Option<Arc<crate::services::hot_config::RegistryPolicy>>,
        metadata: crate::entities::PackageMetadata,
        integrity: crate::services::hot_config::IntegrityPolicy,
        limit: u64,
        registry_label: Arc<str>,
        start: Instant,
    ) -> Result<ProxyResponse, CoreError> {
        let registry_name: &str = req.package_id.registry.as_str();

        // ── 3. Firewall-only: stream directly from upstream, skip all caching ──
        let firewall_only = policy.as_ref().map(|p| p.firewall_only).unwrap_or(false);

        if firewall_only {
            tracing::debug!(registry = %registry_name, "firewall-only mode, streaming from upstream");
            let upstream_start = Instant::now();
            let mut upstream = self
                .fetch_artifact_or_record_error(&client, &req, &registry_label, upstream_start)
                .await?;
            // Times the whole body transfer, not just time-to-headers — this is the
            // only latency signal firewall-only registries get, since they never hit
            // the artifact cache path.
            upstream.stream = super::time_upstream_stream(
                Arc::clone(&registry_label),
                "fetch_artifact",
                upstream_start,
                Arc::clone(&self.metrics),
                upstream.stream,
            );
            super::warn_if_audit_failed(
                // `allowed_read`, not `allowed_download`: a `.sha1`/`.asc`
                // beside a Maven jar is recorded as metadata, so one `mvn`
                // resolution counts as one download rather than four. The local
                // path calls the same function — see
                // `PackageId::is_verification_sidecar`.
                self.repo
                    .record_access(AccessEvent::allowed_read(
                        req.package_id,
                        req.identity.user_id,
                        req.identity.role,
                    ))
                    .await,
                "allowed download",
            );
            super::finish_request(&registry_label, "allowed", start);
            return Ok(ProxyResponse::Stream(upstream.stream));
        }

        // ── 4. Check artifact cache ────────────────────────────────────────────
        let artifact_key = super::proxy_artifact_key(&req.package_id);
        let artifact_ttl = policy.as_ref().and_then(|p| p.artifact_ttl);
        let cached_artifact_is_fresh = self
            .artifact_is_fresh(&artifact_key, artifact_ttl, registry_name)
            .await?;

        if cached_artifact_is_fresh {
            // ── 5a. Cache hit (see `cache::serve_cache_hit`) ──────────────────
            let timing = RequestTiming {
                registry_label,
                start,
            };
            return self
                .serve_cache_hit(req, artifact_key, &integrity, &timing)
                .await;
        }

        // ── 5b. Cache miss: fetch + cache (see `cache::fetch_and_cache`) ───────
        let timing = RequestTiming {
            registry_label,
            start,
        };
        self.fetch_and_cache(req, client, metadata, &integrity, limit, &timing)
            .await
    }

    /// Authorize a read against a registry's policy rules **without** resolving
    /// upstream metadata or streaming an artifact.
    ///
    /// Path-addressed registries (deb/rpm) serve approved files straight from
    /// local storage, bypassing [`Self::handle`]. They call this first so a
    /// Local/Hybrid read enforces the same RBAC as the proxy fall-through (which
    /// builds the same synthetic `repo` coordinate and runs the full rule chain).
    /// Returns `AccessDenied` when the policy denies the read.
    ///
    /// The chain itself lives in [`crate::services::authz`] so that
    /// `LocalRegistryService` can run the same evaluation from its own read
    /// funnels — see that module for why it is not a method here.
    pub async fn authorize_read(
        &self,
        package_id: &crate::entities::PackageId,
        identity: &crate::entities::Identity,
        action: Action,
    ) -> Result<(), CoreError> {
        crate::services::authz::authorize_read(&self.hot, package_id, identity, action).await
    }

    /// Authorize a *listing* — a request for a whole package's version document,
    /// not for one version of it. Only the identity-keyed `rbac` rule runs; see
    /// [`crate::services::authz::authorize_listing`] for why the rest
    /// of the chain would blank the document rather than gate it.
    ///
    /// Public because the web layer needs it for the routes that are listings by
    /// shape rather than by name: a search names many packages and no single
    /// version, and so does a whole-registry index such as Composer's
    /// `packages.json`. Handing either to the full chain judges it against a
    /// coordinate that describes nothing.
    ///
    /// Blocked versions are separately stripped from the document itself by
    /// [`Self::version_document`].
    pub async fn authorize_listing(
        &self,
        package_id: &crate::entities::PackageId,
        identity: &crate::entities::Identity,
        action: Action,
    ) -> Result<(), CoreError> {
        crate::services::authz::authorize_listing(&self.hot, package_id, identity, action).await
    }
    /// Authorise a listing read, filing a denial as its own audit event.
    ///
    /// A denial is recorded individually, with the identity, the coordinate and
    /// the reason. It is a security event that has to be inspectable one at a
    /// time, there are few of them, and an operator asking "who was refused,
    /// and why" needs the answer rather than a count.
    ///
    /// `what` names the document in the audit-write warning: the two callers
    /// serve different listing shapes, and a failed audit write should say
    /// which one it was.
    async fn authorize_listing_audited(
        &self,
        req: &ProxyRequest,
        what: &'static str,
    ) -> Result<(), CoreError> {
        let Err(e) = self
            // `releases:list`, not `req.action`: this funnel is only reached for a
            // document that names many versions or many packages, and §4.2 gives
            // that its own verb. The handler's own action still judges the
            // artifact read that follows.
            .authorize_listing(
                &req.package_id,
                &req.identity,
                crate::entities::Action::ReleasesList,
            )
            .await
        else {
            return Ok(());
        };
        if let CoreError::AccessDenied(reason) = &e {
            super::warn_if_audit_failed(
                self.repo
                    .record_access(AccessEvent::denied_metadata(
                        req.package_id.clone(),
                        req.identity.user_id.clone(),
                        req.identity.role.clone(),
                        reason.clone(),
                    ))
                    .await,
                what,
            );
        }
        Err(e)
    }

    /// Serve a proxied registry's version-listing document — for npm, the
    /// packument — with blocked versions removed and artifact URLs pointed back
    /// at this proxy.
    ///
    /// The two rewrites are what make the document *this* proxy's answer rather
    /// than a copy of the upstream's:
    ///
    /// - **Blocked versions are stripped** and `dist-tags.latest` recomputed, so
    ///   a resolver asking for `latest` or a range never selects a version the
    ///   operator has blocked. Without this the resolver picks the blocked
    ///   version from the upstream listing and the install fails at download —
    ///   the block reads as breakage rather than policy.
    /// - **`dist.tarball` is rewritten** to this proxy's own download route.
    ///   The upstream document points at the upstream CDN; served unchanged it
    ///   would route every download around the proxy, past its cache, its audit
    ///   trail and the download-time gate that is the block's other half.
    ///
    /// Only RBAC is evaluated here, not the whole rule chain. The chain judges a
    /// *concrete* version and still runs on the download that follows; applying
    /// it to the listing would deny the entire document because one version in
    /// it is gated, which is the opposite of letting a client resolve past that
    /// version to one it may have.
    pub async fn version_document(
        &self,
        req: &ProxyRequest,
        doc_kind: DocumentKind,
        public_base: &str,
    ) -> Result<VersionDocument, CoreError> {
        let prelude = self.request_prelude(req).await?;
        self.authorize_listing_audited(req, "denied version document")
            .await?;

        let name = req.package_id.name.as_str();
        let mut doc = match self
            .cached_version_document(&prelude, req, name, doc_kind)
            .await
        {
            Ok(d) => d,
            // A listing an air-gapped instance does not hold is the other
            // half of RFC 0008's record: the next bundle needs the document
            // as much as the bytes.
            Err(e) => {
                return Err(self
                    .record_if_missing(e, &req.package_id, MissKind::Document)
                    .await)
            }
        };

        let kind = prelude.client.registry_type().parse().unwrap_or_else(|_| {
            // Unreachable in practice: `registry_type()` returns the same
            // kebab-case string `RegistryKind` round-trips. Treating an unknown
            // one as `Generic` keeps the listing served and unfiltered, which is
            // the fail-open direction this whole path takes.
            tracing::warn!(
                registry_type = prelude.client.registry_type(),
                "registry client reports a type RegistryKind does not know; not filtering"
            );
            crate::entities::RegistryKind::Generic
        });
        // The blocked set is a statement about the *package*, and for one kind
        // the listing coordinate says more than that: SDKMAN's carries the
        // platform (`java/linuxx64`), and a block on a JDK must cover all of
        // them (RFC 0010 §6.2). Every other kind returns `name` unchanged.
        let blocking_name = kind.blocking_package_name(name);
        let ctx = crate::services::blocking::ListingContext {
            registry: &req.package_id.registry,
            kind,
            document: doc_kind,
            package: blocking_name,
            public_base,
        };

        let blocked = self
            .blocked_versions_for(&req.package_id.registry, blocking_name, kind)
            .await;

        crate::services::blocking::dispatch(&ctx, &mut doc, &blocked);
        crate::services::blocking::rewrite_urls(&ctx, &mut doc);

        // An allowed listing is counted, not filed. `StatsRollupService` turns
        // this into one durable row per registry per hour, so a `cargo build`
        // over a 400-crate graph moves a counter 400 times and writes nothing.
        // What that gives up is per-package and per-identity attribution for
        // *allowed* listing reads; "who downloaded this artifact" and "who was
        // refused" both keep their own rows.
        self.metrics.record_listing_read(&req.package_id.registry);

        Ok(doc)
    }

    /// One package's blocked versions, normalised for its protocol, **failing
    /// open**.
    ///
    /// A repository error logs a warning and returns an empty set, matching
    /// `BlockListRule` and the local path's `filter_blocked`: a database blip
    /// should degrade to showing more versions than intended, never to
    /// reporting every package as empty. The download gate re-checks the
    /// concrete coordinate on every request and denies as soon as the store
    /// recovers, so no failure mode here makes blocked *bytes* retrievable.
    ///
    /// Public because JetBrains Marketplace renders three listing documents
    /// from one intermediate version list rather than from a fetched document,
    /// so its handler filters at that chokepoint instead of going through
    /// [`Self::version_document`].
    pub async fn blocked_versions_for(
        &self,
        registry: &str,
        package: &str,
        kind: crate::entities::RegistryKind,
    ) -> crate::services::blocking::BlockedVersions {
        let mut versions = self
            .repo
            .blocked_versions(registry, package)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    registry = %registry,
                    package = %package,
                    error = %e,
                    "failed to load blocked versions for listing, failing open"
                );
                Vec::new()
            });
        versions.extend(self.held_versions_for(registry, package).await);
        crate::services::blocking::BlockedVersions::new(kind, versions)
    }

    /// The versions a security verdict keeps out of the listings (RFC 0018
    /// §4.2 *Listings*), hidden **by the same mechanism as a block** — so
    /// cargo marks them `yanked`, conda drops them from the channel summary,
    /// and every per-registry caveat of RFC 0006 applies unchanged.
    ///
    /// Empty for a registry without `[security]`, and empty on a store error:
    /// a listing fails open like a block does, because the download gate
    /// re-checks the concrete coordinate on every request and no failure here
    /// makes held bytes retrievable.
    async fn held_versions_for(&self, registry: &str, package: &str) -> Vec<String> {
        let (verdicts, mode) = {
            let hot = self.hot.read().await;
            let Some(policy) = hot.security.get(registry) else {
                return Vec::new();
            };
            (hot.verdicts.clone(), policy.mode)
        };
        let Some(verdicts) = verdicts else {
            return Vec::new();
        };
        let now = chrono::Utc::now();
        // Judged under the registry's *current* mode, not the one the row
        // was written under: a `denied` from before a flip to `warn` must
        // not keep hiding a version the gate would now serve (see
        // `Verdict::hides_from_listings_under`).
        match verdicts.list_for_package(registry, package).await {
            Ok(rows) => rows
                .into_iter()
                .filter(|v| v.hides_from_listings_under(now, mode))
                .map(|v| v.package.version)
                .collect(),
            Err(e) => {
                tracing::warn!(
                    registry = %registry,
                    package = %package,
                    error = %e,
                    "failed to load verdicts for listing, failing open"
                );
                Vec::new()
            }
        }
    }

    /// The blocked `(package, version)` set for a whole registry, behind a
    /// short-lived snapshot.
    ///
    /// **The one place in this design where a block is not effective on the very
    /// next request.** Every other path reads the blocked set through on each
    /// request; this one cannot, because a multi-package index —
    /// `repodata.json` for a busy conda channel is tens of megabytes — is
    /// fetched on every `conda install`, and re-querying per request would put
    /// the whole channel's block list on that path.
    ///
    /// Thirty seconds is short enough that nobody waits on it during an
    /// incident and long enough to collapse a burst of installs into one query.
    /// The delay is documented in the admin guide rather than left to be
    /// discovered, because an undocumented delay is indistinguishable from a
    /// block that did not work.
    ///
    /// The snapshot lives in the metadata cache rather than in a process-local
    /// map, so a Redis-backed deployment shares one query across replicas
    /// instead of one per replica.
    /// The fingerprint of this registry's current blocked-set snapshot.
    ///
    /// For a caller that caches something *derived* from a filtered
    /// multi-package document — conda's compressed `repodata.json` — and needs
    /// the derived entry to change when the blocks do. Reads the same snapshot
    /// the filter uses, so the two cannot disagree about what is blocked.
    /// The registry-wide blocked set, for callers outside the listing path.
    ///
    /// Search needs it: a result page names many packages, so a per-package
    /// query would be one query per hit.
    pub async fn blocked_in_registry_snapshot_public(
        &self,
        registry: &str,
        kind: crate::entities::RegistryKind,
    ) -> crate::services::blocking::MultiPackageBlocks {
        self.blocked_in_registry_snapshot(registry, kind).await
    }

    pub async fn blocked_snapshot_fingerprint(
        &self,
        registry: &str,
        kind: crate::entities::RegistryKind,
    ) -> String {
        self.blocked_in_registry_snapshot(registry, kind)
            .await
            .fingerprint()
    }

    async fn blocked_in_registry_snapshot(
        &self,
        registry: &str,
        kind: crate::entities::RegistryKind,
    ) -> crate::services::blocking::MultiPackageBlocks {
        const SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(30);
        let key = format!("blocks:{registry}");

        if let Ok(Some(entry)) = self.cache.get(&key).await {
            if let Ok(pairs) = serde_json::from_value::<Vec<(String, String)>>(entry.metadata.extra)
            {
                return crate::services::blocking::MultiPackageBlocks::new(kind, pairs);
            }
        }

        let pairs = match self.repo.blocked_in_registry(registry).await {
            Ok(p) => p,
            Err(e) => {
                // Fail open, as everywhere else on this path.
                tracing::warn!(
                    registry = %registry,
                    error = %e,
                    "failed to load the registry's blocked set, serving the index unfiltered"
                );
                return crate::services::blocking::MultiPackageBlocks::new(kind, Vec::new());
            }
        };

        let entry = crate::ports::CacheEntry {
            metadata: crate::entities::PackageMetadata {
                id: crate::entities::PackageId::new(registry, "__blocks__", "__snapshot__"),
                published_at: None,
                download_url: None,
                checksum: None,
                is_signed: None,
                extra: serde_json::to_value(&pairs).unwrap_or(serde_json::Value::Null),
                cache_control: None,
            },
            cached_at: chrono::Utc::now(),
            expires_at: None,
        };
        if let Err(e) = self.cache.set(&key, entry, Some(SNAPSHOT_TTL)).await {
            tracing::warn!(key = %key, error = %e, "caching the blocked-set snapshot failed");
        }

        crate::services::blocking::MultiPackageBlocks::new(kind, pairs)
    }

    /// Serve a **multi-package** index — conda's `repodata.json` — with blocked
    /// packages removed.
    ///
    /// The sibling of [`Self::version_document`] for the listings that describe
    /// a whole channel rather than one package. Same authorisation, same audit
    /// treatment, same fail-open; what differs is the shape of the blocked set
    /// (see [`Self::blocked_in_registry_snapshot`]) and therefore its freshness.
    pub async fn multi_package_document(
        &self,
        req: &ProxyRequest,
        doc_kind: DocumentKind,
        public_base: &str,
    ) -> Result<VersionDocument, CoreError> {
        let prelude = self.request_prelude(req).await?;
        self.authorize_listing_audited(req, "denied multi-package index")
            .await?;

        let name = req.package_id.name.as_str();
        let mut doc = self
            .cached_version_document(&prelude, req, name, doc_kind)
            .await?;

        let kind = prelude
            .client
            .registry_type()
            .parse()
            .unwrap_or(crate::entities::RegistryKind::Generic);
        let ctx = crate::services::blocking::ListingContext {
            registry: &req.package_id.registry,
            kind,
            document: doc_kind,
            package: name,
            public_base,
        };

        let blocked = self
            .blocked_in_registry_snapshot(&req.package_id.registry, kind)
            .await;
        crate::services::blocking::dispatch_multi(&ctx, &mut doc, &blocked);

        self.metrics.record_listing_read(&req.package_id.registry);
        Ok(doc)
    }

    /// The upstream version document, from the metadata cache when fresh.
    ///
    /// What is cached is the document **as the upstream sent it** — before
    /// blocks are applied and before tarball URLs are rewritten. Both of those
    /// must vary per request, and caching them would be wrong in two distinct
    /// ways:
    ///
    /// - A cached *filtered* document would keep serving a version for the rest
    ///   of the TTL after an operator blocked it. Blocks have to take effect on
    ///   the next request, not eventually.
    /// - A cached *rewritten* document would pin one ingress. The same registry
    ///   is reachable at `npm.acme.io` and at `hub.example.com/proxy/npm1`, and
    ///   whichever host warmed the cache would hand its own URLs to clients of
    ///   the other.
    ///
    /// On an upstream failure a stale entry is served when the registry's policy
    /// allows it, matching `resolve_metadata_cached`: an upstream outage should
    /// degrade to slightly old version lists, not to a broken registry.
    /// `pub(super)` so the console's discovery read can reuse the three rungs
    /// rather than inventing a second cache policy for the same document
    /// (RFC 0007 §5.5). Still not public: nothing outside `ProxyService` gets
    /// to fetch a listing document without going through a path that gates it.
    pub(super) async fn cached_version_document(
        &self,
        prelude: &RequestPrelude,
        req: &ProxyRequest,
        name: &str,
        doc_kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        // Distinct from the `meta:` namespace: that key holds a `PackageMetadata`
        // for one version, this holds a whole package's upstream document.
        //
        // `doc_kind` is part of the key because a registry can have more than
        // one listing for the same name — NuGet's flat index and its
        // registration page, RubyGems' versions list and its gem document. Keyed
        // by name alone they collide, and one is served under the other's URL.
        let key = format!(
            "doc:{}:{}:{}",
            req.package_id.registry,
            doc_kind.as_str(),
            name
        );

        // `get` returns only entries the store still considers fresh, so freshness
        // is the store's job here exactly as it is in `resolve_metadata_cached` —
        // hence `expires_at: None` below rather than a second, independently
        // clocked expiry that could disagree with the backing store's own.
        if let Ok(Some(entry)) = self.cache.get(&key).await {
            if let Some(doc) = decode_cached_document(&key, entry.metadata.extra) {
                return Ok(doc);
            }
        }

        match prelude.client.fetch_version_document(name, doc_kind).await {
            Ok(doc) => {
                let encoded = serde_json::to_value(&doc).unwrap_or(serde_json::Value::Null);
                let entry = crate::ports::CacheEntry {
                    metadata: crate::entities::PackageMetadata {
                        id: req.package_id.clone(),
                        published_at: None,
                        download_url: None,
                        checksum: None,
                        is_signed: None,
                        extra: encoded,
                        cache_control: None,
                    },
                    cached_at: chrono::Utc::now(),
                    expires_at: None,
                };
                if let Err(e) = self.cache.set(&key, entry, prelude.ttl).await {
                    tracing::warn!(key = %key, error = %e, "caching version document failed");
                }
                Ok(doc)
            }
            Err(e) => {
                let serve_stale = prelude
                    .policy
                    .as_ref()
                    .map(|p| p.serve_stale_metadata)
                    .unwrap_or(false);
                if serve_stale {
                    if let Ok(Some(stale)) = self.cache.get_stale(&key).await {
                        if let Some(doc) = decode_cached_document(&key, stale.metadata.extra) {
                            tracing::warn!(
                                key = %key,
                                error = %e,
                                "upstream version document unavailable, serving stale"
                            );
                            return Ok(doc);
                        }
                    }
                }
                Err(e)
            }
        }
    }
}

/// Read a cached [`VersionDocument`] back out of the metadata cache's untyped
/// `extra` field.
///
/// `None` on anything that does not deserialize, which is treated as a miss.
/// The realistic cause is an entry written by an older build under a key shape
/// this one reuses; refetching is cheap and correct, where trusting a partially
/// understood document is neither.
fn decode_cached_document(key: &str, extra: serde_json::Value) -> Option<VersionDocument> {
    match serde_json::from_value(extra) {
        Ok(doc) => Some(doc),
        Err(e) => {
            tracing::debug!(key = %key, error = %e, "cached version document unreadable, refetching");
            None
        }
    }
}
