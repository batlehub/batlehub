use std::sync::Arc;

use async_trait::async_trait;

use crate::entities::{Role, Severity};
use crate::ports::VulnerabilityRepository;
use crate::rules::{Rule, RuleContext, RuleDecision};

/// Denies (or, in warn-only mode, permits) access to a package version that has
/// one or more recorded vulnerability findings at or above `min_severity`.
///
/// Findings are produced out-of-band by the periodic SBOM re-scan
/// (`VulnerabilityScanService`). When `block` is `false` the rule never denies —
/// the finding is surfaced in the UI but downloads proceed (warn-only). When
/// `block` is `true`, callers who clear `bypass_roles` (`Role::clears_bypass`) are still allowed.
pub struct CveGateRule {
    pub repo: Arc<dyn VulnerabilityRepository>,
    pub min_severity: Severity,
    pub bypass_roles: Vec<Role>,
    pub block: bool,
}

impl CveGateRule {
    pub fn new(
        repo: Arc<dyn VulnerabilityRepository>,
        min_severity: Severity,
        bypass_roles: Vec<Role>,
        block: bool,
    ) -> Self {
        Self {
            repo,
            min_severity,
            bypass_roles,
            block,
        }
    }
}

#[async_trait]
impl Rule for CveGateRule {
    fn name(&self) -> &str {
        "cve_gate"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        // Warn-only mode never blocks; skip the lookup entirely.
        if !self.block {
            return RuleDecision::Allow;
        }
        if ctx.identity.role.clears_bypass(&self.bypass_roles) {
            return RuleDecision::Allow;
        }

        let id = &ctx.package.id;
        let findings = match self
            .repo
            .list_for_coordinate(&id.registry, &id.name, &id.version)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                // SECURITY: fail-open, consistent with BlockListRule — a DB outage
                // must not turn the proxy into a brick wall.
                tracing::warn!(
                    package = %id,
                    error = %e,
                    "CveGateRule: failed to query vulnerabilities, failing open"
                );
                return RuleDecision::Allow;
            }
        };

        let worst = findings
            .iter()
            .filter(|f| f.severity >= self.min_severity)
            .max_by_key(|f| f.severity);

        match worst {
            Some(f) => RuleDecision::Deny {
                reason: format!(
                    "blocked: known {} vulnerability {} (minimum gated severity: {})",
                    f.severity.as_str(),
                    f.osv_id,
                    self.min_severity.as_str(),
                ),
            },
            None => RuleDecision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::entities::Action;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::entities::{ArtifactVulnerability, Identity, PackageId, PackageMetadata, Role};
    use crate::error::CoreError;
    use crate::rules::RuleContext;

    #[derive(Default)]
    struct MemVulnRepo {
        // keyed by "registry/name/version"
        items: Mutex<HashMap<String, Vec<ArtifactVulnerability>>>,
    }

    impl MemVulnRepo {
        fn with(reg: &str, name: &str, ver: &str, sev: Severity) -> Arc<Self> {
            let r = Arc::new(Self::default());
            r.items.lock().unwrap().insert(
                format!("{reg}/{name}/{ver}"),
                vec![ArtifactVulnerability {
                    id: Uuid::new_v4(),
                    artifact_key: format!("artifact:{reg}/{name}/{ver}"),
                    registry: reg.to_owned(),
                    package_name: name.to_owned(),
                    version: ver.to_owned(),
                    osv_id: "RUSTSEC-2021-0001".to_owned(),
                    severity: sev,
                    summary: "boom".to_owned(),
                    fixed_version: None,
                    purl: format!("pkg:cargo/{name}@{ver}"),
                    detected_at: Utc::now(),
                }],
            );
            r
        }
    }

    #[async_trait]
    impl VulnerabilityRepository for MemVulnRepo {
        async fn replace_findings_for_artifact(
            &self,
            _artifact_key: &str,
            _findings: Vec<ArtifactVulnerability>,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn list_for_coordinate(
            &self,
            registry: &str,
            name: &str,
            version: &str,
        ) -> Result<Vec<ArtifactVulnerability>, CoreError> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&format!("{registry}/{name}/{version}"))
                .cloned()
                .unwrap_or_default())
        }
    }

    fn meta() -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("cargo", "yaml", "0.3.0"),
            published_at: Some(Utc::now()),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    fn identity(role: Role) -> Identity {
        Identity {
            user_id: None,
            role,
            auth_provider: None,
            groups: vec![],
        }
    }

    async fn eval(rule: &CveGateRule, m: &PackageMetadata, id: &Identity) -> RuleDecision {
        let ctx = RuleContext {
            identity: id,
            package: m,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        rule.evaluate(&ctx).await
    }

    #[tokio::test]
    async fn warn_only_allows_even_with_findings() {
        let repo = MemVulnRepo::with("cargo", "yaml", "0.3.0", Severity::Critical);
        let rule = CveGateRule::new(repo, Severity::High, vec![], false);
        let m = meta();
        assert!(matches!(
            eval(&rule, &m, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn block_denies_when_at_or_above_threshold() {
        let repo = MemVulnRepo::with("cargo", "yaml", "0.3.0", Severity::High);
        let rule = CveGateRule::new(repo, Severity::High, vec![Role::Admin], true);
        let m = meta();
        assert!(matches!(
            eval(&rule, &m, &identity(Role::Anonymous)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn block_allows_below_threshold() {
        let repo = MemVulnRepo::with("cargo", "yaml", "0.3.0", Severity::Low);
        let rule = CveGateRule::new(repo, Severity::High, vec![], true);
        let m = meta();
        assert!(matches!(
            eval(&rule, &m, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn bypass_role_allowed_despite_findings() {
        let repo = MemVulnRepo::with("cargo", "yaml", "0.3.0", Severity::Critical);
        let rule = CveGateRule::new(repo, Severity::High, vec![Role::Admin], true);
        let m = meta();
        assert!(matches!(
            eval(&rule, &m, &identity(Role::Admin)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn block_allows_when_no_findings() {
        let repo = Arc::new(MemVulnRepo::default());
        let rule = CveGateRule::new(repo, Severity::High, vec![], true);
        let m = meta();
        assert!(matches!(
            eval(&rule, &m, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    struct ErrVulnRepo;

    #[async_trait]
    impl VulnerabilityRepository for ErrVulnRepo {
        async fn replace_findings_for_artifact(
            &self,
            _artifact_key: &str,
            _findings: Vec<ArtifactVulnerability>,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn list_for_coordinate(
            &self,
            _registry: &str,
            _name: &str,
            _version: &str,
        ) -> Result<Vec<ArtifactVulnerability>, CoreError> {
            Err(CoreError::Database("db down".into()))
        }
    }

    #[tokio::test]
    async fn db_error_fails_open() {
        // SECURITY: a DB outage must not brick the proxy — fail open, like BlockListRule.
        let rule = CveGateRule::new(Arc::new(ErrVulnRepo), Severity::High, vec![], true);
        let m = meta();
        assert!(matches!(
            eval(&rule, &m, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[test]
    fn rule_name_is_cve_gate() {
        let rule = CveGateRule::new(
            Arc::new(MemVulnRepo::default()),
            Severity::High,
            vec![],
            true,
        );
        assert_eq!(rule.name(), "cve_gate");
    }

    #[tokio::test]
    async fn deny_reason_reports_worst_severity_and_id() {
        // Two findings; the gate should report the highest (critical) one.
        let repo = Arc::new(MemVulnRepo::default());
        repo.items.lock().unwrap().insert(
            "cargo/yaml/0.3.0".to_owned(),
            vec![
                ArtifactVulnerability {
                    id: Uuid::new_v4(),
                    artifact_key: "artifact:cargo/yaml/0.3.0".to_owned(),
                    registry: "cargo".to_owned(),
                    package_name: "yaml".to_owned(),
                    version: "0.3.0".to_owned(),
                    osv_id: "LOW-1".to_owned(),
                    severity: Severity::High,
                    summary: "h".to_owned(),
                    fixed_version: None,
                    purl: "pkg:cargo/yaml@0.3.0".to_owned(),
                    detected_at: Utc::now(),
                },
                ArtifactVulnerability {
                    id: Uuid::new_v4(),
                    artifact_key: "artifact:cargo/yaml/0.3.0".to_owned(),
                    registry: "cargo".to_owned(),
                    package_name: "yaml".to_owned(),
                    version: "0.3.0".to_owned(),
                    osv_id: "CRIT-1".to_owned(),
                    severity: Severity::Critical,
                    summary: "c".to_owned(),
                    fixed_version: None,
                    purl: "pkg:cargo/yaml@0.3.0".to_owned(),
                    detected_at: Utc::now(),
                },
            ],
        );
        let rule = CveGateRule::new(repo, Severity::High, vec![], true);
        let m = meta();
        match eval(&rule, &m, &identity(Role::Anonymous)).await {
            RuleDecision::Deny { reason } => {
                assert!(reason.contains("CRIT-1"), "reason: {reason}");
                assert!(reason.contains("critical"), "reason: {reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }
}
