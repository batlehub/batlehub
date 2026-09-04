//! The OSV lookup as a scanner (RFC 0018 §6.3): the existing
//! `VulnerabilityScanner` (OSV by PURL) wrapped, so the periodic SBOM re-check
//! and the quarantine share one client and one parser.
//!
//! It asks about the coordinate's own PURL and, when a CycloneDX SBOM has
//! been recorded for the artifact, about every component PURL in it — the
//! same set the periodic pass queries.

use std::sync::Arc;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, ReasonCode, RegistryKind};
use batlehub_core::ports::{ArtifactScanner, ScanInput, ScannerError, VulnerabilityScanner};

pub struct OsvArtifactScanner {
    pub inner: Arc<dyn VulnerabilityScanner>,
}

impl OsvArtifactScanner {
    pub fn new(inner: Arc<dyn VulnerabilityScanner>) -> Self {
        Self { inner }
    }
}

/// The distinct `purl` strings of a CycloneDX document's `components`.
fn component_purls(document: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(components) = document.get("components").and_then(|c| c.as_array()) {
        for comp in components {
            if let Some(purl) = comp.get("purl").and_then(|p| p.as_str()) {
                if !purl.is_empty() && !out.iter().any(|p| p == purl) {
                    out.push(purl.to_owned());
                }
            }
        }
    }
    out
}

#[async_trait]
impl ArtifactScanner for OsvArtifactScanner {
    fn name(&self) -> &str {
        "osv"
    }

    /// OSV has an ecosystem for every kind with a real PURL type; the
    /// path-proxy family and `generic` have none to ask about.
    fn supports(&self, kind: RegistryKind) -> bool {
        !matches!(
            kind,
            RegistryKind::Deb
                | RegistryKind::Rpm
                | RegistryKind::Pacman
                | RegistryKind::Jetbrains
                | RegistryKind::Generic
                | RegistryKind::Nodedist
                | RegistryKind::Sdkman
                | RegistryKind::JetbrainsMarketplace
                | RegistryKind::Openvsx
                | RegistryKind::VscodeMarketplace
                | RegistryKind::Terraform
        )
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let mut purls = vec![input.purl.clone()];
        if let Some(sbom) = &input.sbom {
            for p in component_purls(sbom) {
                if !purls.contains(&p) {
                    purls.push(p);
                }
            }
        }
        let matches = self
            .inner
            .query_batch(&purls)
            .await
            .map_err(|e| ScannerError::Upstream(e.to_string()))?;
        Ok(matches
            .into_iter()
            .map(|m| {
                Finding::new(
                    "osv",
                    FindingKind::Vulnerability,
                    ReasonCode::Vulnerability,
                    m.severity,
                    m.summary.clone(),
                )
                .with_reference(m.osv_id.clone())
                .with_raw(serde_json::json!({
                    "purl": m.purl,
                    "fixed_version": m.fixed_version,
                }))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{PackageId, PackageMetadata, Severity};
    use batlehub_core::error::CoreError;
    use batlehub_core::ports::OsvMatch;
    use std::sync::Mutex;

    struct Fake {
        asked: Mutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait]
    impl VulnerabilityScanner for Fake {
        async fn query_batch(&self, purls: &[String]) -> Result<Vec<OsvMatch>, CoreError> {
            self.asked.lock().unwrap().extend(purls.iter().cloned());
            if self.fail {
                return Err(CoreError::Registry("osv down".into()));
            }
            Ok(purls
                .iter()
                .filter(|p| p.contains("left-pad"))
                .map(|p| OsvMatch {
                    purl: p.clone(),
                    osv_id: "GHSA-1".into(),
                    severity: Severity::High,
                    summary: "bad".into(),
                    fixed_version: Some("1.3.2".into()),
                })
                .collect())
        }
    }

    fn input(sbom: Option<serde_json::Value>) -> ScanInput {
        ScanInput {
            package: PackageMetadata::minimal(
                PackageId::new("npm", "left-pad", "1.3.1"),
                serde_json::Value::Null,
            ),
            kind: RegistryKind::Npm,
            purl: "pkg:npm/left-pad@1.3.1".into(),
            artifact: None,
            sbom,
            listing: None,
        }
    }

    #[tokio::test]
    async fn the_coordinate_and_its_sbom_components_are_asked_about() {
        let fake = Arc::new(Fake {
            asked: Mutex::new(vec![]),
            fail: false,
        });
        let s = OsvArtifactScanner::new(Arc::clone(&fake) as Arc<dyn VulnerabilityScanner>);
        let sbom = serde_json::json!({ "components": [
            { "purl": "pkg:npm/dep@1.0.0" }, { "purl": "pkg:npm/left-pad@1.3.1" }
        ]});
        let findings = s.scan(&input(Some(sbom))).await.unwrap();
        assert_eq!(fake.asked.lock().unwrap().len(), 2, "deduplicated");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, ReasonCode::Vulnerability);
        assert_eq!(findings[0].reference.as_deref(), Some("GHSA-1"));
        assert_eq!(findings[0].severity, Severity::High);
    }

    /// An unreachable OSV is not a clean answer.
    #[tokio::test]
    async fn an_upstream_failure_is_an_error_not_an_empty_answer() {
        let s = OsvArtifactScanner::new(Arc::new(Fake {
            asked: Mutex::new(vec![]),
            fail: true,
        }));
        let err = s.scan(&input(None)).await.unwrap_err();
        assert_eq!(err.class(), "upstream");
    }

    #[test]
    fn coverage_follows_purl_types() {
        let s = OsvArtifactScanner::new(Arc::new(Fake {
            asked: Mutex::new(vec![]),
            fail: false,
        }));
        assert!(s.supports(RegistryKind::Npm));
        assert!(s.supports(RegistryKind::Cargo));
        assert!(!s.supports(RegistryKind::Generic));
        assert!(!s.supports(RegistryKind::Deb));
    }
}
