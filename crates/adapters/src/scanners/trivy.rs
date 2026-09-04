//! Trivy as an [`ArtifactScanner`] (RFC 0018 §6.3).
//!
//! Trivy is a binary this proxy already provisions (`mise.toml`), and its
//! client mode is the CLI itself: `trivy … --server <endpoint>` sends the
//! scan to a Trivy server that holds the vulnerability database, so the
//! worker image needs no database of its own. Without an endpoint the CLI
//! uses a local database, which it downloads on first use — either way the
//! sandbox keeps the network namespace for this scanner.
//!
//! What is scanned: the CycloneDX SBOM already recorded for the artifact
//! when there is one (`trivy sbom`), else the archive extracted under the
//! `ExtractPolicy` (`trivy fs --scanners vuln`). The SBOM is the better
//! input — it is what this instance already asserts about the artifact —
//! and it keeps hostile bytes out of a third scanner.
//!
//! # Observed, not read
//!
//! The JSON shape — `Results[].Vulnerabilities[]` with `VulnerabilityID`,
//! `Severity` (upper case), `Title`, `FixedVersion`, `PkgName` — is trivy
//! 0.74.0's on a CycloneDX SBOM naming `lodash@4.17.15`; the fixture in the
//! tests is that run's output, trimmed.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, ReasonCode, RegistryKind, Severity};
use batlehub_core::ports::{ArtifactScanner, ScanInput, ScannerError};

use super::extract::{extract_to, ExtractPolicy};
use super::subprocess::{self, Invocation, Sandbox};

pub const NAME: &str = "trivy";

pub struct TrivyScanner {
    pub command: PathBuf,
    /// `--server`; empty means the CLI's own database.
    pub endpoint: String,
    pub sandbox: Sandbox,
    pub extract: ExtractPolicy,
    pub timeout: Duration,
}

impl TrivyScanner {
    fn common_args(&self) -> Vec<String> {
        let mut args = vec![
            "--format".to_owned(),
            "json".to_owned(),
            "--quiet".to_owned(),
            "--skip-version-check".to_owned(),
        ];
        if !self.endpoint.is_empty() {
            args.push("--server".to_owned());
            args.push(self.endpoint.clone());
        }
        args
    }

    pub fn map(doc: &serde_json::Value) -> Vec<Finding> {
        let mut out = Vec::new();
        let Some(results) = doc.get("Results").and_then(|r| r.as_array()) else {
            return out;
        };
        for result in results {
            for v in result
                .get("Vulnerabilities")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let id = v
                    .get("VulnerabilityID")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown");
                let severity = v
                    .get("Severity")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_ascii_lowercase())
                    .and_then(|s| Severity::parse(&s))
                    .unwrap_or(Severity::Unknown);
                let pkg = v.get("PkgName").and_then(|s| s.as_str()).unwrap_or("");
                let title = v.get("Title").and_then(|s| s.as_str()).unwrap_or("");
                let fixed = v
                    .get("FixedVersion")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|f| format!(", fixed in {f}"))
                    .unwrap_or_default();
                let summary = if title.is_empty() {
                    format!("{id} in {pkg}{fixed}")
                } else {
                    format!("{title} ({pkg}{fixed})")
                };
                out.push(
                    Finding::new(
                        NAME,
                        FindingKind::Vulnerability,
                        ReasonCode::Vulnerability,
                        severity,
                        summary,
                    )
                    .with_reference(id)
                    .with_raw(v.clone()),
                );
            }
        }
        out
    }
}

#[async_trait]
impl ArtifactScanner for TrivyScanner {
    fn name(&self) -> &str {
        NAME
    }

    /// Every kind an SBOM is generated for, plus the archive kinds Trivy's
    /// filesystem scanner reads lockfiles out of.
    fn supports(&self, kind: RegistryKind) -> bool {
        matches!(
            kind,
            RegistryKind::Npm
                | RegistryKind::Pypi
                | RegistryKind::Cargo
                | RegistryKind::Rubygems
                | RegistryKind::Composer
                | RegistryKind::Goproxy
                | RegistryKind::Maven
                | RegistryKind::Nuget
                | RegistryKind::Conda
        )
    }

    fn needs_artifact(&self) -> bool {
        true
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let work = subprocess::work_dir("trivy")?;
        let mut args = self.common_args();
        // Trivy's own cache and config live in the work directory: the only
        // writable place, and gone with the job.
        args.push("--cache-dir".into());
        args.push(work.path().join(".trivy").to_string_lossy().into_owned());
        if let Some(sbom) = &input.sbom {
            let path = work.path().join("sbom.cdx.json");
            std::fs::write(&path, serde_json::to_vec(sbom).unwrap_or_default())
                .map_err(|e| ScannerError::Other(format!("io: {e}")))?;
            args.insert(0, "sbom".into());
            args.push(path.to_string_lossy().into_owned());
        } else {
            let Some(artifact) = input.artifact.as_ref() else {
                return Err(ScannerError::Unsupported(
                    "trivy needs an SBOM or the artifact bytes and had neither".into(),
                ));
            };
            let tree = work.path().join("tree");
            extract_to(artifact, &tree, &self.extract)?;
            args.insert(0, "fs".into());
            args.push("--scanners".into());
            args.push("vuln".into());
            args.push(tree.to_string_lossy().into_owned());
        }
        let out = subprocess::run(
            &self.sandbox,
            &Invocation {
                command: self.command.clone(),
                args,
                work_dir: work.path().to_path_buf(),
                // A server to talk to, or a database to download.
                needs_network: true,
                timeout: self.timeout,
            },
        )
        .await?;
        match out.status {
            Some(0) => {}
            Some(code) => {
                return Err(ScannerError::Upstream(format!(
                    "trivy exited {code}: {}",
                    out.stderr_tail.trim()
                )))
            }
            None => return Err(ScannerError::Crashed("trivy was killed".into())),
        }
        let doc = subprocess::parse_json(&out.stdout)?;
        Ok(Self::map(&doc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = include_str!("fixtures/trivy-sbom.json");

    #[test]
    fn the_observed_report_maps_to_vulnerability_findings_with_their_ids() {
        let doc: serde_json::Value = serde_json::from_str(REPORT).unwrap();
        let findings = TrivyScanner::map(&doc);
        assert_eq!(findings.len(), 3);
        let first = &findings[0];
        assert_eq!(first.scanner, "trivy");
        assert_eq!(first.code, ReasonCode::Vulnerability);
        assert_eq!(first.severity, Severity::High, "HIGH lower-cases to high");
        assert_eq!(first.reference.as_deref(), Some("CVE-2020-8203"));
        assert!(first.summary.contains("lodash"), "{}", first.summary);
        assert!(
            first.summary.contains("fixed in 4.17.19"),
            "{}",
            first.summary
        );
    }

    #[test]
    fn a_report_with_no_results_is_a_clean_answer() {
        let doc = serde_json::json!({ "SchemaVersion": 2, "Results": [] });
        assert!(TrivyScanner::map(&doc).is_empty());
        assert!(TrivyScanner::map(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn the_server_endpoint_becomes_the_client_mode_flag() {
        let s = TrivyScanner {
            command: PathBuf::from("trivy"),
            endpoint: "http://trivy:4954".into(),
            sandbox: Sandbox::default(),
            extract: ExtractPolicy::default(),
            timeout: Duration::from_secs(60),
        };
        let args = s.common_args();
        assert!(args
            .windows(2)
            .any(|w| w == ["--server", "http://trivy:4954"]));
        assert!(args.contains(&"--quiet".to_owned()));
    }
}
