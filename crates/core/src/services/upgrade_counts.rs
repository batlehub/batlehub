//! Release N's upgrade counts (RFC 0041 §6.8): per registry, how many
//! versions hold an unrated (`unknown`) security finding and how many
//! artifacts hold an unrated advisory — what the next release judges by
//! `unknown_severity` (default `high`) instead of serving.
//!
//! Logged once per process start, every registry, zero included, at `WARN`
//! under the target `batlehub::upgrade`, so `grep batlehub::upgrade` on the
//! logs answers the CHANGELOG checklist's question.

use std::collections::HashMap;

use crate::error::CoreError;
use crate::ports::{VerdictRepository, VulnerabilityRepository};

/// One line per registry, or `None` when another replica is counting.
pub async fn unrated_upgrade_lines(
    verdicts: &dyn VerdictRepository,
    vulns: &dyn VulnerabilityRepository,
    registries: &[String],
) -> Result<Option<Vec<String>>, CoreError> {
    let Some(versions) = verdicts.count_unrated_by_registry().await? else {
        return Ok(None);
    };
    let Some(artifacts) = vulns.count_unrated_by_registry().await? else {
        return Ok(None);
    };
    let versions: HashMap<String, u64> = versions.into_iter().collect();
    let artifacts: HashMap<String, u64> = artifacts.into_iter().collect();
    Ok(Some(
        registries
            .iter()
            .map(|r| {
                format!(
                    "{} versions on {r} hold an unrated advisory, and {} cached artifacts \
                     carry one in their SBOM scan; from the next release they are judged \
                     unknown_severity (default high)",
                    versions.get(r).copied().unwrap_or(0),
                    artifacts.get(r).copied().unwrap_or(0),
                )
            })
            .collect(),
    ))
}

/// Count and log, once. Errors are logged, never fatal: the count is advice.
pub async fn log_unrated_upgrade_counts(
    verdicts: &dyn VerdictRepository,
    vulns: &dyn VulnerabilityRepository,
    registries: &[String],
) {
    match unrated_upgrade_lines(verdicts, vulns, registries).await {
        Ok(Some(lines)) => {
            for line in lines {
                tracing::warn!(target: "batlehub::upgrade", "{line}");
            }
        }
        Ok(None) => {
            tracing::debug!("upgrade counts: another replica is counting");
        }
        Err(e) => {
            tracing::warn!(target: "batlehub::upgrade", error = %e, "could not count unrated advisories");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};

    use crate::entities::{ArtifactVulnerability, PackageId, Verdict, VerdictState};

    struct Counts(Option<Vec<(String, u64)>>);

    #[async_trait]
    impl VerdictRepository for Counts {
        async fn upsert(&self, _: &Verdict) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get(&self, _: &PackageId) -> Result<Option<Verdict>, CoreError> {
            Ok(None)
        }
        async fn list_for_package(&self, _: &str, _: &str) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
        async fn list_by_state(
            &self,
            _: &str,
            _: VerdictState,
            _: u64,
        ) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
        async fn list_due_for_rescan(
            &self,
            _: &str,
            _: DateTime<Utc>,
            _: u64,
        ) -> Result<Vec<PackageId>, CoreError> {
            Ok(vec![])
        }
        async fn count_unrated_by_registry(&self) -> Result<Option<Vec<(String, u64)>>, CoreError> {
            Ok(self.0.clone())
        }
    }

    #[async_trait]
    impl VulnerabilityRepository for Counts {
        async fn replace_findings_for_artifact(
            &self,
            _: &str,
            _: Vec<ArtifactVulnerability>,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn list_for_coordinate(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<ArtifactVulnerability>, CoreError> {
            Ok(vec![])
        }
        async fn count_unrated_by_registry(&self) -> Result<Option<Vec<(String, u64)>>, CoreError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn every_registry_gets_a_line_zero_included_and_a_cve_gate_only_one_counts() {
        let verdicts = Counts(Some(vec![("npm".into(), 3)]));
        // `helm` has no security profile, so no verdicts: its unrated row is
        // in `artifact_vulnerabilities` only.
        let vulns = Counts(Some(vec![("helm".into(), 2)]));
        let regs = ["npm".to_owned(), "helm".to_owned(), "pypi".to_owned()];
        let lines = unrated_upgrade_lines(&verdicts, &vulns, &regs)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("3 versions on npm") && lines[0].contains("and 0 cached"));
        assert!(lines[1].starts_with("0 versions on helm") && lines[1].contains("and 2 cached"));
        assert!(lines[2].starts_with("0 versions on pypi"));
    }

    #[tokio::test]
    async fn nothing_is_logged_while_another_replica_counts() {
        let busy = Counts(None);
        assert!(unrated_upgrade_lines(&busy, &busy, &["npm".to_owned()])
            .await
            .unwrap()
            .is_none());
    }
}
