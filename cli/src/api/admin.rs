use anyhow::Result;
use batlehub_core::entities::AccessResult;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::percent_encode;
use super::BatleHubClient;

// ── Quota ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaEntry {
    pub registry: String,
    pub user_id: String,
    #[serde(rename = "bytes_published")]
    pub storage_bytes: u64,
    #[serde(rename = "packages_count")]
    pub package_count: u32,
}

// ── IP blocking ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpBlockEntry {
    pub ip: String,
    pub reason: String,
    pub blocked_at: u64,
    pub unblock_at: u64,
}

#[derive(Debug, Serialize)]
pub struct AddIpBlockRequest {
    pub ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ── Banner ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SetBannerRequest {
    pub message: String,
    pub level: String,
}

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangeEntry {
    pub id: Option<String>,
    pub triggered_by: Option<String>,
    #[serde(rename = "triggered_at")]
    pub applied_at: Option<String>,
    pub summary: Option<String>,
    pub status: Option<String>,
}

// ── Warm ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WarmRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// Upstream artifact paths for path-addressed registries (deb/rpm/jetbrains).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

// ── Release import ────────────────────────────────────────────────────────────

/// The server's `ImportRequest` (RFC 0021 §6.4).
///
/// Both fields are skipped when absent rather than sent as `null`: an absent
/// `tag` means "whatever the configuration selects", and an absent `repo` means
/// every import configured into the registry. Sending `null` would say the same
/// thing to serde and something different to a reader of the wire.
#[derive(Debug, Default, Serialize)]
pub struct ImportRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// One asset that did not import, named rather than counted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportFailureDto {
    pub tag: String,
    pub asset: String,
    pub error: String,
}

/// The server's `ImportResponse`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportReportDto {
    pub imported: usize,
    /// Assets whose version this registry already holds. Not an error: it is
    /// what makes a scheduled import free to re-run.
    pub skipped: usize,
    pub errors: usize,
    #[serde(default)]
    pub failures: Vec<ImportFailureDto>,
}

// ── Audit log ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditPackageRef {
    pub registry: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub user_id: Option<String>,
    pub package_id: Option<AuditPackageRef>,
    pub action: Option<String>,
    pub timestamp: Option<String>,
    pub result: Option<AccessResult>,
}

/// Paginated envelope returned by `GET /api/v1/admin/audit-log`, mirroring the
/// server's `AuditLogResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogResponse {
    pub items: Vec<AuditEntry>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Serialize)]
pub struct AuditQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    /// Comma-separated action names; the server rejects an unknown one rather
    /// than returning an empty page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_only: Option<bool>,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeAuditLogResponse {
    pub deleted: u64,
}

/// The server's `CoherenceResponse`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoherenceReportDto {
    pub storage_keys: usize,
    pub meta_rows: usize,
    pub orphaned_deleted: usize,
    #[serde(default)]
    pub deleted_keys: Vec<String>,
    #[serde(default)]
    pub first_seen_orphaned: usize,
    #[serde(default)]
    pub first_seen_keys: Vec<String>,
    #[serde(default)]
    pub keys_truncated: u64,
    #[serde(default)]
    pub dry_run: bool,
}

/// The server's `EvictResponse`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvictionReportDto {
    pub total: usize,
    pub evicted_ttl: usize,
    pub evicted_idle: usize,
    pub evicted_old_versions: usize,
    pub evicted_lru: usize,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub evicted_keys: Vec<String>,
    #[serde(default)]
    pub keys_truncated: u64,
    #[serde(default)]
    pub incomplete_because: Option<String>,
}

// ── Stats ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryStatsEntry {
    pub registry: String,
    pub artifact_hits: u64,
    pub artifact_misses: u64,
    /// Artifact hit rate in [0, 1], or null if no requests yet.
    pub hit_rate: Option<f64>,
    /// Total bytes cached in storage for this registry (from storage backend).
    pub cached_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateStats {
    pub artifact_hits: u64,
    pub artifact_misses: u64,
    pub hit_rate: Option<f64>,
    pub cached_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    /// When the server process started (counters reset on restart).
    pub since_startup: DateTime<Utc>,
    pub aggregate: AggregateStats,
    pub per_registry: Vec<RegistryStatsEntry>,
}

