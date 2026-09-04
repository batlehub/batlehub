//! Hiding administratively blocked versions from version *listings*.
//!
//! Blocking a package version has always denied its download
//! ([`crate::rules::BlockListRule`]), but the listings a client resolves
//! against — an npm packument, a NuGet flat index, a `maven-metadata.xml` — kept
//! advertising it. A resolver reading those listings still picks the blocked
//! version for `latest` or for a range like `^4.17.0`, and only then discovers
//! it cannot have it. The install fails instead of quietly resolving to the
//! newest version the operator does allow: the block reads as breakage rather
//! than as policy.
//!
//! So a block has two halves — the download gate, and this: leaving the version
//! out of what a client is told exists. A direct request for a blocked version
//! still gets its `403` and the operator's stated reason. **Hiding governs
//! resolution; it does not replace diagnosis.**
//!
//! # Shape of this module
//!
//! One file per protocol, each exporting pure functions over a document. The
//! documents are not one shape — npm and NuGet are JSON, `maven-metadata.xml`
//! is XML, a PyPI simple page is HTML, cargo's sparse index is NDJSON — so the
//! thing that generalises is the **dispatch**, not the filter. [`dispatch`] is
//! the single call site: a filter that is a pure function reached from exactly
//! one place cannot be forgotten at a call site the way a rewrite scattered
//! across twenty handlers can.
//!
//! Which protocols are covered, and why the rest are not, is
//! [`RegistryKind::listing_filter`] — an exhaustive match, so a new registry
//! kind does not compile until it answers the question.
//!
//! # Everything fails open
//!
//! A repository error while loading the blocked set logs a warning and serves
//! the *unfiltered* listing, matching `BlockListRule` and the local path's
//! `filter_blocked`. A database blip degrades to showing more versions than
//! intended, never to reporting every package as empty. The same rule applies
//! to a document this proxy cannot parse: it is passed through unchanged and
//! warned about, never partially rewritten. No failure mode of this path makes
//! blocked *bytes* retrievable — the download gate re-checks the concrete
//! coordinate on every request.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::entities::RegistryKind;
use crate::ports::{DocumentKind, VersionDocument};

pub mod cargo;
pub mod composer;
pub mod conda;
pub mod forge;
pub mod goproxy;
pub mod maven;
pub mod nodedist;
pub mod npm;
pub mod nuget;
pub mod pypi;
pub mod rubygems;
pub mod sdkman;
pub mod terraform;

/// Everything a filter needs to know about the request it is filtering for.
///
/// A struct rather than six positional parameters: `registry` and `package` are
/// both `&str` and transposing them would produce a metrics label that reads
/// plausibly and is wrong.
#[derive(Debug, Clone, Copy)]
pub struct ListingContext<'a> {
    /// The configured registry *instance* name (`"npm1"`), for logs and
    /// metrics — not the protocol.
    pub registry: &'a str,
    /// The protocol, which selects the filter.
    pub kind: RegistryKind,
    /// Which of this protocol's listing documents this is.
    pub document: DocumentKind,
    /// The package the document describes. Meaningless for the multi-package
    /// indexes (conda's `repodata.json` and friends), which pass `""`.
    pub package: &'a str,
    /// This registry's public proxy base URL, for rewriting download URLs back
    /// at this proxy.
    pub public_base: &'a str,
}

/// The blocked versions of one package, pre-normalised for comparison.
///
/// Version strings are not comparable across protocols as literal text. NuGet
/// folds `1.0.0.0` to `1.0.0`, PEP 440 normalises `1.0.0-RC1` to `1.0rc1`, Go
/// carries a `v` prefix and `+incompatible` suffixes. A block recorded in one
/// spelling and compared against a listing in another silently hides nothing,
/// and nothing else in the system reports that it did not work — which is why
/// this type also carries the tripwire in [`Self::spelling_may_have_missed`].
#[derive(Debug, Clone)]
pub struct BlockedVersions {
    kind: RegistryKind,
    normalized: HashSet<String>,
    /// At least one entry's normalised form differs from what the operator
    /// recorded, so normalisation is actually load-bearing for this set.
    renormalised: bool,
}

