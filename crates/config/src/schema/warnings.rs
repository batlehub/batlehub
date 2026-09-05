//! Non-fatal configuration problems.
//!
//! Some config states are wrong enough to be worth telling an operator about but
//! not wrong enough to refuse to start: a registry name that cannot become a DNS
//! label, a deprecated key that is being shadowed, a security setting left at its
//! permissive default. A `tracing::warn!` at startup is not good enough for any
//! of them — they are noticed months later, when a hostname mysteriously does not
//! resolve to a registry.
//!
//! [`ConfigWarning`] is the machine-readable form of one such problem, produced
//! by [`AppConfig::warnings`](super::AppConfig::warnings). Warnings are logged at
//! startup *and* on every reload, and served from
//! `GET /api/v1/admin/config/warnings` so the admin UI can render them.

use serde::Serialize;
use utoipa::ToSchema;

/// One non-fatal configuration problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConfigWarning {
    /// Stable slug identifying the *kind* of problem, e.g.
    /// `"subdomain.invalid-dns-label"`. Safe to match on; the `message` is not.
    pub code: String,
    /// Human-readable explanation, including what the server did instead.
    pub message: String,
    /// Where in the config the problem is, in a form that can be searched for in
    /// the TOML: `"server.trusted_proxies"`, `"registries[3].name"`.
    pub path: String,
}

impl ConfigWarning {
    pub fn new(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: path.into(),
        }
    }
}

// ── Warning codes ─────────────────────────────────────────────────────────────

/// Both `[server].trusted_proxies` and the deprecated
/// `[ip_blocking].trusted_proxies` carry a list; `[server]` wins.
pub const PROXY_TRUST_SHADOWED_DEPRECATED_KEY: &str = "proxy-trust.shadowed-deprecated-key";

/// No trusted-proxy list anywhere, and no host-based routing to force the issue.
/// Forwarded host and scheme are believed unconditionally — today's default, but
/// worth stating explicitly.
pub const PROXY_TRUST_UNCONFIGURED: &str = "proxy-trust.unconfigured";

/// Host-based routing is configured and the trust policy comes only from the
/// deprecated `[ip_blocking].trusted_proxies`.
pub const PROXY_TRUST_DEPRECATED_KEY_ONLY: &str = "proxy-trust.deprecated-key-only";

/// An entry of the deprecated `[ip_blocking].trusted_proxies` is not an IP or a
/// CIDR range. It was silently dropped before this key gained a validator, so it
/// is still dropped (with this warning) rather than refusing to start.
pub const PROXY_TRUST_INVALID_DEPRECATED_ENTRY: &str = "proxy-trust.invalid-deprecated-entry";

/// `[subdomain_routing]` is enabled but a registry name is not a valid DNS
/// label, so no wildcard host is derived for it.
pub const SUBDOMAIN_INVALID_DNS_LABEL: &str = "subdomain.invalid-dns-label";

/// `[server].cors_allowed_origins` contains `"*"`, so any website may issue
/// cross-origin requests to this server and read the responses. Legitimate for a
/// public mirror, rarely what an internal deployment wants — and since 1.1.0 it
/// only happens when someone wrote it down, which is the point of the warning.
pub const CORS_ANY_ORIGIN: &str = "cors.any-origin";

/// A registry has `signed_downloads = true` *and* still grants the anonymous
/// read that signing exists to remove. Legal — belt and braces — but almost
/// certainly a migration someone stopped halfway: the signed URLs are minted
/// and verified, and the registry is open to everyone anyway, so nothing is
/// actually closed (RFC 0012 §7).
pub const SIGNED_URLS_ANONYMOUS_STILL_GRANTED: &str = "signed-urls.anonymous-still-granted";

/// An `sdkman` registry's `upstreams` entry does not end in `/2`. SDKMAN
/// versions its candidates API in the path; the URL is served as given, but a
/// missing version segment is more likely a typo than a choice (RFC 0010 §4.5).
pub const SDKMAN_UPSTREAM_WITHOUT_API_VERSION: &str = "sdkman.upstream-without-api-version";

/// `[server.signed_urls]` is configured and no registry sets
/// `signed_downloads = true`, so the secret signs nothing. Harmless, and worth
/// saying: it is the shape of a feature enabled on the wrong side.
pub const SIGNED_URLS_UNUSED: &str = "signed-urls.unused";

/// A `license_gate` rule is configured on a registry whose type has no manifest
/// parser, so the licence of every version is permanently unknown and the gate
/// can never observe what it claims to govern.
///
/// This is the warning RFC 0004-bis §13.1 owes: licence extraction covers five
/// of the twenty-one registry types, and on the other sixteen a `license_gate`
/// with the default `allow_unknown = true` never fires. An operator who wrote
/// an allow list and saw no errors would reasonably believe they had a licence
/// policy. They have an inert rule.
pub const LICENSE_GATE_NO_EXTRACTOR: &str = "license-gate.no-extractor";

