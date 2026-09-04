//! The one rule that reads verdicts (RFC 0018 §5.1).
//!
//! `build_policy` places it **first** in the chain of every registry that
//! opts into `[security]`, where `BlockListRule` sits otherwise, so there is
//! no path around it: an artifact in such a registry is never streamed
//! without a persisted verdict in `allowed` or `warned`. "Not scanned yet" is
//! a persisted `SCAN_PENDING` hold, not an absence, so the fail-open of
//! `CveGateRule` cannot recur here.
//!
//! It ignores `bypass_roles` on purpose: the only bypass is a
//! `GateExemption` on the `security_verdict` gate, which the service folds in
//! as `ADMIN_OVERRIDE` — a `warned`, never an `allowed`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::entities::SecurityPolicy;
use crate::ports::PolicyRepository;
use crate::rules::{Rule, RuleContext, RuleDecision};
use crate::services::verdict::{apply_override, VerdictService, VERDICT_EXEMPTION_GATE};

pub struct VerdictGateRule {
    pub service: Arc<VerdictService>,
    pub policy: SecurityPolicy,
    /// For the `security_verdict` exemption. `None` means no exemption can
    /// exist, which is what a deployment with no policy store has.
    pub policy_repo: Option<Arc<dyn PolicyRepository>>,
}

impl VerdictGateRule {
    pub fn new(
        service: Arc<VerdictService>,
        policy: SecurityPolicy,
        policy_repo: Option<Arc<dyn PolicyRepository>>,
    ) -> Self {
        Self {
            service,
            policy,
            policy_repo,
        }
    }
}

