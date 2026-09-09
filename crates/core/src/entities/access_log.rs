use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::entities::{PackageId, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AccessAction {
    Download,
    ViewMetadata,
    Block,
    Unblock,
    Delete,
    /// A principal (user or group) was granted ownership of a package.
    AddOwner,
    /// A principal (user or group) had ownership of a package revoked.
    RemoveOwner,
    /// A package's visibility (public/internal/team) was changed.
    SetVisibility,
    /// An account-wide action: a user was blocked from authenticating.
    BlockUser,
    /// An account-wide action: a previously blocked user was unblocked.
    UnblockUser,
    /// A network-wide action: an IP address was blocked.
    BlockIp,
    /// A network-wide action: a previously blocked IP address was unblocked.
    UnblockIp,
    /// The access-audit trail itself was purged up to a cutoff timestamp.
    AuditPurge,
    /// A local/hybrid-mode version was yanked (hidden from install, still resolvable by exact pin).
    Yank,
    /// A previously yanked version was restored.
    Unyank,
    /// A local/hybrid-mode version was flagged deprecated.
    Deprecate,
    /// A deprecation was reversed.
    Undeprecate,
    /// A local/hybrid-mode version was hidden from registry-protocol listings.
    Unlist,
    /// An unlisted version was made visible in listings again.
    Relist,
    /// A principal was added to a registry's beta channel.
    AddBetaMember,
    /// A principal was removed from a registry's beta channel.
    RemoveBetaMember,
    /// A team namespace was claimed by a principal.
    ClaimNamespace,
    /// A previously claimed team namespace was released.
    ReleaseNamespace,
    /// A user's publish/download quota usage was reset by an admin.
    ResetQuota,
    /// A registry's aged-out tombstone detail was stripped (RFC 0016 §4.5).
    ///
    /// An action in its own right rather than a flavour of [`Self::Delete`],
    /// following the precedent [`Self::AuditPurge`] set: compaction is
    /// destructive to *history* while being harmless to the invariant, which is
    /// a different fact about a system than a version being deleted, and an
    /// operator reading the trail has to be able to separate the two.
    TombstoneCompact,
    /// A package- or version-tier grant was written or replaced
    /// ([RFC 0017](/rfc/0017-writing-grants-at-the-package-and-version-tiers) §7).
    ///
    /// Two actions rather than one toggle, unlike [`Self::SetRetentionPin`]:
    /// a pin's current state is readable from the version row, and a grant's is
    /// not once the row is gone. "Who could read this, and since when" is the
    /// question after an incident, and a revocation that left no event would
    /// make the trail answer it wrongly — it would show the grant being written
    /// and nothing afterwards, which reads as still held.
    GrantWrite,
    /// A package- or version-tier grant was removed.
    GrantRevoke,
    /// A version was pinned against retention, or the pin was released
    /// (RFC 0016 §4.1).
    ///
    /// One action for both directions, unlike `Yank`/`Unyank`: a pin is a toggle
    /// whose current state is readable from the version row, and two actions
    /// would make "who exempted this version from the policy" a question you
    /// answer by scanning for the newest of two event kinds rather than the
    /// newest of one.
    SetRetentionPin,
    /// A version was reclaimed by a retention *policy* rather than by a person
    /// (RFC 0016 §4.2).
    ///
    /// Not a flavour of [`Self::Delete`]: RFC 0016's goals require that "an
    /// operator reading the audit trail must be able to tell a policy
    /// reclamation from a human deletion", and until this variant existed they
    /// could not — a run is triggered by an admin's own token, so the event it
    /// left was byte-for-byte what that admin deleting the version by hand
    /// leaves. The authorization is unchanged and still `releases:delete`; only
    /// the trail distinguishes them.
    RetentionReclaim,
    /// A retention run that was allowed to write (RFC 0016 §4.2).
    ///
    /// Registry-scoped, one per run, recorded whether or not it reclaimed
    /// anything — "who ran the policy, and when" is a question the per-version
    /// [`Self::RetentionReclaim`] events cannot answer for a run that reclaimed
    /// nothing.
    RetentionRun,
    /// A retention run under `dry_run`, which wrote nothing.
    ///
    /// A separate action rather than a flag on [`Self::RetentionRun`], because
    /// the `AccessEvent` shape has nowhere to carry a flag and because the
    /// distinction is the first thing an auditor asks of a retention event: an
    /// operator filtering `?action=retention_run` must see exactly the runs
    /// that *could* have deleted something. A dry run is still recorded — it is
    /// an operator's action against a registry, not a read of package data,
    /// which is the line `compact_tombstone_detail` draws when it declines to
    /// audit its own preview.
    RetentionDryRun,
    /// One proxy-cached artifact was dropped by hand.
    ///
    /// A *cache* eviction, never a [`Self::Delete`]: what goes is a copy of
    /// something the upstream still has, and the next request re-fetches it.
    /// Conflating the two would make "was this package deleted" answerable only
    /// by reading the registry's mode out of a config file.
    CacheEvict,
    /// A whole registry's cached artifacts were dropped by hand.
    ///
    /// Registry-scoped, one event: `clear-cache` is a `delete_by_prefix` that
    /// reports a count and never knew the coordinates, so there is nothing
    /// per-artifact to record even in principle. It is also the bluntest of the
    /// four `cache:evict` surfaces, which is why it gets its own action rather
    /// than looking like a single-artifact drop in the trail.
    CacheClear,
    /// A configured eviction sweep ran and was allowed to write.
    ///
    /// Registry-scoped, one per run. Deliberately **not** one event per evicted
    /// artifact, unlike [`Self::RetentionReclaim`]: an LRU sweep on a large
    /// estate evicts by the thousand, the copy it drops is recoverable by a
    /// re-fetch, and a trail nobody finishes reading protects nobody. What went
    /// is in the run's report and its log line; that it ran, and who ran it, is
    /// here.
    CacheEvictRun,
    /// An eviction sweep under `dry_run`, which dropped nothing.
    CacheEvictDryRun,
    /// A cache-coherence sweep ran and was allowed to write.
    ///
    /// Not a flavour of [`Self::CacheEvictRun`], for the reason
    /// [`Self::TombstoneCompact`] is not a flavour of [`Self::Delete`]: eviction
    /// discards blobs a policy decided it no longer wants, coherence deletes
    /// blobs **nothing references at all** — a leak being collected, not a cache
    /// being trimmed. An operator reading the trail has to be able to separate
    /// "the policy took it" from "it was already unreachable".
    CacheCoherenceRun,
    /// A cache-coherence sweep under `dry_run`, which deleted nothing and — the
    /// part that matters — did not advance any blob toward deletion either.
    CacheCoherenceDryRun,
}

impl AccessAction {
    /// Every action, in declaration order.
    ///
    /// The parsing table: [`Self::from_wire`] searches it, so a variant missing
    /// here is a variant the audit-log `?action=` filter cannot name.
    pub const ALL: &[AccessAction] = &[
        Self::Download,
        Self::ViewMetadata,
        Self::Block,
        Self::Unblock,
        Self::Delete,
        Self::AddOwner,
        Self::RemoveOwner,
        Self::SetVisibility,
        Self::BlockUser,
        Self::UnblockUser,
        Self::BlockIp,
        Self::UnblockIp,
        Self::AuditPurge,
        Self::Yank,
        Self::Unyank,
        Self::Deprecate,
        Self::Undeprecate,
        Self::Unlist,
        Self::Relist,
        Self::AddBetaMember,
        Self::RemoveBetaMember,
        Self::ClaimNamespace,
        Self::ReleaseNamespace,
        Self::ResetQuota,
        Self::GrantWrite,
        Self::GrantRevoke,
        Self::TombstoneCompact,
        Self::SetRetentionPin,
        Self::RetentionReclaim,
        Self::RetentionRun,
        Self::RetentionDryRun,
        Self::CacheEvict,
        Self::CacheClear,
        Self::CacheEvictRun,
        Self::CacheEvictDryRun,
        Self::CacheCoherenceRun,
        Self::CacheCoherenceDryRun,
    ];

    /// The canonical wire name, snake_case.
    ///
    /// What the `access_events.action` column has always stored, and what the
    /// package-detail timeline renders. Defined here rather than in the
    /// Postgres adapter because the audit-log filter has to parse the same
    /// vocabulary the adapter writes, and two tables in two crates is how the
    /// two spellings diverge.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::ViewMetadata => "view_metadata",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::Delete => "delete",
            Self::AddOwner => "add_owner",
            Self::RemoveOwner => "remove_owner",
            Self::SetVisibility => "set_visibility",
            Self::BlockUser => "block_user",
            Self::UnblockUser => "unblock_user",
            Self::BlockIp => "block_ip",
            Self::UnblockIp => "unblock_ip",
            Self::AuditPurge => "audit_purge",
            Self::Yank => "yank",
            Self::Unyank => "unyank",
            Self::Deprecate => "deprecate",
            Self::Undeprecate => "undeprecate",
            Self::Unlist => "unlist",
            Self::Relist => "relist",
            Self::AddBetaMember => "add_beta_member",
            Self::RemoveBetaMember => "remove_beta_member",
            Self::ClaimNamespace => "claim_namespace",
            Self::ReleaseNamespace => "release_namespace",
            Self::ResetQuota => "reset_quota",
            Self::GrantWrite => "grant_write",
            Self::GrantRevoke => "grant_revoke",
            Self::TombstoneCompact => "tombstone_compact",
            Self::SetRetentionPin => "set_retention_pin",
            Self::RetentionReclaim => "retention_reclaim",
            Self::RetentionRun => "retention_run",
            Self::RetentionDryRun => "retention_dry_run",
            Self::CacheEvict => "cache_evict",
            Self::CacheClear => "cache_clear",
            Self::CacheEvictRun => "cache_evict_run",
            Self::CacheEvictDryRun => "cache_evict_dry_run",
            Self::CacheCoherenceRun => "cache_coherence_run",
            Self::CacheCoherenceDryRun => "cache_coherence_dry_run",
        }
    }

    /// Parse a wire name, tolerating **both** spellings this API emits.
    ///
    /// The same action is on the wire twice: `access_events.action` and the
    /// package-detail timeline spell it snake_case (`view_metadata`), while the
    /// audit-log JSON serialises this enum through serde's
    /// `rename_all = "lowercase"` and spells it `viewmetadata`. An operator
    /// filtering by an action they just read out of a response would otherwise
    /// be right half the time, so separators and case are normalised away
    /// before matching rather than one spelling being declared the winner.
    ///
    /// (Unifying the two is a breaking change to the generated TypeScript
    /// client's enum, so it is not made here.)
    pub fn from_wire(s: &str) -> Option<Self> {
        fn normalise(s: &str) -> String {
            s.chars()
                .filter(|c| *c != '_' && *c != '-')
                .flat_map(char::to_lowercase)
                .collect()
        }
        let want = normalise(s);
        Self::ALL
            .iter()
            .find(|a| normalise(a.as_str()) == want)
            .copied()
    }
}