/// A `license_gate` on a registry whose `[registries.sbom]` block is absent or
/// `enabled = false`.
///
/// The licence is recorded as a side effect of SBOM generation
/// (`ProxyService::maybe_trigger_sbom` returns early when SBOM is off for the
/// registry), so with SBOM disabled *nothing is ever extracted* and the gate
/// sees an unknown licence for every version — no matter how good the parser
/// for that registry type is. Found by running the thing rather than reading
/// it: the parser worked, the rule was loaded, and no licence was ever stored.
pub const LICENSE_GATE_SBOM_DISABLED: &str = "license-gate.sbom-disabled";

/// `require_signed_release` is configured on a registry that also *publishes*,
/// while `[registries.signing] required` is not set.
///
/// The rule reads `PackageMetadata::is_signed`. For a proxied artifact the
/// adapter fills that in (a `.asc` asset on GitHub, a signature blob in an
/// OpenVSX extension), and a type with no such signal leaves it `None`, which
/// the rule skips by default. A **locally published** row is different: it
/// reports `Some(false)` whenever it carries no signature bytes, and
/// `RequireSignedReleaseRule` denies `Some(false)` outright —
/// `deny_missing_signature` governs only the `None` case.
///
/// So on a hybrid registry, enabling this rule to gate the *proxied* half also
/// makes every locally published artifact `403 this release is not signed`, at
/// **download** time, for the consumer rather than the publisher. Pairing it
/// with `signing.required = true` moves that refusal to the publish request,
/// where the person who can act on it is the one who sees it.
pub const REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES: &str =
    "require-signed-release.unsigned-publishes";

/// A `[registries.readme]` block written down on a registry type that has no
/// README to give — `maven`, the source-hosting kinds, the path-addressed kinds.
///
/// Accepted and inert. Only raised when the operator wrote the block: the
/// feature is on by default, so warning about every absent block would put a
/// notice on the admin panel for every `maven` registry in every deployment,
/// which is noise rather than information. Written down, it is a belief about
/// what the server will do, and it is wrong.
pub const README_UNSUPPORTED_TYPE: &str = "readme.unsupported-type";

/// `from_archive = true` on a registry kind whose README is metadata-borne only.
///
/// The artifact is never opened for it, so the setting does nothing. Distinct
/// from [`README_UNSUPPORTED_TYPE`]: READMEs *are* stored for this registry, and
/// an operator who set this to control cost should know it was already free.
pub const README_FROM_ARCHIVE_INERT: &str = "readme.from-archive-inert";

/// `from_archive = true` on a `firewall_only` registry.
///
/// `firewall_only` streams without buffering, so no artifact is ever cached to
/// extract from. Metadata-borne sources still work; archive-borne ones never
/// will — which on an archive-only kind (cargo, NuGet, Go, …) means this
/// registry stores no README at all, however the block is written.
pub const README_FROM_ARCHIVE_FIREWALL_ONLY: &str = "readme.from-archive-firewall-only";

/// `[registries.upstream_detail]` enabled on a `local`-mode registry.
///
/// Accepted and inert: there is no upstream to ask, and the package page is
/// already complete from local rows.
pub const UPSTREAM_DETAIL_LOCAL_MODE: &str = "upstream-detail.local-mode";

/// `[registries.upstream_detail]` enabled on a kind that cannot be asked.
///
/// The path-addressed kinds have no package identity to ask about, and the
/// source-hosting kinds' release listings are not what a package page shows.
/// Accepted and inert; the detail page answers from local rows only.
pub const UPSTREAM_DETAIL_UNSUPPORTED_KIND: &str = "upstream-detail.unsupported-kind";

/// `[registries.upstream_detail]` enabled on a registry with no reachable
/// upstream configured.
///
/// Warned rather than rejected because an air-gapped estate is a supported
/// deployment (RFC 0008), and its operator should be told the setting will
/// produce one failed attempt per TTL rather than have the server refuse to
/// start.
pub const UPSTREAM_DETAIL_NO_UPSTREAM: &str = "upstream-detail.no-upstream";

/// The same missing parser, but with `allow_unknown = false` and `block = true`
/// — so instead of never firing, the gate refuses **every** download from that
/// registry. Separate code from [`LICENSE_GATE_NO_EXTRACTOR`] because the
/// consequence is the opposite one and an operator triaging an outage needs to
/// find this by name.
pub const LICENSE_GATE_DENIES_EVERYTHING: &str = "license-gate.denies-everything";