impl BlockedVersions {
    pub fn new(kind: RegistryKind, versions: Vec<String>) -> Self {
        let mut normalized = HashSet::with_capacity(versions.len());
        let mut renormalised = false;
        for v in versions {
            let n = normalize(kind, &v);
            renormalised |= n != v;
            normalized.insert(n.into_owned());
        }
        Self {
            kind,
            normalized,
            renormalised,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    pub fn len(&self) -> usize {
        self.normalized.len()
    }

    /// Whether `version`, as the *document* spells it, is blocked.
    pub fn contains(&self, version: &str) -> bool {
        self.normalized
            .contains(normalize(self.kind, version).as_ref())
    }

    /// The tripwire of §4.4: this set was non-empty, it matched nothing, and
    /// normalisation was in play — so "the operator's spelling never matched"
    /// is a live explanation rather than "upstream simply does not serve it".
    ///
    /// Narrower than "matched nothing" on purpose. A block on a version the
    /// upstream genuinely never had is normal and must not warn on every
    /// request for that package; a block whose recorded spelling had to be
    /// rewritten to be comparable, and still matched nothing, is the failure
    /// mode with no other symptom.
    fn spelling_may_have_missed(&self, removed: &[String]) -> bool {
        removed.is_empty() && !self.is_empty() && self.renormalised
    }
}

/// Every blocked `(package, version)` in one registry, pre-normalised.
///
/// The counterpart of [`BlockedVersions`] for the listings that describe
/// **many packages at once** — conda's `repodata.json` is the only one today.
/// A per-package query is the wrong shape there: filtering a channel's repodata
/// one package at a time would be a query per package in the document.
///
/// Package names are compared verbatim; only versions are normalised, because
/// name canonicalisation is per-ecosystem (PyPI folds `_` to `-`, conda does
/// not) and getting it wrong in the *widening* direction would hide a package
/// nobody blocked.
#[derive(Debug, Clone)]
pub struct MultiPackageBlocks {
    kind: RegistryKind,
    /// Package name → its blocked, normalised versions.
    ///
    /// Nested rather than a set of pairs so [`Self::contains`] probes with
    /// borrowed keys. A flat `HashSet<(String, String)>` can only be probed by
    /// constructing the tuple, which would be two allocations per entry of a
    /// `repodata.json` that runs to tens of thousands of them.
    blocked: std::collections::HashMap<String, HashSet<String>>,
    len: usize,
}

impl MultiPackageBlocks {
    pub fn new(kind: RegistryKind, pairs: Vec<(String, String)>) -> Self {
        let len = pairs.len();
        let mut blocked: std::collections::HashMap<String, HashSet<String>> =
            std::collections::HashMap::new();
        for (name, version) in pairs {
            blocked
                .entry(name)
                .or_default()
                .insert(normalize(kind, &version).into_owned());
        }
        Self { kind, blocked, len }
    }

    pub fn is_empty(&self) -> bool {
        self.blocked.is_empty()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this exact package at this version is blocked.
    pub fn contains(&self, name: &str, version: &str) -> bool {
        self.blocked
            .get(name)
            .is_some_and(|versions| versions.contains(normalize(self.kind, version).as_ref()))
    }

    /// A stable digest of exactly which `(package, version)` pairs are blocked.
    ///
    /// For caching something *derived from a filtered document* — conda's
    /// compressed `repodata.json`, where recompressing tens of megabytes per
    /// request is not affordable but serving a copy filtered under an older
    /// block list would re-open the hole RFC 0006 closed.
    ///
    /// Keying the derived artifact by this makes a block change produce a
    /// different key, so the stale entry is never read rather than being
    /// evicted — which is what lets the cache be correct without a TTL race.
    ///
    /// Sorted before hashing because `HashMap` iteration order is not stable
    /// across processes, and two replicas must agree on the key or they cache
    /// the same bytes twice.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut pairs: Vec<(&str, &str)> = self
            .blocked
            .iter()
            .flat_map(|(name, versions)| versions.iter().map(move |v| (name.as_str(), v.as_str())))
            .collect();
        pairs.sort_unstable();

        let mut h = Sha256::new();
        for (name, version) in pairs {
            h.update(name.as_bytes());
            h.update(b"\0");
            h.update(version.as_bytes());
            h.update(b"\0");
        }
        hex::encode(&h.finalize()[..16])
    }
}

/// Remove every blocked package from a multi-package index.
///
/// The [`dispatch`] of the multi-package world; separate because the blocked
/// set has a different shape, not because the protocols do.
pub fn dispatch_multi(
    ctx: &ListingContext<'_>,
    doc: &mut VersionDocument,
    blocked: &MultiPackageBlocks,
) -> Vec<String> {
    if blocked.is_empty() {
        return Vec::new();
    }
    let removed = match ctx.kind {
        RegistryKind::Conda => match ctx.document {
            // `channeldata.json` summarises a channel by *package*, with a
            // `version` field naming the newest release of each. Blocking that
            // release has to move the summary onto one that is still allowed,
            // exactly as RubyGems' gem document does — not drop the package,
            // which would tell `conda search` it does not exist.
            DocumentKind::CHANNELDATA => {
                with_json(doc, |json| conda::strip_channeldata(json, blocked))
            }
            _ => with_json(doc, |json| conda::strip_repodata(json, blocked)),
        },

        // RubyGems' compact index `/versions` is a whole-registry document for
        // the same reason `repodata.json` is — one line per gem — so it takes
        // the same entry point. `/names` is *not* here: it lists gem names and
        // no versions, so it names nothing to hide (RFC 0009 §7.3).
        RegistryKind::Rubygems => match ctx.document {
            DocumentKind::COMPACT_VERSIONS => {
                with_text(doc, |text| rubygems::strip_compact_versions(text, blocked))
            }
            _ => Vec::new(),
        },

        other => {
            tracing::warn!(
                kind = %other,
                "no multi-package listing filter for this registry kind"
            );
            Vec::new()
        }
    };

    if !removed.is_empty() {
        tracing::debug!(
            registry = %ctx.registry,
            kind = %ctx.kind,
            document = %ctx.document,
            removed = removed.len(),
            "hid blocked packages from a multi-package index"
        );
        metrics::counter!(
            "listing_versions_hidden_total",
            "registry" => ctx.registry.to_owned(),
            "kind" => ctx.kind.as_str(),
            // A third label beyond RFC 0006 §4.4's `{registry,kind}`: a
            // registry with two listing documents (NuGet's flat index and its
            // registration pages, RubyGems' two APIs) would otherwise report
            // them as one series, and "which document did the block reach" is
            // the question an operator asks when only one of them looks right.
            "document" => ctx.document.as_str(),
        )
        .increment(removed.len() as u64);
    }
    removed
}

/// Remove every blocked version from `doc`, in the way `ctx.kind`'s protocol
/// requires, and repair whatever that protocol calls "newest".
///
/// Returns the versions actually removed, as the *document* spelled them.
///
/// This is the only place a proxied listing is filtered. `ProxyService::
/// version_document` is in turn the only path by which a proxied listing
/// reaches a client — anything that short-circuits it (a `proxy_stream` on a
/// listing coordinate, a bare upstream GET) is a hole, not a shortcut.
pub fn dispatch(
    ctx: &ListingContext<'_>,
    doc: &mut VersionDocument,
    blocked: &BlockedVersions,
) -> Vec<String> {
    // Short-circuit before parsing anything. The filters cost time proportional
    // to document size rather than to the number of blocks, and a deployment
    // with no blocks at all — the overwhelmingly common case — should not pay
    // for a walk of every `repodata.json` it proxies.
    if blocked.is_empty() {
        return Vec::new();
    }
    let removed = strip(ctx, doc, blocked).unwrap_or_default();

    if !removed.is_empty() {
        tracing::debug!(
            registry = %ctx.registry,
            kind = %ctx.kind,
            document = %ctx.document,
            package = %ctx.package,
            removed = removed.len(),
            "hid blocked versions from version listing"
        );
        // Filtering is invisible when it works, which is exactly when an
        // operator wants evidence that it did. A counter answers "did the block
        // take effect" without turning on debug logging on a production proxy.
        metrics::counter!(
            "listing_versions_hidden_total",
            "registry" => ctx.registry.to_owned(),
            "kind" => ctx.kind.as_str(),
            // A third label beyond RFC 0006 §4.4's `{registry,kind}`: a
            // registry with two listing documents (NuGet's flat index and its
            // registration pages, RubyGems' two APIs) would otherwise report
            // them as one series, and "which document did the block reach" is
            // the question an operator asks when only one of them looks right.
            "document" => ctx.document.as_str(),
        )
        .increment(removed.len() as u64);
    } else if blocked.spelling_may_have_missed(&removed) {
        tracing::warn!(
            registry = %ctx.registry,
            kind = %ctx.kind,
            document = %ctx.document,
            package = %ctx.package,
            blocked = blocked.len(),
            "blocked versions matched nothing in this listing after version \
             normalisation; the recorded spelling may not match the upstream's"
        );
    }

    removed
}

/// The protocol switch behind [`dispatch`], without the logging.
///
/// `None` means **this kind has no listing filter** — a signed deb index, a
/// RubyGems Marshal blob, `generic`'s absent listing. Distinct from
/// `Some(vec![])`, which means the filter ran and found nothing to remove.
/// `every_advertised_filter_is_reachable_from_dispatch` holds the two apart
/// against [`RegistryKind::listing_filter`], so the admin guide's generated
/// table cannot promise filtering this function declines to do.
///
/// Exhaustive over [`RegistryKind`] with no wildcard arm: a new registry kind
/// does not compile until someone decides what its listings do, the same way
/// `server/src/builders.rs` already forces a decision about client
/// construction.
fn strip(
    ctx: &ListingContext<'_>,
    doc: &mut VersionDocument,
    blocked: &BlockedVersions,
) -> Option<Vec<String>> {
    match ctx.kind {
        RegistryKind::Npm => Some(with_json(doc, |json| npm::strip_packument(json, blocked))),

        RegistryKind::Nuget => Some(match ctx.document {
            DocumentKind::REGISTRATION => with_json(doc, |json| {
                let (removed, saw_paged) = nuget::strip_registration(json, blocked);
                if saw_paged {
                    tracing::warn!(
                        registry = %ctx.registry,
                        package = %ctx.package,
                        "NuGet registration has paged items; those pages are served \
                         unfiltered. The flat index, which is what resolves the version, \
                         is filtered either way"
                    );
                }
                removed
            }),
            _ => with_json(doc, |json| nuget::strip_flat_index(json, blocked)),
        }),

        RegistryKind::Terraform => Some(with_json(doc, |json| {
            terraform::strip_versions(json, blocked)
        })),

        RegistryKind::Rubygems => Some(match ctx.document {
            // The gem document names exactly one version and has no list to
            // pick a replacement from, so repairing it needs the versions API
            // as well. That composition belongs to the handler, which has both;
            // here the document passes through untouched.
            DocumentKind::GEM => Vec::new(),
            // The compact index's per-gem document — what Bundler reads once
            // `/versions` has told it which gems to look at (RFC 0009 §7.3).
            DocumentKind::COMPACT_INFO => {
                with_text(doc, |text| rubygems::strip_compact_info(text, blocked))
            }
            // `/versions` is whole-registry and goes through `dispatch_multi`;
            // `/names` names no version. Neither reaches here, and both are
            // `Vec::new()` rather than a fall-through to the versions-API
            // filter, which would try to parse plain text as JSON.
            DocumentKind::COMPACT_VERSIONS | DocumentKind::COMPACT_NAMES => Vec::new(),
            _ => with_json(doc, |json| rubygems::strip_versions(json, blocked)),
        }),

        RegistryKind::Goproxy => Some(match ctx.document {
            // Same shape of problem as RubyGems' gem document: `@latest` names
            // one version and carries no list. `goproxy::repaired_latest` does
            // the repair from the filtered `@v/list`, in the handler that has
            // both documents.
            DocumentKind::LATEST => Vec::new(),
            _ => with_text(doc, |text| goproxy::strip_version_list(text, blocked)),
        }),

        RegistryKind::Maven => Some(with_text(doc, |xml| maven::strip_metadata(xml, blocked))),

        RegistryKind::Composer => Some(with_json(doc, |json| composer::strip_p2(json, blocked))),

        // `repodata.json` describes a whole channel, so its blocked set is a
        // registry's worth rather than a package's: it goes through
        // `dispatch_multi`, not here. `Some(vec![])` rather than `None` because
        // the kind *is* filtered — just on the other entry point.
        RegistryKind::Conda => Some(Vec::new()),

        // Three APIs, one document shape. Forgejo is GitHub-compatible here and
        // GitLab uses the same `tag_name` field.
        RegistryKind::Github | RegistryKind::Forgejo | RegistryKind::Gitlab => {
            Some(with_json(doc, |json| forge::strip_releases(json, blocked)))
        }

        // The one protocol that marks rather than removes: `yanked` is cargo's
        // own "exists, do not select", and keeps an existing lockfile's
        // diagnostics honest where a deleted line would not.
        RegistryKind::Cargo => Some(with_text(doc, |body| cargo::mark_yanked(body, blocked))),

        RegistryKind::Pypi => Some(match ctx.document {
            DocumentKind::SIMPLE_JSON => {
                with_json(doc, |json| pypi::strip_simple_json(json, blocked))
            }
            _ => with_text(doc, |html| pypi::strip_simple_html(html, blocked)),
        }),

        // Two encodings of the one release table (RFC 0010 §6.2). `index.tab`
        // is what nvm resolves every install through; `index.json` is the same
        // rows for fnm and mise. `SHASUMS256.txt` never comes here: it is an
        // artifact, signed by a sibling, and not rewritten.
        RegistryKind::Nodedist => Some(match ctx.document {
            DocumentKind::INDEX_JSON => {
                with_json(doc, |json| nodedist::strip_index_json(json, blocked))
            }
            _ => with_text(doc, |text| nodedist::strip_index_tab(text, blocked)),
        }),

        // Three text documents (RFC 0010 §6.2). `candidates/default` names one
        // version and carries no list, so — like Go's `@latest` — it is
        // repaired in the handler against the filtered `versions/all`. The
        // relayed documents (hooks, healthcheck, `candidates/all`, …) name no
        // version, or are bash the client runs, and pass through untouched.
        RegistryKind::Sdkman => Some(match ctx.document {
            DocumentKind::SDKMAN_DEFAULT | DocumentKind::RELAYED => Vec::new(),
            DocumentKind::SDKMAN_VERSIONS_LIST => {
                with_text(doc, |text| sdkman::strip_rendered_list(text, blocked))
            }
            _ => with_text(doc, |text| sdkman::strip_versions_csv(text, blocked)),
        }),

        // No listing document, one that must not be rewritten, or one filtered
        // at a handler chokepoint instead (see `FILTERED_ELSEWHERE`). The
        // reasons are recorded once, in `listing_filter()`.
        RegistryKind::Openvsx
        | RegistryKind::VscodeMarketplace
        | RegistryKind::Deb
        | RegistryKind::Rpm
        | RegistryKind::Pacman
        | RegistryKind::Jetbrains
        | RegistryKind::JetbrainsMarketplace
        | RegistryKind::Generic => None,
    }
}

/// Run a JSON filter, or pass the document through if it is not JSON.
///
/// A body in an encoding its filter does not understand is a bug in the
/// adapter, not in the document — and the safe direction is over-listing.
fn with_json<F>(doc: &mut VersionDocument, f: F) -> Vec<String>
where
    F: FnOnce(&mut serde_json::Value) -> Vec<String>,
{
    match doc.body.as_json_mut() {
        Some(json) => f(json),
        None => {
            tracing::warn!("listing filter expected a JSON document and got text; passing through");
            Vec::new()
        }
    }
}

/// [`with_json`] for the protocols whose listing is text — Go's `@v/list`,
/// `maven-metadata.xml`, a PyPI simple page, cargo's NDJSON.
fn with_text<F>(doc: &mut VersionDocument, f: F) -> Vec<String>
where
    F: FnOnce(&mut String) -> Vec<String>,
{
    match doc.body.as_text_mut() {
        Some(text) => f(text),
        None => {
            tracing::warn!("listing filter expected a text document and got JSON; passing through");
            Vec::new()
        }
    }
}

/// Rewrite a listing's download URLs to point back at this proxy.
///
/// Separate from [`dispatch`] because it happens whether or not anything was
/// blocked: served unrewritten, the upstream document routes every download
/// around the proxy — past its cache, its audit trail, and the download-time
/// gate that is the block's other half.
pub fn rewrite_urls(ctx: &ListingContext<'_>, doc: &mut VersionDocument) {
    match ctx.kind {
        RegistryKind::Npm => {
            if let Some(json) = doc.body.as_json_mut() {
                npm::rewrite_tarball_urls(json, ctx.public_base, ctx.package);
            }
        }
        RegistryKind::Composer => {
            if let Some(json) = doc.body.as_json_mut() {
                composer::rewrite_dist_urls(json, ctx.public_base);
            }
        }
        // RFC 0019 §4.2 *API reads* — a forge release document advertises
        // absolute upstream download URLs, and a client that follows them
        // goes past the proxy entirely.
        kind if kind.is_forge() => {
            if let Some(json) = doc.body.as_json_mut() {
                forge::rewrite_release_urls(json, ctx.public_base, ctx.package);
            }
        }
        _ => {
            // Most protocols address downloads by a path the client builds
            // itself from the version list, so there is nothing in the document
            // to repoint.
        }
    }
}

/// The version a protocol's "newest" field should name: highest stable, or
/// highest overall when every survivor is a pre-release.
///
/// Protocol-neutral, because every protocol that has such a field wants the
/// same answer — npm's `dist-tags.latest`, Maven's `<release>`, Go's `@latest`,
/// a RubyGems gem document. Ordering is semver-aware, with a lexicographic
/// fallback for the versions these registries accept but semver does not parse;
/// those only win when they are all there is.
pub fn best_latest(versions: &[String]) -> Option<String> {
    let parsed: Vec<(Option<semver::Version>, &String)> = versions
        .iter()
        .map(|v| {
            (
                semver::Version::parse(v.strip_prefix('v').unwrap_or(v)).ok(),
                v,
            )
        })
        .collect();

    let stable_max = parsed
        .iter()
        .filter(|(sv, _)| sv.as_ref().is_some_and(|s| s.pre.is_empty()))
        .max_by(|a, b| a.0.as_ref().unwrap().cmp(b.0.as_ref().unwrap()))
        .map(|(_, raw)| (*raw).clone());
    if stable_max.is_some() {
        return stable_max;
    }

    let semver_max = parsed
        .iter()
        .filter(|(sv, _)| sv.is_some())
        .max_by(|a, b| a.0.as_ref().unwrap().cmp(b.0.as_ref().unwrap()))
        .map(|(_, raw)| (*raw).clone());
    if semver_max.is_some() {
        return semver_max;
    }

    versions.iter().max().cloned()
}

/// A version string in the one spelling this protocol's blocked set and its
/// listings can be compared in.
///
/// Not a canonical form for display or storage — a comparison key. The only
/// property that matters is that two strings the protocol considers the *same
/// version* normalise to the same output, applied to both sides.
///
/// Identity for the protocols where it is not yet needed (npm, Maven), with the
/// arm present rather than absent so the decision is recorded rather than
/// assumed. npm's semver spelling is near-canonical already, and Maven's
/// qualifier rules are involved enough that guessing at them would be worse
/// than the honest identity this documents.
pub fn normalize(kind: RegistryKind, version: &str) -> Cow<'_, str> {
    match kind {
        RegistryKind::Nuget => normalize_nuget(version),
        RegistryKind::Pypi => normalize_pep440(version),
        RegistryKind::Goproxy => normalize_go(version),
        // A forge "version" is a *tag*, and the same release is `1.2.3` in one
        // repository and `v1.2.3` in the next. Which one a repository uses is a
        // habit rather than a convention, so a block must not depend on whose
        // habit the operator happened to copy.
        //
        // Node's tree spells every version `v22.11.0` and every client accepts
        // `22.11.0`: `nvm install 22.11.0` is the documented form, so that is
        // the spelling an operator copies into a block.
        RegistryKind::Github
        | RegistryKind::Forgejo
        | RegistryKind::Gitlab
        | RegistryKind::Nodedist => {
            let v = version.trim();
            Cow::Borrowed(v.strip_prefix('v').unwrap_or(v))
        }
        _ => Cow::Borrowed(version),
    }
}

/// NuGet folds a version to three or four numeric components with leading zeros
/// stripped, and lowercases the pre-release tag: `1.0.0.0`, `1.00.0` and `1.0`
/// are all `1.0.0`, and `1.0.0-RC1` is `1.0.0-rc1`.
///
/// Build metadata (`+sha`) is dropped: NuGet ignores it when comparing
/// versions, so a block recorded with one build must hide a listing with
/// another.
fn normalize_nuget(version: &str) -> Cow<'_, str> {
    let v = version.trim();
    let v = v.split('+').next().unwrap_or(v);
    let (release, pre) = match v.split_once('-') {
        Some((r, p)) => (r, Some(p.to_ascii_lowercase())),
        None => (v, None),
    };