impl std::fmt::Display for AccessAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum AccessResult {
    Allowed,
    Denied {
        reason: String,
    },
    #[serde(rename = "error")]
    ProxyError {
        reason: String,
    },
}

impl AccessResult {
    pub fn is_denied(&self) -> bool {
        matches!(self, AccessResult::Denied { .. })
    }
}

/// Where a request came from, as the audit trail records it.
///
/// The proxy path has carried both fields for as long as the trail has existed,
/// threaded through `ProxyRequest`. The local read path built its event from the
/// `Identity` alone, so `audit pulls` reported an empty `source_ip` and
/// `client_user_agent` on exactly the deployments that publish their own
/// packages (RFC 0018 §13.10). Both paths now carry this.
///
/// A named pair rather than two more positional `Option<String>` parameters: it
/// travels through four signatures of the local read path, and two adjacent
/// `Option<String>` arguments are two a caller can transpose without the
/// compiler ever noticing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerNet {
    /// The caller's address, as the proxy-trust rules resolved it.
    pub ip: Option<String>,
    /// The caller's `User-Agent` header.
    pub user_agent: Option<String>,
}

impl CallerNet {
    /// No request behind the call: a scheduled task, an internal read, a test.
    ///
    /// Spelled out rather than left to `Default::default()` at the call site,
    /// because "we did not record this" and "there was nothing to record" read
    /// identically in the trail and only one of them is a bug.
    pub fn unknown() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessEvent {
    pub id: Uuid,
    pub user_id: Option<String>,
    pub user_role: Role,
    /// The package coordinate this event is about, when applicable.
    ///
    /// `None` for account-wide/network-wide admin actions that are not scoped
    /// to a specific package (e.g. blocking a user or an IP address).
    pub package_id: Option<PackageId>,
    pub action: AccessAction,
    pub result: AccessResult,
    pub timestamp: DateTime<Utc>,
    /// Caller's IP address (from X-Forwarded-For / RemoteAddr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    /// HTTP User-Agent from the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

impl AccessEvent {
    pub fn allowed_download(
        package_id: PackageId,
        user_id: Option<String>,
        user_role: Role,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            user_role,
            package_id: Some(package_id),
            action: AccessAction::Download,
            result: AccessResult::Allowed,
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        }
    }

    /// An allowed read of something that is *about* an artifact rather than the
    /// artifact — the counterpart of [`AccessEvent::denied_metadata`].
    ///
    /// Its one caller today is the checksum/signature sidecar split described on
    /// [`PackageId::is_verification_sidecar`]: the fetch is recorded, so the
    /// audit trail stays complete, but it does not count as a download.
    pub fn allowed_metadata(
        package_id: PackageId,
        user_id: Option<String>,
        user_role: Role,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            user_role,
            package_id: Some(package_id),
            action: AccessAction::ViewMetadata,
            result: AccessResult::Allowed,
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        }
    }