/// `[registries.retention]` has `tombstone_detail_for_days` set with
/// `dry_run = false`, so compaction will actually strip detail on the next run.
///
/// Legal, and the only way the feature ever does anything — but it is the one
/// configuration in this block that destroys something, and RFC 0016 §4.6 asks
/// for it to be said loudly on **every** reload rather than once at the edit
/// that introduced it. What is lost is a deleted version's checksum, publisher
/// and metadata; what is kept, always and with no setting that changes it, is
/// the coordinate claim.
pub const RETENTION_COMPACTION_LIVE: &str = "retention.compaction-live";

/// `[registries.retention]` will reclaim versions on its next run — `dry_run` is
/// off and at least one keep condition is set.
///
/// Legal, and the only way reclamation ever happens. Said loudly on every reload
/// because unlike cache eviction it destroys the only copy: an evicted cache
/// entry is re-fetchable from upstream, a reclaimed local version is frequently
/// the only copy in existence.
pub const RETENTION_RECLAMATION_LIVE: &str = "retention.reclamation-live";

/// `[registries.retention]` reclaims on a policy that does not consult the
/// download signal — `dry_run = false` with no `keep_if_pulled_days`.
///
/// **The mistake this feature exists to make hard** (RFC 0016 §4.6). Without it,
/// `keep_versions = 10` throws away the version half the estate is pinned to
/// because it happens to be eleventh by date, and the first anyone hears of it
/// is a build failing against a lockfile that resolved yesterday. With it,
/// whatever anyone is actually using stays, regardless of age or count.
pub const RETENTION_NO_PULL_VETO: &str = "retention.no-pull-veto";

/// `[search] readmes = true` while every registry has README capture off.
///
/// Accepted. The index will exist and stay empty, because nothing is ever stored
/// to put in it — so the search box grows an option that can only ever answer
/// "no package here says that" (RFC 0007-bis §4.5).
pub const SEARCH_READMES_NOTHING_STORED: &str = "search.readmes-nothing-stored";

// ── RFC 0015 §4.9 — tiered policy ─────────────────────────────────────────────

/// `immutable = "always"` on a node that also grants `releases:overwrite`.
///
/// Not a contradiction and not an error: §4.5 is explicit that immutability is a
/// property of the *resource* while the verb is a property of the *subject*, and
/// a replace needs both. So the grant is simply inert here — which is exactly
/// what makes it worth saying. An operator who wrote both believes one of them
/// is doing something, and the one that is not doing anything is the permission.
pub const IMMUTABLE_ALWAYS_WITH_OVERWRITE_GRANT: &str =
    "versioning.immutable-always-with-overwrite";

/// A namespace with `grants` but no `visibility`, on a registry whose default is
/// `public`.
///
/// The shape of an operator who has carefully decided *who* may reach a
/// namespace and has not noticed that its packages are readable by everyone
/// regardless — grants widen, and the audience is still the registry's. §4.5's
/// two directions, met one at a time.
pub const NAMESPACE_GRANTS_WITHOUT_VISIBILITY: &str = "namespace.grants-without-visibility";

/// `allow_prerelease = false` beside `immutable = "released"`.
///
/// Legal and inert: `released` means "a release is immutable, a pre-release may
/// be replaced", and a namespace that refuses to publish pre-releases at all can
/// never take the second branch. The operator has written `always` in two words.
pub const IMMUTABLE_RELEASED_WITHOUT_PRERELEASES: &str =
    "versioning.immutable-released-no-prereleases";

/// `prerelease_visibility` on a proxy-mode registry, which publishes nothing.
///
/// A **warning rather than a rejection**, and §4.9 spends a paragraph on why.
/// `[registries.beta_channel]` carries no mode restriction today, so a
/// proxy-mode registry with a beta-channel block starts now, and §10 rule 6
/// translates that block into exactly this setting. Rejecting would stop such an
/// instance from booting on upgrade, which is the one thing §10 forbids.
pub const PRERELEASE_VISIBILITY_PROXY_MODE: &str = "visibility.prerelease-proxy-mode";

/// `prerelease_visibility` **wider** than the `visibility` beside it.
///
/// Legal — nothing in the model forbids showing pre-releases to a wider audience
/// than releases — and almost always a typo, since the setting exists to do the
/// opposite.
pub const PRERELEASE_VISIBILITY_WIDER: &str = "visibility.prerelease-wider-than-release";