// ── Health ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryAccessInfo {
    pub roles: Vec<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentErrorEntry {
    pub timestamp: DateTime<Utc>,
    pub user_id: Option<String>,
    pub package_name: String,
    pub version: String,
    /// "denied" (blocked / RBAC) or "error" (upstream proxy failure).
    pub error_type: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryHealthEntry {
    pub registry: String,
    pub registry_type: String,
    pub package_count: i64,
    pub cached_artifact_count: i64,
    pub total_size_bytes: Option<i64>,
    pub last_pull_at: Option<DateTime<Utc>>,
    pub pulls_last_hour: i64,
    pub pulls_last_day: i64,
    pub recent_errors: Vec<RecentErrorEntry>,
    pub access: RegistryAccessInfo,
}

// ── Package visibility ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityResponse {
    pub visibility: String,
}

// ── Team namespaces ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamNamespaceEntry {
    pub registry: String,
    pub prefix: String,
    pub group_id: String,
    pub claimed_by: Option<String>,
}

// ── User blocks ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedUserEntry {
    pub user_id: String,
    pub blocked_at: DateTime<Utc>,
    pub blocked_by: String,
    pub reason: Option<String>,
}

// ── Notifications ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationChannelEntry {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // mirrors batlehub_core::entities::NotificationEventType
pub enum NotificationEventTypeEntry {
    PackagePublished,
    PackageYanked,
    PackageUnyanked,
    PackageDeleted,
    /// RFC 0014 §4.5 — the upstream audit's three.
    PackageDisappearedUpstream,
    PackageReappearedUpstream,
    UpstreamUnreachable,
}

impl std::fmt::Display for NotificationEventTypeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::PackagePublished => "package_published",
            Self::PackageYanked => "package_yanked",
            Self::PackageUnyanked => "package_unyanked",
            Self::PackageDeleted => "package_deleted",
            Self::PackageDisappearedUpstream => "package_disappeared_upstream",
            Self::PackageReappearedUpstream => "package_reappeared_upstream",
            Self::UpstreamUnreachable => "upstream_unreachable",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSubscriptionEntry {
    pub id: Uuid,
    /// `None` matches all registries.
    pub registry: Option<String>,
    /// `None` matches all packages in the selected registries.
    pub package_name: Option<String>,
    pub event_types: Vec<NotificationEventTypeEntry>,
    pub channel_name: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub enabled: bool,
}

// ── Bulk operations ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkPackageFailureEntry {
    pub name: String,
    pub version: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkPackageResult {
    pub processed: usize,
    pub succeeded: usize,
    pub failed: Vec<BulkPackageFailureEntry>,
}