    let mut parts: Vec<String> = release
        .split('.')
        .map(|p| {
            let trimmed = p.trim_start_matches('0');
            if trimmed.is_empty() && !p.is_empty() {
                "0".to_owned()
            } else {
                trimmed.to_owned()
            }
        })
        .collect();
    // A fourth component of zero is not part of the identity; a non-zero one is.
    if parts.len() == 4 && parts[3] == "0" {
        parts.pop();
    }
    while parts.len() < 3 {
        parts.push("0".to_owned());
    }

    let mut out = parts.join(".");
    if let Some(p) = pre {
        out.push('-');
        out.push_str(&p);
    }
    Cow::Owned(out)
}

/// Go module versions carry a `v` prefix, and modules without a `go.mod` carry
/// a `+incompatible` suffix that names the same release either way.
fn normalize_go(version: &str) -> Cow<'_, str> {
    let v = version.trim();
    let v = v.strip_suffix("+incompatible").unwrap_or(v);
    Cow::Borrowed(v.strip_prefix('v').unwrap_or(v))
}

/// PEP 440, as a *comparison* key rather than a canonical rendering.
///
/// The one deliberate departure from PEP 440's canonical form: trailing zero
/// components of the release segment are trimmed, so `1.0.0` and `1.0` compare
/// equal. PEP 440 says they *are* equal — it zero-pads the shorter one — and a
/// key that renders them differently would leave a block recorded as `1.0`
/// silently failing to hide a wheel listed as `1.0.0`. Two components is the
/// floor, so `1.0.0-RC1` and `1.0rc1` both land on `1.0rc1`.
fn normalize_pep440(version: &str) -> Cow<'_, str> {
    let lower = version.trim().to_ascii_lowercase();
    let mut s = lower.as_str();
    s = s.strip_prefix('v').unwrap_or(s);

    // Local version identifier: kept (it distinguishes real artifacts), with
    // its separators folded to `.` as PEP 440 requires.
    let (s, local) = match s.split_once('+') {
        Some((head, tail)) => (head, Some(tail.replace(['-', '_'], "."))),
        None => (s, None),
    };
    // `0!` is the implicit epoch and is not written.
    let (epoch, s) = match s.split_once('!') {
        Some((e, rest)) if e.trim_start_matches('0').is_empty() => (None, rest),
        Some((e, rest)) => (Some(e.trim_start_matches('0').to_owned()), rest),
        None => (None, s),
    };

    let release_end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (release, suffix) = s.split_at(release_end);

    let mut parts: Vec<&str> = release
        .split('.')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let t = p.trim_start_matches('0');
            if t.is_empty() {
                "0"
            } else {
                t
            }
        })
        .collect();
    while parts.len() > 2 && parts.last() == Some(&"0") {
        parts.pop();
    }
    while parts.len() < 2 {
        parts.push("0");
    }

    let mut out = String::new();
    if let Some(e) = epoch {
        out.push_str(&e);
        out.push('!');
    }
    out.push_str(&parts.join("."));
    out.push_str(&normalize_pep440_suffix(suffix));
    if let Some(l) = local {
        out.push('+');
        out.push_str(&l);
    }
    Cow::Owned(out)
}