/// A node's grants are in **shadow**: they resolve, the would-have-been is
/// recorded, and nothing is refused because of them (RFC 0015 §4.7).
///
/// **The most dangerous setting in RFC 0015**, and the reason §4.7 asks for this
/// warning on *every* reload rather than once at the edit that introduced it:
/// dry run on grants means a request that would be refused is **served**. It is
/// what makes §10's migration survivable — enable the new model in shadow, watch
/// a week of real traffic, then enforce — and it is also, if forgotten, an
/// authorization bypass configured on purpose.
///
/// The expiry is named in the message because the countdown is the point. A
/// shadow that cannot be forgotten is what the required `until` buys, and a
/// warning that did not say when it lapses would be one more thing to go and
/// look up.
pub const GRANTS_IN_SHADOW: &str = "grants.shadow-active";

/// `[…versioning] dry_run = true` — a badly-named or duplicate version is
/// accepted (RFC 0015 §4.7).
///
/// Quieter than [`GRANTS_IN_SHADOW`] because the direction is different: §4.7's
/// table calls this one **mixed** rather than fail-open. Bad data lands, nothing
/// leaks. It still warns, because the operator who set it during an import is
/// the operator who forgets to unset it afterwards.
pub const VERSIONING_IN_DRY_RUN: &str = "versioning.dry-run-active";

/// `[cache_coherence] interval_secs` short enough to narrow the grace window
/// the sweep's safety rests on.
///
/// The sweep deletes a blob only if the *previous* run also saw it orphaned, and
/// the gap between the two runs is the interval. That gap is the margin a cache
/// write in flight gets: the bytes are stored, then the row is recorded, and a
/// blob caught between them looks exactly like an orphan. Milliseconds against a
/// daily sweep is not a race; milliseconds against a sweep every few seconds,
/// on a slow storage backend under load, is one.
///
/// Not an error, because a small estate with fast storage can reasonably want a
/// tighter loop — and because the fresh point-lookup before each delete is a
/// second guard. But it is the one setting here that trades away the margin on
/// data this must never delete, so it says so.
pub const COHERENCE_INTERVAL_TOO_SHORT: &str = "cache-coherence.interval-too-short";

/// A `github`, `gitlab` or `forgejo` registry with no `[registries.upstream_auth]`
/// (RFC 0019 §4.3). Anonymous GitHub is 60 requests an hour, and ref resolution
/// spends one or two per new ref.
/// `[air_gap]` is on and a registry is hybrid: its fall-through can never
/// reach upstream, so it behaves as local (RFC 0008 §4.5).
pub const AIR_GAP_HYBRID_REGISTRY: &str = "air-gap.hybrid-registry";

/// Bundle keys configured on a connected instance: legitimate — that is how
/// a bundle is staged — and worth saying they authorise imports only.
pub const AIR_GAP_KEYS_UNUSED: &str = "air-gap.keys-unused";

pub const FORGE_ANONYMOUS_UPSTREAM: &str = "forge.anonymous-upstream";

/// A forge registry serves no raw content while the setup snippet it hands
/// out rewrites the forge's raw host at it (RFC 0019 §4.1, phase 3).
pub const FORGE_RAW_DISABLED_BUT_LINKED: &str = "forge.raw-disabled-but-linked";

/// `[registries.security]` with `mode = "warn"` and no `required_scanners`:
/// nothing can ever hold a version (RFC 0018 §4.3).
pub const SECURITY_UNPROTECTED: &str = "security.unprotected";

/// A `[[flag_sources]]` entry may push `hard_block` (RFC 0002 §4.3).
pub const FLAG_SOURCE_CAN_HARD_BLOCK: &str = "flag-source.can-hard-block";

/// A `required_scanners` entry that only enriches other findings and never
/// creates one (RFC 0018 §6.3, `mlab`).
pub const SECURITY_ENRICHMENT_REQUIRED: &str = "security.enrichment-required";

/// `[registries.security]` with `hold_missing_timestamp = true` on a kind that
/// structurally has no publish date — the path-proxy family — so every version
/// is held open-ended (RFC 0018 §4.3, decision 27).
pub const SECURITY_TIMESTAMP_HOLD_UNAVAILABLE: &str = "security.timestamp-hold-unavailable";

/// `[upstream_audit]` enabled with no registry in `proxy`/`hybrid` mode
/// (RFC 0014 §4.4): a config in transition is legitimate, so a warning.
pub const UPSTREAM_AUDIT_NOTHING_TO_AUDIT: &str = "upstream-audit.nothing-to-audit";

/// `[upstream_audit]` enabled on a process without the `worker` role (RFC
/// 0014 §13): the sweep runs on a worker, and this one is not it.
pub const UPSTREAM_AUDIT_NO_WORKER: &str = "upstream-audit.no-worker-role";

/// `on_confirmed = "block"` with `retain_disappeared = false` (RFC 0014
/// §4.4): a blocked package is never read, so idle eviction deletes the
/// bytes the block was keeping.
pub const UPSTREAM_AUDIT_BLOCK_WITHOUT_HOLD: &str = "upstream-audit.block-without-hold";