// ── Config content ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigContentResponse {
    pub content: String,
    /// True when hot reload is disabled (e.g. Kubernetes ConfigMap mount).
    pub is_readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedRegistryEntry {
    pub name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReloadDiff {
    pub added_registries: Vec<String>,
    pub removed_registries: Vec<String>,
    pub changed_registries: Vec<ChangedRegistryEntry>,
    pub access_config_changed: bool,
    pub limits_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadResponse {
    pub diff: ReloadDiff,
}

// ── Access check ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SimulateAccessRequest {
    pub registry: String,
    pub package_name: String,
    pub version: String,
    pub resource_type: String,
    /// Simulated user id (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Simulated role: "anonymous", "user", or "admin". Defaults to "anonymous".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Simulated OIDC groups the identity belongs to.
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessSimulationResponse {
    /// "allow" or "deny".
    pub decision: String,
    /// Present when decision is "deny".
    pub reason: Option<String>,
    /// Name of the rule that triggered the deny, if any.
    pub rule_matched: Option<String>,
}

/// One stored grant, as `GET /grants` reports it (RFC 0017 §4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantDto {
    pub node_kind: String,
    pub node_key: String,
    pub subject: String,
    pub actions: Vec<String>,
    #[serde(default)]
    pub granted_by: Option<String>,
    /// The ownership projection's row rather than the editor's. Shown so an
    /// operator is not surprised by the `409` that editing one earns.
    #[serde(default)]
    pub from_ownership: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantListResponse {
    pub grants: Vec<GrantDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutGrantResponse {
    /// What was actually stored — `releases:*` names one verb and stores several.
    pub actions: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteGrantResponse {
    pub removed: bool,
}

impl BatleHubClient {
    // ── Quota ──────────────────────────────────────────────────────────────────

    pub async fn list_quota(&self, registry: Option<&str>) -> Result<Vec<QuotaEntry>> {
        match registry {
            Some(r) => self.get(&format!("/api/v1/admin/quota/{r}")).await,
            None => self.get("/api/v1/admin/quota").await,
        }
    }

    pub async fn reset_quota(&self, registry: &str, user_id: &str) -> Result<()> {
        self.delete(&format!("/api/v1/admin/quota/{registry}/{user_id}"))
            .await
    }

    // ── IP blocking ────────────────────────────────────────────────────────────

    pub async fn list_ip_blocks(&self) -> Result<Vec<IpBlockEntry>> {
        self.get("/api/v1/admin/ip-blocks").await
    }

    pub async fn add_ip_block(&self, ip: &str, reason: Option<&str>) -> Result<()> {
        let body = AddIpBlockRequest {
            ip: ip.to_string(),
            reason: reason.map(str::to_string),
        };
        self.post_void("/api/v1/admin/ip-blocks", &body).await
    }

    pub async fn remove_ip_block(&self, ip: &str) -> Result<()> {
        self.delete(&format!("/api/v1/admin/ip-blocks/{ip}")).await
    }

    // ── Config ─────────────────────────────────────────────────────────────────

    pub async fn config_reload(&self) -> Result<()> {
        self.post_no_body("/api/v1/admin/config/reload").await
    }

    pub async fn config_changes(&self) -> Result<Vec<ConfigChangeEntry>> {
        #[derive(Deserialize)]
        struct Wrapper {
            items: Vec<ConfigChangeEntry>,
        }
        let w: Wrapper = self.get("/api/v1/admin/config/changes").await?;
        Ok(w.items)
    }

    // ── Cache ──────────────────────────────────────────────────────────────────

    pub async fn cache_warm(
        &self,
        registry: &str,
        packages: Vec<String>,
        paths: Vec<String>,
    ) -> Result<()> {
        let body = WarmRequest { packages, paths };
        self.post_void(&format!("/api/v1/admin/registries/{registry}/warm"), &body)
            .await
    }

    pub async fn cache_clear(&self, registry: &str) -> Result<()> {
        self.post_no_body(&format!("/api/v1/admin/registries/{registry}/clear-cache"))
            .await
    }

    /// Sweep a registry's storage for blobs the artifact-meta table has no row
    /// for.
    pub async fn coherence_sweep(
        &self,
        registry: &str,
        dry_run: bool,
    ) -> Result<CoherenceReportDto> {
        self.post_no_body_json(&format!(
            "/api/v1/admin/registries/{registry}/coherence?dry_run={dry_run}"
        ))
        .await
    }

    /// Run a registry's configured eviction strategies (RFC 0016's trail, cache
    /// side).
    pub async fn evict_registry(&self, registry: &str, dry_run: bool) -> Result<EvictionReportDto> {
        self.post_no_body_json(&format!(
            "/api/v1/admin/registries/{registry}/evict?dry_run={dry_run}"
        ))
        .await
    }

    /// Run a configured release import now (RFC 0021 §6.4).
    ///
    /// `post` rather than `post_no_body_json`, which `evict_registry` next door
    /// uses: this endpoint takes a body, and `tag`/`repo` are how an operator
    /// reaches a pre-release or one repository out of several.
    pub async fn import_registry(
        &self,
        registry: &str,
        tag: Option<String>,
        repo: Option<String>,
    ) -> Result<ImportReportDto> {
        let body = ImportRequest { tag, repo };
        self.post(
            &format!("/api/v1/admin/registries/{registry}/import"),
            &body,
        )
        .await
    }

    // ── Banner ─────────────────────────────────────────────────────────────────

    pub async fn set_banner(&self, message: &str, level: &str) -> Result<()> {
        let body = SetBannerRequest {
            message: message.to_string(),
            level: level.to_string(),
        };
        self.put("/api/v1/admin/banner", &body).await
    }

    pub async fn clear_banner(&self) -> Result<()> {
        self.delete("/api/v1/admin/banner").await
    }

    // ── Audit log ──────────────────────────────────────────────────────────────

    pub async fn audit_log(&self, query: AuditQuery) -> Result<AuditLogResponse> {
        self.get_with_params("/api/v1/admin/audit-log", &query)
            .await
    }

    pub async fn purge_audit_log(&self, before: &str) -> Result<PurgeAuditLogResponse> {
        #[derive(serde::Serialize)]
        struct Q<'a> {
            before: &'a str,
        }
        self.delete_with_params_json("/api/v1/admin/audit-log", &Q { before })
            .await
    }

    // ── Stats ──────────────────────────────────────────────────────────────────

    pub async fn admin_stats(&self) -> Result<StatsResponse> {
        self.get("/api/v1/admin/stats").await
    }

    // ── Health ─────────────────────────────────────────────────────────────────

    pub async fn registry_health(&self) -> Result<Vec<RegistryHealthEntry>> {
        self.get("/api/v1/admin/health").await
    }

    // ── Visibility ─────────────────────────────────────────────────────────────

    pub async fn get_visibility(&self, registry: &str, name: &str) -> Result<VisibilityResponse> {
        self.get(&format!(
            "/api/v1/admin/registries/{registry}/packages/{name}/visibility"
        ))
        .await
    }

    pub async fn set_visibility(&self, registry: &str, name: &str, visibility: &str) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            visibility: &'a str,
        }
        self.put(
            &format!("/api/v1/admin/registries/{registry}/packages/{name}/visibility"),
            &Body { visibility },
        )
        .await
    }

    // ── Grants (RFC 0017) ──────────────────────────────────────────────────────

    pub async fn list_grants(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
    ) -> Result<GrantListResponse> {
        let mut path = format!(
            "/api/v1/admin/registries/{registry}/grants?package={}",
            percent_encode(package)
        );
        if let Some(v) = version {
            path.push_str(&format!("&version={}", percent_encode(v)));
        }
        self.get(&path).await
    }

    pub async fn put_grant(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
        subject: &str,
        actions: &[String],
    ) -> Result<PutGrantResponse> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            package: &'a str,
            version: Option<&'a str>,
            subject: &'a str,
            actions: &'a [String],
        }
        self.put_json(
            &format!("/api/v1/admin/registries/{registry}/grants"),
            &Body {
                package,
                version,
                subject,
                actions,
            },
        )
        .await
    }

    pub async fn delete_grant(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
        subject: &str,
    ) -> Result<DeleteGrantResponse> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            package: &'a str,
            version: Option<&'a str>,
            subject: &'a str,
        }
        self.delete_json(
            &format!("/api/v1/admin/registries/{registry}/grants"),
            &Body {
                package,
                version,
                subject,
            },
        )
        .await
    }

    // ── Team namespaces ────────────────────────────────────────────────────────

    pub async fn list_namespaces(&self, registry: &str) -> Result<Vec<TeamNamespaceEntry>> {
        self.get(&format!("/api/v1/admin/registries/{registry}/namespaces"))
            .await
    }

    pub async fn claim_namespace(
        &self,
        registry: &str,
        prefix: &str,
        group_id: &str,
    ) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            prefix: &'a str,
            group_id: &'a str,
        }
        self.post_void(
            &format!("/api/v1/admin/registries/{registry}/namespaces"),
            &Body { prefix, group_id },
        )
        .await
    }

    pub async fn release_namespace(&self, registry: &str, prefix: &str) -> Result<()> {
        self.delete(&format!(
            "/api/v1/admin/registries/{registry}/namespaces/{prefix}"
        ))
        .await
    }

    // ── User blocks ────────────────────────────────────────────────────────────

    pub async fn list_blocked_users(&self) -> Result<Vec<BlockedUserEntry>> {
        self.get("/api/v1/admin/users/blocked").await
    }

    pub async fn block_user(&self, user_id: &str, reason: Option<&str>) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            reason: Option<&'a str>,
        }
        self.post_void(
            &format!("/api/v1/admin/users/{user_id}/block"),
            &Body { reason },
        )
        .await
    }

    pub async fn unblock_user(&self, user_id: &str) -> Result<()> {
        self.delete(&format!("/api/v1/admin/users/{user_id}/block"))
            .await
    }

    // ── SBOM ───────────────────────────────────────────────────────────────────
    //
    // The server serializes the raw SBOM document (`s.document` in
    // `crates/web/src/handlers/back_office/sbom.rs`), whose shape is SPDX or
    // CycloneDX JSON depending on the requested `format` — genuinely dynamic,
    // not a fixed server-side struct. `serde_json::Value` is the correct type
    // here rather than a DTO that would misrepresent one arbitrary schema.

    pub async fn get_sbom(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        format: &str,
    ) -> Result<serde_json::Value> {
        #[derive(serde::Serialize)]
        struct Q<'a> {
            format: &'a str,
        }
        self.get_with_params(
            &format!("/api/v1/sbom/{registry}/{name}/{version}"),
            &Q { format },
        )
        .await
    }

    pub async fn export_sbom(
        &self,
        registry: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        format: &str,
    ) -> Result<serde_json::Value> {
        #[derive(serde::Serialize)]
        struct Q<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            registry: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            from: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            to: Option<&'a str>,
            format: &'a str,
        }
        self.get_with_params(
            "/api/v1/sbom/export",
            &Q {
                registry,
                from,
                to,
                format,
            },
        )
        .await
    }

    // ── Notifications ──────────────────────────────────────────────────────────

    pub async fn list_notification_channels(&self) -> Result<Vec<NotificationChannelEntry>> {
        self.get("/api/v1/admin/notifications/channels").await
    }

    pub async fn list_notification_subscriptions(
        &self,
    ) -> Result<Vec<NotificationSubscriptionEntry>> {
        self.get("/api/v1/admin/notifications/subscriptions").await
    }

    pub async fn delete_notification_subscription(&self, id: &str) -> Result<()> {
        self.delete(&format!("/api/v1/admin/notifications/subscriptions/{id}"))
            .await
    }

    // ── Bulk operations ────────────────────────────────────────────────────────

    pub async fn bulk_yank(
        &self,
        registry: &str,
        packages: Vec<(String, String)>,
    ) -> Result<BulkPackageResult> {
        self.bulk_operation(registry, "bulk-yank", packages).await
    }

    pub async fn bulk_unyank(
        &self,
        registry: &str,
        packages: Vec<(String, String)>,
    ) -> Result<BulkPackageResult> {
        self.bulk_operation(registry, "bulk-unyank", packages).await
    }

    pub async fn bulk_delete(
        &self,
        registry: &str,
        packages: Vec<(String, String)>,
    ) -> Result<BulkPackageResult> {
        self.bulk_operation(registry, "bulk-delete", packages).await
    }

    async fn bulk_operation(
        &self,
        registry: &str,
        op: &str,
        packages: Vec<(String, String)>,
    ) -> Result<BulkPackageResult> {
        #[derive(serde::Serialize)]
        struct PkgRef {
            name: String,
            version: String,
        }
        #[derive(serde::Serialize)]
        struct Body {
            packages: Vec<PkgRef>,
        }
        let body = Body {
            packages: packages
                .into_iter()
                .map(|(name, version)| PkgRef { name, version })
                .collect(),
        };
        self.post(&format!("/api/v1/admin/registries/{registry}/{op}"), &body)
            .await
    }

    // ── Config content ─────────────────────────────────────────────────────────

    pub async fn config_content(&self) -> Result<ConfigContentResponse> {
        self.get("/api/v1/admin/config/content").await
    }

    pub async fn config_validate(&self, content: &str) -> Result<ReloadResponse> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            content: &'a str,
        }
        self.post("/api/v1/admin/config/validate", &Body { content })
            .await
    }

    pub async fn config_from_content(&self, content: &str) -> Result<ReloadResponse> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            content: &'a str,
        }
        self.post("/api/v1/admin/config/from-content", &Body { content })
            .await
    }

    // ── Deprecate / unlist ─────────────────────────────────────────────────────

    pub async fn deprecate_package(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        message: Option<&str>,
    ) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            name: &'a str,
            version: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            message: Option<&'a str>,
        }
        self.post_void(
            &format!("/api/v1/admin/registries/{registry}/deprecate"),
            &Body {
                name,
                version,
                message,
            },
        )
        .await
    }

    pub async fn undeprecate_package(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            name: &'a str,
            version: &'a str,
        }
        self.post_void(
            &format!("/api/v1/admin/registries/{registry}/undeprecate"),
            &Body { name, version },
        )
        .await
    }

    pub async fn unlist_package(&self, registry: &str, name: &str, version: &str) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            name: &'a str,
            version: &'a str,
        }
        self.post_void(
            &format!("/api/v1/admin/registries/{registry}/unlist"),
            &Body { name, version },
        )
        .await
    }

    pub async fn relist_package(&self, registry: &str, name: &str, version: &str) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            name: &'a str,
            version: &'a str,
        }
        self.post_void(
            &format!("/api/v1/admin/registries/{registry}/relist"),
            &Body { name, version },
        )
        .await
    }

    pub async fn simulate_access(
        &self,
        req: &SimulateAccessRequest,
    ) -> Result<AccessSimulationResponse> {
        self.post("/api/v1/admin/access-check", req).await
    }

    pub async fn export_audit_log(
        &self,
        registry: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        action: Option<&str>,
        format: &str,
    ) -> Result<String> {
        use anyhow::bail;
        use reqwest::Method;
        #[derive(serde::Serialize)]
        struct Params<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            registry: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            from: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            to: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            action: Option<&'a str>,
            format: &'a str,
        }
        let req = self
            .request(Method::GET, "/api/v1/admin/audit-log/export")
            .query(&Params {
                registry,
                from,
                to,
                action,
                format,
            });
        let resp = self.send(req).await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.text().await?)
        } else {
            let body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {body}")
        }
    }
}

