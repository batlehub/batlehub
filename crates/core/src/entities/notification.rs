use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEventType {
    PackagePublished,
    PackageYanked,
    PackageUnyanked,
    PackageDeleted,
    /// RFC 0014 §4.5: the upstream audit confirmed a cached package (or one
    /// version of it — `version` says which) is no longer at its upstream.
    PackageDisappearedUpstream,
    /// RFC 0014 §4.5: a package the audit had confirmed gone answered again.
    PackageReappearedUpstream,
    /// RFC 0014 §4.5: a sweep was voided — too many misses at once to be
    /// unpublishes. Registry-scoped: `package_name` is `"*"`.
    UpstreamUnreachable,
    /// RFC 0018 §4.2 (decision 23): a rescan moved a *served* verdict to
    /// `denied`. The admin alert — it carries the identities that pulled
    /// the version inside `pullers_window_days`, and is sent to nobody else.
    VerdictChanged,
    /// RFC 0018 §4.2: a hold lifted and the version is served; carries the
    /// identities that were refused it while it was held.
    ArtifactReleased,
}

impl NotificationEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PackagePublished => "package_published",
            Self::PackageYanked => "package_yanked",
            Self::PackageUnyanked => "package_unyanked",
            Self::PackageDeleted => "package_deleted",
            Self::PackageDisappearedUpstream => "package_disappeared_upstream",
            Self::PackageReappearedUpstream => "package_reappeared_upstream",
            Self::UpstreamUnreachable => "upstream_unreachable",
            Self::VerdictChanged => "verdict_changed",
            Self::ArtifactReleased => "artifact_released",
        }
    }

    /// Every variant, in wire order — what the console's picker and the
    /// CLI's help list, so neither enumerates the enum by hand.
    pub const ALL: &'static [Self] = &[
        Self::PackagePublished,
        Self::PackageYanked,
        Self::PackageUnyanked,
        Self::PackageDeleted,
        Self::PackageDisappearedUpstream,
        Self::PackageReappearedUpstream,
        Self::UpstreamUnreachable,
        Self::VerdictChanged,
        Self::ArtifactReleased,
    ];
}

impl std::fmt::Display for NotificationEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for NotificationEventType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "package_published" => Ok(Self::PackagePublished),
            "package_yanked" => Ok(Self::PackageYanked),
            "package_unyanked" => Ok(Self::PackageUnyanked),
            "package_deleted" => Ok(Self::PackageDeleted),
            "package_disappeared_upstream" => Ok(Self::PackageDisappearedUpstream),
            "package_reappeared_upstream" => Ok(Self::PackageReappearedUpstream),
            "upstream_unreachable" => Ok(Self::UpstreamUnreachable),
            "verdict_changed" => Ok(Self::VerdictChanged),
            "artifact_released" => Ok(Self::ArtifactReleased),
            other => Err(format!("unknown event type: {other}")),
        }
    }
}

/// A package lifecycle event that may trigger outbound notifications.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotificationEvent {
    pub id: Uuid,
    pub event_type: NotificationEventType,
    pub registry: String,
    pub package_name: String,
    pub version: Option<String>,
    /// User ID of the actor who triggered the event.
    pub actor: String,
    pub occurred_at: DateTime<Utc>,
    /// Extra ecosystem-specific metadata (e.g. checksum, tags).
    pub metadata: serde_json::Value,
}

impl NotificationEvent {
    pub fn new(
        event_type: NotificationEventType,
        registry: impl Into<String>,
        package_name: impl Into<String>,
        version: Option<String>,
        actor: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type,
            registry: registry.into(),
            package_name: package_name.into(),
            version,
            actor: actor.into(),
            occurred_at: Utc::now(),
            metadata: serde_json::Value::Object(Default::default()),
        }
    }
}

/// Admin-created subscription that routes matching events to a named channel.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotificationSubscription {
    pub id: Uuid,
    /// When `None`, matches all registries.
    pub registry: Option<String>,
    /// When `None`, matches all packages in the selected registries.
    pub package_name: Option<String>,
    /// Event types that this subscription listens for.
    pub event_types: Vec<NotificationEventType>,
    /// Must match the `name` of a channel configured in `config.toml`.
    pub channel_name: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub enabled: bool,
}

impl NotificationSubscription {
    pub fn matches(
        &self,
        registry: &str,
        package: &str,
        event_type: &NotificationEventType,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let registry_match = self.registry.as_deref().is_none_or(|r| r == registry);
        let package_match = self.package_name.as_deref().is_none_or(|p| p == package);
        let event_match = self.event_types.contains(event_type);
        registry_match && package_match && event_match
    }
}

/// A raw event received from an external system via the inbound webhook API.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InboundWebhookEvent {
    pub id: Uuid,
    pub webhook_name: String,
    pub payload: serde_json::Value,
    pub source_ip: Option<String>,
    pub received_at: DateTime<Utc>,
    /// `Some(true)` = HMAC verified, `Some(false)` = HMAC mismatch, `None` = no secret configured.
    pub signature_valid: Option<bool>,
}