/// The pre/post/dev tail of a PEP 440 version: spellings folded to their
/// canonical token, separators dropped, an implicit number written as `0`.
fn normalize_pep440_suffix(suffix: &str) -> String {
    // Split into (word, digits) runs, ignoring the `.`/`-`/`_` separators PEP
    // 440 allows anywhere between them.
    let cleaned: String = suffix.chars().filter(|c| !"-_. ".contains(*c)).collect();
    let mut out = String::new();
    let mut rest = cleaned.as_str();
    while !rest.is_empty() {
        let word_end = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        let (word, tail) = rest.split_at(word_end);
        let num_end = tail
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(tail.len());
        let (num, next) = tail.split_at(num_end);
        let num = num.trim_start_matches('0');
        let num = if num.is_empty() { "0" } else { num };

        match word {
            "a" | "alpha" => out.push_str(&format!("a{num}")),
            "b" | "beta" => out.push_str(&format!("b{num}")),
            "c" | "rc" | "pre" | "preview" => out.push_str(&format!("rc{num}")),
            "post" | "rev" | "r" => out.push_str(&format!(".post{num}")),
            "dev" => out.push_str(&format!(".dev{num}")),
            // Anything this proxy does not recognise is preserved verbatim
            // rather than dropped: over-listing is the safe direction, and a
            // silently mangled suffix would compare equal to versions it is not.
            other => out.push_str(&format!("{other}{num}")),
        }
        rest = next;
    }
    out
}