    /// [`AccessEvent::allowed_download`], or [`AccessEvent::allowed_metadata`]
    /// when the coordinate names a checksum or signature sidecar.
    ///
    /// The two recording sites — `ProxyService::handle` and
    /// `LocalRegistryService::record_download` — must agree on this, or a
    /// hybrid registry's download counts would depend on whether the artifact
    /// happened to be published locally. Hence one function rather than the
    /// same `if` written twice.
    pub fn allowed_read(package_id: PackageId, user_id: Option<String>, user_role: Role) -> Self {
        if package_id.is_verification_sidecar() {
            Self::allowed_metadata(package_id, user_id, user_role)
        } else {
            Self::allowed_download(package_id, user_id, user_role)
        }
    }

    pub fn denied_download(
        package_id: PackageId,
        user_id: Option<String>,
        user_role: Role,
        reason: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            user_role,
            package_id: Some(package_id),
            action: AccessAction::Download,
            result: AccessResult::Denied { reason },
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        }
    }

    /// A refused *listing* — a version document a client was not allowed to
    /// read.
    ///
    /// [`AccessAction::ViewMetadata`] rather than `Download`, because nothing
    /// was downloaded. Recording a listing as a download puts rows in the audit
    /// trail that transferred no bytes, which reads as traffic that never
    /// happened when an incident asks what an identity actually pulled.
    ///
    /// There is deliberately no `allowed_metadata` counterpart: an allowed
    /// listing is counted in `ProxyMetrics` and rolled up hourly, not filed per
    /// request. A `cargo build` over a 400-crate graph is 400 listings, and one
    /// row each would drown the trail in the least interesting of the three
    /// questions it answers. Denials are few and each one matters, so they
    /// stay rows.
    pub fn denied_metadata(
        package_id: PackageId,
        user_id: Option<String>,
        user_role: Role,
        reason: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            user_role,
            package_id: Some(package_id),
            action: AccessAction::ViewMetadata,
            result: AccessResult::Denied { reason },
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        }
    }

    pub fn proxy_error(
        package_id: PackageId,
        user_id: Option<String>,
        user_role: Role,
        reason: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            user_role,
            package_id: Some(package_id),
            action: AccessAction::Download,
            result: AccessResult::ProxyError { reason },
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        }
    }

    /// Builder: attach the caller's IP address and User-Agent to this event.
    pub fn with_ip_ua(mut self, ip: Option<String>, ua: Option<String>) -> Self {
        self.ip_address = ip;
        self.user_agent = ua;
        self
    }
}

