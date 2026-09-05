//! The scanners that live in `core` (RFC 0018 §6.1): the gates a
//! `[security]` registry no longer runs as rules, re-expressed as findings.
//!
//! `block_list` and `cve_gate` are implemented directly against their
//! repositories rather than by wrapping the rules, because the rules swallow
//! a repository error into `Allow` — and the whole point of moving them here
//! is that a repository error becomes `SCANNER_ERROR`, never an allow. The
//! three gates that judge metadata (`license_gate`, `require_signed_release`,
//! `trusted_publisher`) are wrapped as-is through [`RuleAsScanner`]; their
//! own fail-open on a storage blip is the residual RFC 0018 §13 records.

use std::sync::Arc;

use async_trait::async_trait;

use crate::entities::{
    Finding, FindingKind, Identity, PackageStatus, ReasonCode, RegistryKind, Severity, UpstreamKey,
    UpstreamState,
};
use crate::ports::{
    AdvisoryRepository, ArtifactScanner, PackageRepository, ScanInput, ScannerError,
    UpstreamStatusPort, VulnerabilityRepository,
};
use crate::rules::{Rule, RuleContext, RuleDecision};

/// An administrator's block, as a finding. Always `denied`.
pub struct BlockListScanner {
    pub repo: Arc<dyn PackageRepository>,
}

#[async_trait]
impl ArtifactScanner for BlockListScanner {
    fn name(&self) -> &str {
        "block_list"
    }
    fn supports(&self, _: RegistryKind) -> bool {
        true
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let id = &input.package.id;
        match self.repo.get_status(id).await {
            Ok(PackageStatus::Blocked {
                reason, blocked_by, ..
            }) => Ok(vec![Finding::new(
                "block_list",
                FindingKind::BlockList,
                ReasonCode::BlockList,
                Severity::Critical,
                format!("blocked by {blocked_by}: {reason}"),
            )]),
            Ok(PackageStatus::Available) => Ok(Vec::new()),
            // Fail closed: the rule's own `Allow` on this path is the defect
            // RFC 0018 §2 names.
            Err(e) => Err(ScannerError::Upstream(format!(
                "block list unreadable: {e}"
            ))),
        }
    }
}

/// The recorded OSV findings for the coordinate, as findings — one per
/// advisory, each with its own severity, so the threshold judges each.
pub struct RecordedVulnerabilityScanner {
    pub repo: Arc<dyn VulnerabilityRepository>,
}

#[async_trait]
impl ArtifactScanner for RecordedVulnerabilityScanner {
    fn name(&self) -> &str {
        "cve_gate"
    }
    fn supports(&self, _: RegistryKind) -> bool {
        true
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let id = &input.package.id;
        let rows = self
            .repo
            .list_for_coordinate(&id.registry, &id.name, &id.version)
            .await
            .map_err(|e| ScannerError::Upstream(format!("vulnerability store unreadable: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                Finding::new(
                    "cve_gate",
                    FindingKind::Vulnerability,
                    ReasonCode::Vulnerability,
                    r.severity,
                    r.summary.clone(),
                )
                .with_reference(r.osv_id.clone())
                .with_raw(serde_json::json!({
                    "purl": r.purl,
                    "fixed_version": r.fixed_version,
                }))
            })
            .collect())
    }
}

/// What the forge says about who made this commit, as a finding (RFC 0019
/// phase 5).
///
/// The verdict is about a *version*, and on a forge a version is a commit —
/// so what this asks for is the commit's own signature. A release asset's
/// build attestation is keyed on the asset's digest, which is a
/// sub-coordinate the verdict does not have; the read path carries that in
/// `extra.forge` for the response, and the RFC's §13 records it as the one
/// provenance source the worker cannot reach.
///
/// A forge with no answer reports `PROVENANCE_MISSING`, never a guess.
/// `PROVENANCE_UNVERIFIABLE` comes from GitLab's release evidence alone —
/// `provenance_never_unverifiable_outside_gitlab` in the adapters pins that.
pub struct ForgeProvenanceScanner {
    /// The registry's client. Captured rather than looked up: the internal
    /// scanner map is rebuilt on every config reload from the same pass that
    /// builds the clients, so this handle is exactly as fresh as the map
    /// holding it.
    pub client: Arc<dyn crate::ports::RegistryClient>,
}