#[async_trait]
impl Rule for VerdictGateRule {
    fn name(&self) -> &str {
        "verdict_gate"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let now = Utc::now();
        let verdict = match self.service.current(ctx.package, &self.policy, now).await {
            Ok(v) => v,
            Err(e) => {
                // Fail closed. This is the reverse of every other gate's
                // storage error, and the property the design buys.
                tracing::error!(
                    package = %ctx.package.id,
                    error = %e,
                    "security: verdict store unavailable; refusing"
                );
                return RuleDecision::Deny {
                    reason: format!(
                        "{}:{}@{} cannot be served: the verdict store is unavailable",
                        ctx.package.id.registry, ctx.package.id.name, ctx.package.id.version
                    ),
                };
            }
        };
        if verdict.is_served() {
            return RuleDecision::Allow;
        }
        let exempt =
            crate::services::authz::exempt_gates_in(self.policy_repo.as_ref(), &verdict.package)
                .await;
        if exempt.iter().any(|g| g == VERDICT_EXEMPTION_GATE) {
            let over = apply_override(verdict);
            tracing::info!(
                package = %over.package,
                codes = ?over.reason_codes,
                "security: verdict overridden by an active exemption; serving as warned"
            );
            return RuleDecision::Allow;
        }
        let status = "403";
        for code in &verdict.reason_codes {
            metrics::counter!(
                "batlehub_verdict_denials_served_total",
                "registry" => verdict.package.registry.clone(),
                "code" => code.as_str(),
                "status" => status,
            )
            .increment(1);
        }
        RuleDecision::Deny {
            reason: verdict.short_message(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{
        Action, Finding, FindingKind, Identity, PackageId, PackageMetadata, ReasonCode, ScanJob,
        ScanTrigger, Severity, Verdict, VerdictState,
    };
    use crate::error::CoreError;
    use crate::ports::{QueuedCount, ScanQueue, VerdictRepository};
    use chrono::DateTime;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct MemVerdicts(Mutex<HashMap<String, Verdict>>);
    #[async_trait]
    impl VerdictRepository for MemVerdicts {
        async fn upsert(&self, v: &Verdict) -> Result<(), CoreError> {
            self.0
                .lock()
                .unwrap()
                .insert(v.package.cache_key(), v.clone());
            Ok(())
        }
        async fn get(&self, p: &PackageId) -> Result<Option<Verdict>, CoreError> {
            Ok(self.0.lock().unwrap().get(&p.cache_key()).cloned())
        }
        async fn list_by_state(
            &self,
            _: &str,
            _: VerdictState,
            _: u64,
        ) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
    }

    #[derive(Default)]
    struct MemQueue(Mutex<Vec<PackageId>>);
    #[async_trait]
    impl ScanQueue for MemQueue {
        async fn enqueue(
            &self,
            p: &PackageId,
            _: Option<DateTime<Utc>>,
            _: ScanTrigger,
        ) -> Result<bool, CoreError> {
            self.0.lock().unwrap().push(p.clone());
            Ok(true)
        }
        async fn lease(
            &self,
            _: &str,
            _: &[String],
            _: u32,
            _: u64,
            _: u32,
        ) -> Result<Vec<ScanJob>, CoreError> {
            Ok(vec![])
        }
        async fn heartbeat(&self, _: uuid::Uuid, _: u64) -> Result<(), CoreError> {
            Ok(())
        }
        async fn complete(&self, _: uuid::Uuid) -> Result<(), CoreError> {
            Ok(())
        }
        async fn fail(&self, _: uuid::Uuid, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        async fn exhausted(&self, _: u32, _: u32) -> Result<Vec<ScanJob>, CoreError> {
            Ok(vec![])
        }
        async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError> {
            Ok(vec![])
        }
    }

    fn gate(verdicts: Arc<MemVerdicts>, queue: Arc<MemQueue>) -> VerdictGateRule {
        let mut policy = SecurityPolicy::defaults_for("r");
        policy.min_age = Duration::from_secs(3600);
        VerdictGateRule::new(Arc::new(VerdictService::new(verdicts, queue)), policy, None)
    }

    fn meta(age_secs: i64) -> PackageMetadata {
        let mut m = PackageMetadata::minimal(
            PackageId::new("r", "p", "1.0.0").with_artifact("tarball"),
            serde_json::Value::Null,
        );
        m.published_at = Some(Utc::now() - chrono::Duration::seconds(age_secs));
        m
    }

    async fn decide(
        rule: &VerdictGateRule,
        m: &PackageMetadata,
        role: crate::entities::Role,
    ) -> RuleDecision {
        let identity = Identity {
            user_id: Some("u".into()),
            role,
            auth_provider: None,
            groups: vec![],
        };
        rule.evaluate(&RuleContext {
            identity: &identity,
            package: m,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: Some("1.0.0"),
        })
        .await
    }

    #[tokio::test]
    async fn first_sight_is_a_persisted_hold_and_a_queued_job() {
        let verdicts = Arc::new(MemVerdicts::default());
        let queue = Arc::new(MemQueue::default());
        let rule = gate(Arc::clone(&verdicts), Arc::clone(&queue));
        let d = decide(&rule, &meta(7200), crate::entities::Role::Admin).await;
        let RuleDecision::Deny { reason } = d else {
            panic!("unscanned must be refused")
        };
        assert!(reason.contains("SCAN_PENDING"), "{reason}");
        assert_eq!(queue.0.lock().unwrap().len(), 1);
        let stored = verdicts
            .get(&PackageId::new("r", "p", "1.0.0"))
            .await
            .unwrap();
        assert_eq!(stored.unwrap().state, VerdictState::Quarantined);
    }

    #[tokio::test]
    async fn a_served_verdict_allows_and_the_role_does_not_matter() {
        let verdicts = Arc::new(MemVerdicts::default());
        let queue = Arc::new(MemQueue::default());
        verdicts
            .upsert(&Verdict {
                package: PackageId::new("r", "p", "1.0.0"),
                state: VerdictState::Allowed,
                reason_codes: vec![],
                findings: vec![],
                policy_ref: "r/default".into(),
                available_at: None,
                evaluated_at: Utc::now(),
                last_scanned_at: Some(Utc::now()),
                scanners_done: vec!["osv".into()],
            })
            .await
            .unwrap();
        let rule = gate(Arc::clone(&verdicts), Arc::clone(&queue));
        assert!(
            !decide(&rule, &meta(7200), crate::entities::Role::Anonymous)
                .await
                .is_deny()
        );
        assert!(queue.0.lock().unwrap().is_empty(), "nothing re-queued");
    }

    #[tokio::test]
    async fn a_denied_verdict_refuses_an_admin_too() {
        let verdicts = Arc::new(MemVerdicts::default());
        let queue = Arc::new(MemQueue::default());
        verdicts
            .upsert(&Verdict {
                package: PackageId::new("r", "p", "1.0.0"),
                state: VerdictState::Denied,
                reason_codes: vec![ReasonCode::Vulnerability],
                findings: vec![Finding::new(
                    "osv",
                    FindingKind::Vulnerability,
                    ReasonCode::Vulnerability,
                    Severity::Critical,
                    "GHSA-1",
                )],
                policy_ref: "r/default".into(),
                available_at: None,
                evaluated_at: Utc::now(),
                last_scanned_at: Some(Utc::now()),
                scanners_done: vec!["osv".into()],
            })
            .await
            .unwrap();
        let rule = gate(verdicts, queue);
        let d = decide(&rule, &meta(7200), crate::entities::Role::Admin).await;
        assert!(d.is_deny());
    }
}