/// Filter for querying access events.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub registry: Option<String>,
    pub package_name: Option<String>,
    pub user_id: Option<String>,
    /// Keep only these actions. Empty means every action, so the default filter
    /// is unchanged.
    ///
    /// A set rather than one action: the questions an operator asks are about a
    /// *kind* of activity — "every deletion" is `delete` and
    /// [`AccessAction::RetentionReclaim`] both, and being unable to ask for the
    /// two together is what made the retention split unusable when it was
    /// proposed.
    pub actions: Vec<AccessAction>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub denied_only: bool,
    pub limit: u64,
    pub offset: u64,
}

impl EventFilter {
    pub fn new() -> Self {
        Self {
            limit: 100,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{PackageId, Role};

    fn pkg() -> PackageId {
        PackageId::new("cargo", "tokio", "1.0.0")
    }

    #[test]
    fn is_denied_only_for_denied_variant() {
        assert!(!AccessResult::Allowed.is_denied());
        assert!(AccessResult::Denied {
            reason: "blocked".into()
        }
        .is_denied());
        assert!(!AccessResult::ProxyError {
            reason: "timeout".into()
        }
        .is_denied());
    }

    #[test]
    fn allowed_download_sets_correct_fields() {
        let ev = AccessEvent::allowed_download(pkg(), Some("alice".into()), Role::User);
        assert!(matches!(ev.result, AccessResult::Allowed));
        assert!(matches!(ev.action, AccessAction::Download));
        assert_eq!(ev.user_id.as_deref(), Some("alice"));
        assert_eq!(ev.user_role, Role::User);
    }

    #[test]
    fn denied_download_sets_reason() {
        let ev = AccessEvent::denied_download(pkg(), None, Role::Anonymous, "blocklisted".into());
        assert!(matches!(&ev.result, AccessResult::Denied { reason } if reason == "blocklisted"));
    }

    /// A listing that transferred no bytes must not look like a download in the
    /// trail.
    #[test]
    fn denied_metadata_records_a_view_not_a_download() {
        let ev = AccessEvent::denied_metadata(pkg(), Some("bob".into()), Role::User, "rbac".into());
        assert!(matches!(ev.action, AccessAction::ViewMetadata));
        assert!(matches!(&ev.result, AccessResult::Denied { reason } if reason == "rbac"));
        assert_eq!(ev.user_id.as_deref(), Some("bob"));
    }

    #[test]
    fn proxy_error_sets_reason() {
        let ev = AccessEvent::proxy_error(pkg(), None, Role::Anonymous, "upstream timeout".into());
        assert!(
            matches!(&ev.result, AccessResult::ProxyError { reason } if reason == "upstream timeout")
        );
    }

    #[test]
    fn event_filter_new_default_limit() {
        let f = EventFilter::new();
        assert_eq!(f.limit, 100);
        assert_eq!(f.offset, 0);
        assert!(!f.denied_only);
        assert!(f.registry.is_none());
    }

    /// The default filter must keep every action, or adding the field silently
    /// empties every existing caller's result.
    #[test]
    fn event_filter_new_selects_every_action() {
        assert!(EventFilter::new().actions.is_empty());
    }

    #[test]
    fn from_wire_round_trips_every_variant() {
        for action in AccessAction::ALL {
            assert_eq!(AccessAction::from_wire(action.as_str()), Some(*action));
        }
    }

    /// `as_str` is an exhaustive match, so a new variant cannot compile without
    /// a name — but it *can* compile without being in `ALL`, which would leave
    /// it unparseable by the audit-log filter. This is the gate for that.
    #[test]
    fn all_lists_every_variant() {
        assert_eq!(
            AccessAction::ALL.len(),
            37,
            "a new AccessAction variant must be added to ALL (and this count bumped), \
             or ?action= cannot name it"
        );
        let mut names: Vec<&str> = AccessAction::ALL.iter().map(|a| a.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two actions share a wire name");
    }

    /// Both spellings the API emits parse: the snake_case one the database and
    /// the package-detail timeline use, and the squashed lowercase one serde
    /// puts in the audit-log JSON.
    #[test]
    fn from_wire_accepts_both_spellings_on_the_wire() {
        let squashed = serde_json::to_string(&AccessAction::ViewMetadata).unwrap();
        assert_eq!(squashed, "\"viewmetadata\"");
        assert_eq!(
            AccessAction::from_wire("viewmetadata"),
            Some(AccessAction::ViewMetadata)
        );
        assert_eq!(
            AccessAction::from_wire("view_metadata"),
            Some(AccessAction::ViewMetadata)
        );
        assert_eq!(
            AccessAction::from_wire("View-Metadata"),
            Some(AccessAction::ViewMetadata)
        );
    }

    #[test]
    fn from_wire_rejects_unknown() {
        assert_eq!(AccessAction::from_wire("not_an_action"), None);
        assert_eq!(AccessAction::from_wire(""), None);
    }

    /// A retention reclamation and a hand deletion must not be the same event
    /// (RFC 0016 §3).
    #[test]
    fn retention_reclaim_is_not_delete() {
        assert_ne!(AccessAction::RetentionReclaim, AccessAction::Delete);
        assert_ne!(
            AccessAction::RetentionReclaim.as_str(),
            AccessAction::Delete.as_str()
        );
    }

    /// Dropping a cached copy is not deleting a package, and an auditor must
    /// not have to read a registry's mode out of a config file to tell them
    /// apart.
    #[test]
    fn a_cache_eviction_is_not_a_deletion() {
        for evict in [
            AccessAction::CacheEvict,
            AccessAction::CacheClear,
            AccessAction::CacheEvictRun,
        ] {
            assert_ne!(evict, AccessAction::Delete);
            assert_ne!(evict, AccessAction::RetentionReclaim);
        }
    }
}