/// Percent-encode a package name so it survives as **one** path segment.
///
/// The download route is `/proxy/{registry}/{package}/{version}/tarball`, and an
/// actix path segment never spans `/`. A scoped npm name interpolated raw turns
/// `@vue/cli` into two segments, so the URL has one more segment than the
/// pattern and 404s — every scoped dependency in the document becomes
/// undownloadable. `host_routing` already relies on `%2f` keeping scoped names
/// whole on the way in; this is the same requirement on the way out.
pub(crate) fn encode_package_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(kind: RegistryKind, v: &str) -> String {
        normalize(kind, v).into_owned()
    }

    /// Registry kinds that *are* filtered, but not through [`strip`] — so
    /// `every_advertised_filter_is_reachable_from_dispatch` has to skip them.
    ///
    /// Both entries earn the exemption rather than deferring the work:
    ///
    /// - **conda** filters through [`dispatch_multi`]. `repodata.json`
    ///   describes a whole channel, so its blocked set is a registry's worth
    ///   rather than a package's — a different entry point, not a missing one.
    /// - **JetBrains Marketplace** renders three listing documents
    ///   (`updatePlugins.xml`, `/plugins/list`, `/api/plugins/{id}/updates`)
    ///   from one intermediate version list, so its handler filters at that
    ///   chokepoint. Filtering three rendered documents instead would be three
    ///   chances to forget one; the local path already does the same thing in
    ///   `load_visible_versions`.
    /// - **OpenVSX and VS Code Marketplace** filter in
    ///   `handlers/proxy/vsx/source.rs`, for two reasons `strip` cannot serve.
    ///   The gallery response is selected by a POST *body* rather than by a
    ///   URL, so `strip`'s `(kind, document, package)` signature cannot address
    ///   it; and the same entries render into two different client protocols
    ///   (`extensionquery` and the OpenVSX REST API), so filtering the entries
    ///   rather than the documents is what keeps them in agreement.
    const FILTERED_ELSEWHERE: &[RegistryKind] = &[
        RegistryKind::Conda,
        RegistryKind::JetbrainsMarketplace,
        RegistryKind::Openvsx,
        RegistryKind::VscodeMarketplace,
    ];

    /// Exemptions at *document* granularity, for a kind that is otherwise
    /// filtered through `strip`.
    ///
    /// `FILTERED_ELSEWHERE` above exempts a whole kind, which is too blunt for
    /// RubyGems: its JSON APIs go through `strip` and must keep being checked,
    /// while two of its compact-index documents do not go through `strip` at
    /// all. Each entry carries the reason, so the list cannot become a place to
    /// park work.
    const DOCUMENTS_FILTERED_ELSEWHERE: &[(RegistryKind, &str, &str)] = &[(
        RegistryKind::Rubygems,
        "compact-versions",
        "whole-registry document: one line per gem, so it filters through \
             `dispatch_multi` against the registry-wide blocked set, exactly as conda's \
             `repodata.json` does",
    )];

    #[test]
    fn best_latest_prefers_stable_over_prerelease() {
        let vs = ["1.0.0".to_owned(), "2.0.0-rc.1".to_owned()];
        assert_eq!(best_latest(&vs), Some("1.0.0".to_owned()));
    }

    #[test]
    fn best_latest_tolerates_unparseable_versions() {
        let vs = ["not-semver".to_owned(), "also-not".to_owned()];
        assert_eq!(best_latest(&vs), Some("not-semver".to_owned()));
    }

    #[test]
    fn best_latest_of_nothing_is_none() {
        assert_eq!(best_latest(&[]), None);
    }

    // --- normalisation: the spellings that actually differ -------------------

    #[test]
    fn nuget_folds_a_zero_fourth_component_and_leading_zeros() {
        assert_eq!(norm(RegistryKind::Nuget, "1.0.0.0"), "1.0.0");
        assert_eq!(norm(RegistryKind::Nuget, "1.00.1"), "1.0.1");
        assert_eq!(norm(RegistryKind::Nuget, "1.0"), "1.0.0");
        assert_eq!(
            norm(RegistryKind::Nuget, "1.0.0.5"),
            "1.0.0.5",
            "a non-zero fourth component is part of the identity"
        );
    }

    #[test]
    fn nuget_lowercases_the_prerelease_and_drops_build_metadata() {
        assert_eq!(norm(RegistryKind::Nuget, "1.0.0-RC1"), "1.0.0-rc1");
        assert_eq!(norm(RegistryKind::Nuget, "1.0.0+deadbeef"), "1.0.0");
    }

    #[test]
    fn nuget_block_and_listing_in_different_spellings_compare_equal() {
        let blocked = BlockedVersions::new(RegistryKind::Nuget, vec!["1.0.0.0".to_owned()]);
        assert!(blocked.contains("1.0.0"));
        assert!(!blocked.contains("1.0.1"));
    }

    #[test]
    fn pep440_folds_case_separators_and_trailing_zeros() {
        assert_eq!(norm(RegistryKind::Pypi, "1.0.0-RC1"), "1.0rc1");
        assert_eq!(norm(RegistryKind::Pypi, "1.0rc1"), "1.0rc1");
        assert_eq!(norm(RegistryKind::Pypi, "1.0.0"), "1.0");
        assert_eq!(norm(RegistryKind::Pypi, "1"), "1.0");
        assert_eq!(norm(RegistryKind::Pypi, "v1.2.3"), "1.2.3");
    }

    #[test]
    fn pep440_folds_the_pre_post_and_dev_spellings() {
        assert_eq!(norm(RegistryKind::Pypi, "1.2.ALPHA.1"), "1.2a1");
        assert_eq!(norm(RegistryKind::Pypi, "1.2beta2"), "1.2b2");
        assert_eq!(norm(RegistryKind::Pypi, "1.2-preview3"), "1.2rc3");
        assert_eq!(norm(RegistryKind::Pypi, "1.2.post_1"), "1.2.post1");
        assert_eq!(norm(RegistryKind::Pypi, "1.2.dev0"), "1.2.dev0");
        assert_eq!(
            norm(RegistryKind::Pypi, "1.2rc"),
            "1.2rc0",
            "an implicit pre-release number is zero"
        );
    }

    #[test]
    fn pep440_keeps_epoch_and_local_version() {
        assert_eq!(norm(RegistryKind::Pypi, "1!1.0.0"), "1!1.0");
        assert_eq!(norm(RegistryKind::Pypi, "0!1.0"), "1.0");
        assert_eq!(norm(RegistryKind::Pypi, "1.0+ubuntu-1"), "1.0+ubuntu.1");
    }

    #[test]
    fn go_strips_the_v_prefix_and_incompatible_suffix() {
        assert_eq!(norm(RegistryKind::Goproxy, "v1.2.3"), "1.2.3");
        assert_eq!(norm(RegistryKind::Goproxy, "v1.2.3+incompatible"), "1.2.3");
        assert_eq!(norm(RegistryKind::Goproxy, "1.2.3"), "1.2.3");
    }

    #[test]
    fn forge_tags_compare_with_or_without_their_v_prefix() {
        assert_eq!(norm(RegistryKind::Github, "v1.2.3"), "1.2.3");
        assert_eq!(norm(RegistryKind::Gitlab, "1.2.3"), "1.2.3");
        let blocked = BlockedVersions::new(RegistryKind::Forgejo, vec!["1.2.3".to_owned()]);
        assert!(blocked.contains("v1.2.3"));
    }

    /// `index.tab` spells `v22.11.0`; `nvm install 22.11.0` is the documented
    /// form and so the one an operator copies into a block.
    #[test]
    fn nodedist_versions_compare_with_or_without_their_v_prefix() {
        assert_eq!(norm(RegistryKind::Nodedist, "v22.11.0"), "22.11.0");
        let blocked = BlockedVersions::new(RegistryKind::Nodedist, vec!["22.11.0".to_owned()]);
        assert!(blocked.contains("v22.11.0"));
    }

    #[test]
    fn npm_and_maven_normalisation_is_identity() {
        assert_eq!(norm(RegistryKind::Npm, "4.17.21"), "4.17.21");
        assert_eq!(norm(RegistryKind::Maven, "1.0-SNAPSHOT"), "1.0-SNAPSHOT");
    }

    // --- the blocked set ------------------------------------------------------

    #[test]
    fn an_empty_blocked_set_is_empty() {
        let b = BlockedVersions::new(RegistryKind::Npm, Vec::new());
        assert!(b.is_empty());
        assert!(!b.contains("1.0.0"));
    }

    /// The tripwire fires only when normalisation was load-bearing: a block on
    /// a version upstream simply never had must not warn on every request.
    #[test]
    fn tripwire_ignores_a_block_whose_spelling_was_already_canonical() {
        let b = BlockedVersions::new(RegistryKind::Nuget, vec!["9.9.9".to_owned()]);
        assert!(!b.spelling_may_have_missed(&[]));
    }

    #[test]
    fn tripwire_fires_when_a_renormalised_block_matched_nothing() {
        let b = BlockedVersions::new(RegistryKind::Nuget, vec!["1.0.0.0".to_owned()]);
        assert!(b.spelling_may_have_missed(&[]));
        assert!(
            !b.spelling_may_have_missed(&["1.0.0".to_owned()]),
            "it matched something, so the spelling was fine"
        );
    }

    // --- dispatch -------------------------------------------------------------

    fn ctx(kind: RegistryKind) -> ListingContext<'static> {
        ListingContext {
            registry: "r1",
            kind,
            document: DocumentKind::Versions,
            package: "lodash",
            public_base: "https://h/proxy/r1",
        }
    }

    #[test]
    fn dispatch_routes_npm_to_the_packument_filter() {
        let mut doc = VersionDocument::json(serde_json::json!({
            "dist-tags": { "latest": "2.0.0" },
            "versions": { "1.0.0": {}, "2.0.0": {} }
        }));
        let blocked = BlockedVersions::new(RegistryKind::Npm, vec!["2.0.0".to_owned()]);
        let removed = dispatch(&ctx(RegistryKind::Npm), &mut doc, &blocked);

        assert_eq!(removed, vec!["2.0.0".to_owned()]);
        let json = doc.body.as_json().unwrap();
        assert_eq!(json["dist-tags"]["latest"], serde_json::json!("1.0.0"));
    }

    #[test]
    fn dispatch_with_nothing_blocked_touches_nothing() {
        let mut doc = VersionDocument::json(serde_json::json!({"versions": {"1.0.0": {}}}));
        let before = doc.clone();
        let blocked = BlockedVersions::new(RegistryKind::Npm, Vec::new());

        assert!(dispatch(&ctx(RegistryKind::Npm), &mut doc, &blocked).is_empty());
        assert_eq!(doc, before);
    }

    /// A text body handed to a JSON filter is passed through, not mangled.
    #[test]
    fn dispatch_leaves_a_body_in_the_wrong_encoding_alone() {
        let mut doc = VersionDocument::text("text/xml", "<metadata/>");
        let blocked = BlockedVersions::new(RegistryKind::Npm, vec!["1.0.0".to_owned()]);

        assert!(dispatch(&ctx(RegistryKind::Npm), &mut doc, &blocked).is_empty());
        assert_eq!(doc.body.as_text(), Some("<metadata/>"));
    }

    /// The contract that keeps the admin guide honest: a kind whose
    /// `listing_filter()` advertises *any* filtered document must reach a
    /// filter in `strip`, and a kind that advertises none must not.
    ///
    /// The generated coverage table in `docs/guide/admin-policies.md` is built
    /// from `listing_filter()`. Without this test the table could promise
    /// filtering that `strip` silently declines to do, which is the exact
    /// documented-but-not-closed gap this RFC exists to remove.
    #[test]
    fn every_advertised_filter_is_reachable_from_dispatch() {
        use crate::entities::ListingSupport;
        for kind in RegistryKind::ALL {
            if FILTERED_ELSEWHERE.contains(kind) {
                continue;
            }
            let advertised = kind
                .listing_filter()
                .iter()
                .any(|d| !matches!(d.support, ListingSupport::Unsupported(_)));

            let mut doc = VersionDocument::json(serde_json::json!({}));
            let blocked = BlockedVersions::new(*kind, vec!["1.0.0".to_owned()]);
            let ctx = ListingContext {
                registry: "r1",
                kind: *kind,
                document: DocumentKind::Versions,
                package: "p",
                public_base: "https://h/proxy/r1",
            };
            let reachable = strip(&ctx, &mut doc, &blocked).is_some();

            assert_eq!(
                advertised, reachable,
                "{kind}: listing_filter() advertises {advertised} but strip() reaches a filter: {reachable}"
            );
        }
    }

    /// The same contract, per *document* rather than per kind — RFC 0009 §4.1.
    ///
    /// The test above is exhaustive over `RegistryKind` and blind to
    /// `DocumentKind`: it only ever asks about `Versions`. So a kind already
    /// answering "filtered" for its primary listing could grow a second,
    /// unfiltered document and nothing would complain. That is exactly how
    /// RubyGems' gap survived — `specs.4.8.gz` was named and marked
    /// `Unsupported`, and nothing recorded that the document Bundler actually
    /// reads was neither served nor named.
    ///
    /// This walks every `DocumentKind` each advertised `ListingDocument` claims
    /// to cover and requires `strip` to reach a filter for it.
    #[test]
    fn every_advertised_document_reaches_a_filter() {
        use crate::entities::ListingSupport;

        for kind in RegistryKind::ALL {
            if FILTERED_ELSEWHERE.contains(kind) {
                continue;
            }
            for entry in kind.listing_filter() {
                if matches!(entry.support, ListingSupport::Unsupported(_)) {
                    assert!(
                        entry.documents.is_empty(),
                        "{kind}: {:?} is Unsupported but names DocumentKinds {:?} — an \
                         unsupported row describes something `strip` never sees",
                        entry.label,
                        entry.documents
                    );
                    continue;
                }

                assert_documents_reach_a_filter(kind, entry);
            }
        }
    }

    /// Every `DocumentKind` one `listing_filter()` row claims to cover must
    /// reach a filter in `strip`.
    fn assert_documents_reach_a_filter(
        kind: &RegistryKind,
        entry: &crate::entities::ListingDocument,
    ) {
        for name in entry.documents {
            if DOCUMENTS_FILTERED_ELSEWHERE
                .iter()
                .any(|(k, d, _)| k == kind && d == name)
            {
                continue;
            }
            let document = document_kind_named(name).unwrap_or_else(|| {
                panic!(
                    "{kind}: {:?} names document kind {name:?}, which no \
                     `DocumentKind` answers to — a typo here would otherwise \
                     skip the document silently",
                    entry.label
                )
            });

            let mut doc = VersionDocument::json(serde_json::json!({}));
            let blocked = BlockedVersions::new(*kind, vec!["1.0.0".to_owned()]);
            let ctx = ListingContext {
                registry: "r1",
                kind: *kind,
                document,
                package: "p",
                public_base: "https://h/proxy/r1",
            };

            assert!(
                strip(&ctx, &mut doc, &blocked).is_some(),
                "{kind}: listing_filter() advertises {:?} as covering the {name:?} \
                 document, but strip() reaches no filter for it",
                entry.label
            );
        }
    }

    /// Resolve a `listing_filter()` discriminant back to its `DocumentKind`.
    ///
    /// The named constants plus `Versions`. Anything else is a typo, and the
    /// caller panics rather than skipping.
    fn document_kind_named(name: &str) -> Option<DocumentKind> {
        const KNOWN: &[DocumentKind] = &[
            DocumentKind::Versions,
            DocumentKind::REGISTRATION,
            DocumentKind::GEM,
            DocumentKind::LATEST,
            DocumentKind::CURRENT_REPODATA,
            DocumentKind::P2_DEV,
            DocumentKind::SIMPLE_JSON,
            DocumentKind::COMPACT_VERSIONS,
            DocumentKind::COMPACT_INFO,
            DocumentKind::COMPACT_NAMES,
            DocumentKind::INDEX_JSON,
            DocumentKind::SDKMAN_DEFAULT,
            DocumentKind::SDKMAN_VERSIONS_LIST,
            DocumentKind::RELAYED,
        ];
        KNOWN.iter().find(|k| k.as_str() == name).copied()
    }
}