pub const FORGE_PROVENANCE_SCANNER: &str = "forge_provenance";

#[async_trait]
impl ArtifactScanner for ForgeProvenanceScanner {
    fn name(&self) -> &str {
        FORGE_PROVENANCE_SCANNER
    }

    fn supports(&self, kind: RegistryKind) -> bool {
        kind.is_forge()
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let id = &input.package.id;
        let Some(forge) = self.client.forge() else {
            return Err(ScannerError::Unsupported(format!(
                "registry '{}' has no forge client",
                id.registry
            )));
        };
        // The digest the read path recorded for a release asset, when the
        // coordinate is one; the commit otherwise.
        let asset_digest = input
            .package
            .extra
            .get(crate::entities::FORGE_EXTRA_KEY)
            .and_then(|f| f.get(crate::entities::FORGE_ASSET_DIGEST))
            .and_then(|v| v.as_str());
        let provenance = forge
            .provenance(&id.name, &id.version, asset_digest)
            .await
            .map_err(|e| ScannerError::Upstream(format!("provenance lookup failed: {e}")))?;
        let Some(code) = provenance.reason_code() else {
            return Ok(Vec::new());
        };
        Ok(vec![Finding::new(
            FORGE_PROVENANCE_SCANNER,
            FindingKind::Provenance,
            code,
            Severity::Low,
            format!("{}: {}", provenance.state(), provenance.detail()),
        )])
    }
}

/// The pushed flags covering the version (RFC 0002 §13 decision 1), as
/// findings: `hard_block` is `SOC_VERDICT`, `gate` a vulnerability at its
/// severity, `warn`/`inform` a low one for the record. Re-emitted on every
/// scan, so a revoked flag disappears with the next rescan — `record_scan`
/// drops the previous run's `flags` findings for that reason.
pub struct FlagsScanner {
    pub repo: Arc<dyn AdvisoryRepository>,
}

#[async_trait]
impl ArtifactScanner for FlagsScanner {
    fn name(&self) -> &str {
        crate::entities::FLAGS_SCANNER
    }
    fn supports(&self, _: RegistryKind) -> bool {
        true
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let id = &input.package.id;
        let now = chrono::Utc::now();
        let flags = self
            .repo
            .live_flags_for_package(&id.registry, &id.name, now)
            .await
            .map_err(|e| ScannerError::Upstream(format!("flag store unreadable: {e}")))?;
        Ok(flags
            .iter()
            .filter(|f| f.is_live(now) && f.covers(&id.version))
            .map(|f| f.as_finding())
            .collect())
    }
}

/// An existing gate, run as a scanner: `Deny { reason }` becomes one finding
/// with a fixed kind, code and severity; `Allow` becomes none. The rule is
/// judged for the system identity, so a `bypass_roles` entry never applies —
/// the only bypass of a verdict is a `GateExemption`.
pub struct RuleAsScanner {
    pub rule: Box<dyn Rule>,
    pub kind: FindingKind,
    pub code: ReasonCode,
    pub severity: Severity,
}

impl RuleAsScanner {
    /// The wrapper for one of the three metadata gates, or `None` for a rule
    /// this pipeline does not wrap.
    pub fn for_rule(rule: Box<dyn Rule>) -> Option<Self> {
        let (kind, code, severity) = match rule.name() {
            "license_gate" => (
                FindingKind::License,
                ReasonCode::LicenseDenied,
                Severity::High,
            ),
            "require_signed_release" => (
                FindingKind::Signature,
                ReasonCode::SignatureMissing,
                Severity::High,
            ),
            "trusted_publisher" => (
                FindingKind::Publisher,
                ReasonCode::UntrustedPublisher,
                Severity::High,
            ),
            _ => return None,
        };
        Some(Self {
            rule,
            kind,
            code,
            severity,
        })
    }
}