// ── RFC 0002 (recast): flags and the exposure report ──────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct FlagsQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    pub include_dead: bool,
    pub page: u64,
    pub per_page: u64,
}

/// One pushed flag, as `GET /api/v1/admin/flags` lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagEntry {
    pub id: Uuid,
    pub source: String,
    pub external_id: String,
    pub registry: String,
    pub package_name: String,
    pub version: String,
    pub kind: String,
    pub effect: String,
    #[serde(default)]
    pub severity: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub url: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagsResponse {
    pub items: Vec<FlagEntry>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ExposureQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_effect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub limit: u64,
}

/// One consumer × coordinate × flag of the exposure report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposureRow {
    pub consumer: String,
    pub consumer_role: String,
    pub registry: String,
    pub package_name: String,
    pub version: String,
    pub source: String,
    pub external_id: String,
    pub kind: String,
    pub effect: String,
    #[serde(default)]
    pub severity: Option<String>,
    pub summary: String,
    pub flag_first_seen: DateTime<Utc>,
    pub pulls: u64,
    pub pulls_before_flag: u64,
    pub first_pull: DateTime<Utc>,
    pub last_pull: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposureScanState {
    pub registry: String,
    pub last_scan_at: DateTime<Utc>,
    pub artifacts_scanned: u64,
    pub findings: u64,
    pub errors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposureSourceCoverage {
    pub source: String,
    pub live_flags: u64,
    #[serde(default)]
    pub last_push_at: Option<DateTime<Utc>>,
}

/// What the report could and could not see.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposureCoverage {
    pub registries_total: u64,
    pub sbom_configured: u64,
    pub security_profiles: u64,
    #[serde(default)]
    pub last_scan: Vec<ExposureScanState>,
    #[serde(default)]
    pub flag_sources: Vec<ExposureSourceCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposureResponse {
    pub rows: Vec<ExposureRow>,
    #[serde(default)]
    pub next: Option<String>,
    pub coverage: ExposureCoverage,
}

impl BatleHubClient {
    /// `GET /api/v1/admin/flags`.
    pub async fn list_flags(&self, query: FlagsQuery) -> Result<FlagsResponse> {
        self.get_with_params("/api/v1/admin/flags", &query).await
    }

    /// `GET /api/v1/admin/exposure`, one page.
    pub async fn exposure(&self, query: ExposureQuery) -> Result<ExposureResponse> {
        self.get_with_params("/api/v1/admin/exposure", &query).await
    }
}

// ── RFC 0008: the bundle ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleImportResponse {
    pub bundle_id: String,
    pub signer_key: String,
    pub imported: u64,
    pub entries: u64,
    pub rejected: u64,
    #[serde(default)]
    pub rejections: Vec<String>,
    #[serde(default)]
    pub already_imported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleImportEntry {
    pub bundle_id: String,
    pub signer_key: String,
    pub imported_at: DateTime<Utc>,
    #[serde(default)]
    pub imported_by: Option<String>,
    pub entries: u64,
    pub blobs: u64,
    pub rejected: u64,
}

impl BatleHubClient {
    /// `POST /api/v1/admin/bundle/import` with the bundle as the raw body.
    pub async fn import_bundle(&self, bytes: Vec<u8>) -> Result<BundleImportResponse> {
        let req = self
            .request(reqwest::Method::POST, "/api/v1/admin/bundle/import")
            .header("Content-Type", "application/octet-stream")
            .body(bytes);
        // `expect_ok`, not a bare `json()`: an import is refused far more
        // often than it succeeds — an untrusted signature, a key that is not
        // configured, a verb the caller lacks — and decoding the refusal as
        // the success shape reports "missing field `bundle_id`" instead of
        // the reason the server gave.
        crate::api::expect_ok(self.send(req).await?).await
    }

    /// `GET /api/v1/admin/bundle`.
    pub async fn list_bundles(&self) -> Result<Vec<BundleImportEntry>> {
        #[derive(Deserialize)]
        struct Wrapper {
            items: Vec<BundleImportEntry>,
        }
        let w: Wrapper = self.get("/api/v1/admin/bundle").await?;
        Ok(w.items)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MissingQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedMissEntry {
    pub registry: String,
    pub storage_key: String,
    pub kind: String,
    #[serde(default)]
    pub coordinate: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub count: u64,
    /// The version the client asked for, when its request named one (RFC
    /// 0008-bis §4.4).
    #[serde(default)]
    pub requested_version: Option<String>,
    /// What the instance held of the package: what its synthesised
    /// listing named.
    #[serde(default)]
    pub held_versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingResponse {
    pub items: Vec<RecordedMissEntry>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
    #[serde(default)]
    pub air_gapped: bool,
}

impl BatleHubClient {
    /// `GET /api/v1/admin/air-gap/missing`.
    pub async fn air_gap_missing(&self, query: MissingQuery) -> Result<MissingResponse> {
        self.get_with_params("/api/v1/admin/air-gap/missing", &query)
            .await
    }
}