#[async_trait]
impl ArtifactScanner for RuleAsScanner {
    fn name(&self) -> &str {
        self.rule.name()
    }
    fn supports(&self, _: RegistryKind) -> bool {
        true
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let system = Identity::system();
        let ctx = RuleContext {
            identity: &system,
            package: &input.package,
            action: crate::entities::Action::ReleasesRead,
            cache_entry: None,
            requested_version: Some(&input.package.id.version),
        };
        match self.rule.evaluate(&ctx).await {
            RuleDecision::Allow => Ok(Vec::new()),
            RuleDecision::Deny { reason } => Ok(vec![Finding::new(
                self.rule.name(),
                self.kind,
                self.code,
                self.severity,
                reason,
            )]),
        }
    }
}

/// The names of the gates a `[security]` registry runs as scanners rather
/// than as rules (RFC 0018 §4.1). `build_policy` omits these from the chain
/// and registers the scanners instead; every one of them is required.
pub const WRAPPED_GATES: &[&str] = &[
    "block_list",
    "cve_gate",
    "license_gate",
    "require_signed_release",
    "trusted_publisher",
];

/// RFC 0014's probe, as the finding it produces (0018 decision 29).
///
/// The sweep decides — population gate, confirmation window — and writes the
/// `upstream_status` row; this scanner reads the row for the coordinate and
/// says `UNPUBLISHED_UPSTREAM` when it is confirmed. It never probes upstream
/// itself: a scan is one coordinate and the whole point of 0014 §5.1 is that
/// one coordinate's 404 proves nothing. The severity is the policy's
/// `on_confirmed`: `high` under `"block"` (a `denied` verdict on a
/// `[security]` registry), `low` under `"audit"` (recorded, visible in
/// `batlehub why`, never a hold).
pub struct UpstreamPresenceScanner {
    pub status: Arc<dyn UpstreamStatusPort>,
    /// `on_confirmed = "block"`.
    pub deny: bool,
}

/// The scanner's name, as `scanners_done` records it.
pub const UPSTREAM_PRESENCE_SCANNER: &str = "upstream-presence";

#[async_trait]
impl ArtifactScanner for UpstreamPresenceScanner {
    fn name(&self) -> &str {
        UPSTREAM_PRESENCE_SCANNER
    }
    fn supports(&self, _: RegistryKind) -> bool {
        true
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let id = &input.package.id;
        let version = UpstreamKey::version(&id.registry, &id.name, &id.version);
        let package = UpstreamKey::package(&id.registry, &id.name);
        let mut row = None;
        for key in [version, package] {
            match self.status.get(&key).await {
                Ok(Some(r)) if r.state == UpstreamState::Disappeared => {
                    row = Some(r);
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(ScannerError::Upstream(format!(
                        "upstream-status unreadable: {e}"
                    )))
                }
            }
        }
        let Some(row) = row else {
            return Ok(Vec::new());
        };
        let scope = if row.version.is_some() {
            "this version"
        } else {
            "the whole package"
        };
        let confirmed = row
            .confirmed_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_owned());
        Ok(vec![Finding::new(
            UPSTREAM_PRESENCE_SCANNER,
            FindingKind::Transition,
            ReasonCode::UnpublishedUpstream,
            if self.deny {
                Severity::High
            } else {
                Severity::Low
            },
            format!(
                "upstream no longer lists {scope}: confirmed {confirmed} after {} misses since {}",
                row.consecutive_misses,
                row.first_missed_at.to_rfc3339()
            ),
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{PackageId, PackageMetadata};

    struct DenyingRule;
    #[async_trait]
    impl Rule for DenyingRule {
        fn name(&self) -> &str {
            "license_gate"
        }
        async fn evaluate(&self, _: &RuleContext<'_>) -> RuleDecision {
            RuleDecision::Deny {
                reason: "AGPL-3.0 is denied".into(),
            }
        }
    }

    fn input() -> ScanInput {
        ScanInput {
            package: PackageMetadata::minimal(
                PackageId::new("r", "p", "1.0.0"),
                serde_json::Value::Null,
            ),
            kind: RegistryKind::Npm,
            purl: "pkg:npm/p@1.0.0".into(),
            artifact: None,
            sbom: None,
            listing: None,
        }
    }

    #[tokio::test]
    async fn a_wrapped_denial_is_one_finding_with_the_gates_fixed_shape() {
        let s = RuleAsScanner::for_rule(Box::new(DenyingRule)).unwrap();
        let findings = s.scan(&input()).await.unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, ReasonCode::LicenseDenied);
        assert_eq!(findings[0].kind, FindingKind::License);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].summary, "AGPL-3.0 is denied");
    }

    struct Unwrapped;
    #[async_trait]
    impl Rule for Unwrapped {
        fn name(&self) -> &str {
            "deny_latest"
        }
        async fn evaluate(&self, _: &RuleContext<'_>) -> RuleDecision {
            RuleDecision::Allow
        }
    }

    #[test]
    fn selection_rules_are_not_wrapped() {
        assert!(RuleAsScanner::for_rule(Box::new(Unwrapped)).is_none());
    }
}

/// A scanner under the name its `[scanners.<name>]` entry gave it.
///
/// `required_scanners` and `scanners_done` speak in config keys, and the
/// worker records a scanner as done under `name()`. An adapter answers its
/// *type* — `osv`, `postmortem` — so a second OSV pointed at another
/// database (`[scanners.osvflip] type = "osv"`) or a postmortem-shaped
/// probe under its own key was never "done" and held its registry forever
/// (found by `tests/heavy/quarantine.sh` step 7). This wrapper is the
/// config key, all the way down: its findings carry it too, so a
/// per-scanner escalation block keyed on it applies.
pub struct NamedScanner {
    name: String,
    inner: Arc<dyn ArtifactScanner>,
}

impl NamedScanner {
    /// `inner` under `name`; the inner scanner itself when the two agree.
    pub fn wrap(name: &str, inner: Arc<dyn ArtifactScanner>) -> Arc<dyn ArtifactScanner> {
        if inner.name() == name {
            inner
        } else {
            Arc::new(Self {
                name: name.to_owned(),
                inner,
            })
        }
    }
}

#[async_trait]
impl ArtifactScanner for NamedScanner {
    fn name(&self) -> &str {
        &self.name
    }
    fn supports(&self, kind: RegistryKind) -> bool {
        self.inner.supports(kind)
    }
    fn needs_artifact(&self) -> bool {
        self.inner.needs_artifact()
    }
    fn needs_listing(&self) -> bool {
        self.inner.needs_listing()
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let mut findings = self.inner.scan(input).await?;
        for f in &mut findings {
            f.scanner = self.name.clone();
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod named_tests {
    use super::*;
    use crate::entities::{PackageId, PackageMetadata, ReasonCode, Severity};

    struct Typed;
    #[async_trait]
    impl ArtifactScanner for Typed {
        fn name(&self) -> &str {
            "osv"
        }
        fn supports(&self, _: RegistryKind) -> bool {
            true
        }
        async fn scan(&self, _: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
            Ok(vec![Finding::new(
                "osv",
                FindingKind::Vulnerability,
                ReasonCode::Vulnerability,
                Severity::High,
                "x",
            )])
        }
    }

    #[tokio::test]
    async fn a_scanner_answers_under_its_config_key_and_so_do_its_findings() {
        let same = NamedScanner::wrap("osv", Arc::new(Typed));
        assert_eq!(same.name(), "osv");
        let renamed = NamedScanner::wrap("osvflip", Arc::new(Typed));
        assert_eq!(renamed.name(), "osvflip");
        let input = ScanInput {
            package: PackageMetadata::minimal(
                PackageId::new("r", "p", "1"),
                serde_json::Value::Null,
            ),
            kind: RegistryKind::Npm,
            purl: String::new(),
            artifact: None,
            sbom: None,
            listing: None,
        };
        let findings = renamed.scan(&input).await.unwrap();
        assert_eq!(findings[0].scanner, "osvflip");
    }
}
