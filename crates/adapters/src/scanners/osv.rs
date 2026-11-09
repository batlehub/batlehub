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
use batlehub_core::ports::{
    ArtifactScanner, LookupFailure, ScanInput, ScannerError, VulnerabilityScanner,
};
use batlehub_core::services::scanners::as_known_malicious;

/// The most component PURLs one artifact's SBOM may make the worker ask
/// about. An `Upstream` or `Extracted` SBOM is attacker-supplied, so its
/// length is not trusted (RFC 0041 T20); above this the scan is `Permanent`.
pub const MAX_COMPONENT_PURLS: usize = 2_000;

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

    fn is_advisory_db(&self) -> bool {
        true
    }

    fn unavailable_for(&self) -> Option<std::time::Duration> {
        self.inner.unavailable_for()
    }

    /// A partial answer is not a clean one: any PURL that failed — the
    /// package's own or a component's — fails the scan, with the worst
    /// class among them (RFC 0041 §6.8).
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let mut purls = vec![input.purl.clone()];
        if let Some(sbom) = &input.sbom {
            let components = component_purls(sbom);
            if components.len() > MAX_COMPONENT_PURLS {
                tracing::warn!(purl = %input.purl, components = components.len(), "osv: SBOM over the component cap; not scanned");
                return Err(ScannerError::Permanent(format!(
                    "the SBOM lists {} component PURLs, over the cap of {MAX_COMPONENT_PURLS}",
                    components.len()
                )));
            }
            for p in components {
                if !purls.contains(&p) {
                    purls.push(p);
                }
            }
        }
        let answer = self
            .inner
            .query_batch(&purls)
            .await
            .map_err(|e| ScannerError::Transient(e.to_string()))?;
        if let Some(e) = worst_failure(&answer.failed) {
            if matches!(e, ScannerError::Permanent(_)) {
                tracing::warn!(purl = %input.purl, error = %e, "osv: permanent lookup failure");
            }
            return Err(e);
        }
        Ok(answer
            .matches
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
                    "aliases": m.aliases,
                }))
            })
            .map(as_known_malicious)
            .collect())
    }
}

/// The failure a batch comes to: permanent before rate limited before
/// transient — a permanent one will not clear by waiting, and a rate limit
/// carries the longest `Retry-After` given.
fn worst_failure(failed: &[(String, LookupFailure)]) -> Option<ScannerError> {
    let (purl, first) = failed.first()?;
    if let Some((purl, LookupFailure::Permanent(e))) = failed
        .iter()
        .find(|(_, f)| matches!(f, LookupFailure::Permanent(_)))
    {
        return Some(ScannerError::Permanent(format!("{purl}: {e}")));
    }
    let limited: Vec<Option<std::time::Duration>> = failed
        .iter()
        .filter_map(|(_, f)| match f {
            LookupFailure::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        })
        .collect();
    if !limited.is_empty() {
        return Some(ScannerError::RateLimited {
            retry_after: limited.into_iter().flatten().max(),
        });
    }
    Some(ScannerError::Transient(format!(
        "{} PURL(s) got no answer, first {purl}: {first}",
        failed.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{PackageId, PackageMetadata, Severity};
    use batlehub_core::error::CoreError;
    use batlehub_core::ports::{BatchAnswer, OsvMatch};
    use std::sync::Mutex;

    struct Fake {
        asked: Mutex<Vec<String>>,
        fail: bool,
        /// PURLs answering a 429.
        throttled: Vec<String>,
    }

    #[async_trait]
    impl VulnerabilityScanner for Fake {
        async fn query_batch(&self, purls: &[String]) -> Result<BatchAnswer, CoreError> {
            self.asked.lock().unwrap().extend(purls.iter().cloned());
            if self.fail {
                return Err(CoreError::Registry("osv down".into()));
            }
            let failed = purls
                .iter()
                .filter(|p| self.throttled.contains(p))
                .map(|p| {
                    (
                        p.clone(),
                        LookupFailure::RateLimited {
                            retry_after: Some(std::time::Duration::from_secs(30)),
                        },
                    )
                })
                .collect();
            let matches = purls
                .iter()
                .filter(|p| p.contains("left-pad"))
                .map(|p| OsvMatch {
                    purl: p.clone(),
                    osv_id: "GHSA-1".into(),
                    severity: Severity::High,
                    summary: "bad".into(),
                    fixed_version: Some("1.3.2".into()),
                    aliases: vec![],
                })
                .collect();
            Ok(BatchAnswer { matches, failed })
        }
    }

    fn fake() -> Arc<Fake> {
        Arc::new(Fake {
            asked: Mutex::new(vec![]),
            fail: false,
            throttled: vec![],
        })
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
        let fake = fake();
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

    /// RFC 0041 §4.5: a `MAL-` match (here malicious through an alias, so
    /// `parse_vulns` named it by its `MAL-` id) is `KNOWN_MALICIOUS`.
    #[tokio::test]
    async fn a_mal_match_is_known_malicious_and_critical() {
        struct Mal;
        #[async_trait]
        impl VulnerabilityScanner for Mal {
            async fn query_batch(&self, purls: &[String]) -> Result<BatchAnswer, CoreError> {
                Ok(BatchAnswer {
                    matches: vec![OsvMatch {
                        purl: purls[0].clone(),
                        osv_id: "MAL-2023-1000".into(),
                        severity: Severity::Critical,
                        summary: "malicious".into(),
                        fixed_version: None,
                        aliases: vec!["GHSA-gjjj-m5j2-h9fv".into()],
                    }],
                    failed: vec![],
                })
            }
        }
        let findings = OsvArtifactScanner::new(Arc::new(Mal))
            .scan(&input(None))
            .await
            .unwrap();
        let f = &findings[0];
        assert_eq!(f.kind, FindingKind::MalwareSignal);
        assert_eq!(f.code, ReasonCode::KnownMalicious);
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.reference.as_deref(), Some("MAL-2023-1000"));
        assert_eq!(f.raw["aliases"][0], "GHSA-gjjj-m5j2-h9fv");
    }

    /// An unreachable OSV is not a clean answer.
    #[tokio::test]
    async fn an_upstream_failure_is_an_error_not_an_empty_answer() {
        let s = OsvArtifactScanner::new(Arc::new(Fake {
            asked: Mutex::new(vec![]),
            fail: true,
            throttled: vec![],
        }));
        let err = s.scan(&input(None)).await.unwrap_err();
        assert!(err.is_deferrable(), "{err}");
    }

    #[test]
    fn coverage_follows_purl_types() {
        let s = OsvArtifactScanner::new(fake());
        assert!(s.supports(RegistryKind::Npm));
        assert!(s.supports(RegistryKind::Cargo));
        assert!(!s.supports(RegistryKind::Generic));
        assert!(!s.supports(RegistryKind::Deb));
    }

    /// RFC 0041 §6.8: one failed component fails the scan, deferrably.
    #[tokio::test]
    async fn one_failed_component_is_a_deferrable_error() {
        let s = OsvArtifactScanner::new(Arc::new(Fake {
            asked: Mutex::new(vec![]),
            fail: false,
            throttled: vec!["pkg:npm/dep@1.0.0".into()],
        }));
        let sbom = serde_json::json!({ "components": [{ "purl": "pkg:npm/dep@1.0.0" }] });
        let err = s.scan(&input(Some(sbom))).await.unwrap_err();
        assert!(
            matches!(err, ScannerError::RateLimited { retry_after: Some(d) } if d.as_secs() == 30),
            "{err:?}"
        );
        assert!(err.is_deferrable() && s.is_advisory_db());
    }

    /// T20: an SBOM over the cap is not asked about at all.
    #[tokio::test]
    async fn an_sbom_over_the_component_cap_is_permanent() {
        let fake = fake();
        let s = OsvArtifactScanner::new(Arc::clone(&fake) as Arc<dyn VulnerabilityScanner>);
        let components: Vec<_> = (0..=MAX_COMPONENT_PURLS)
            .map(|i| serde_json::json!({ "purl": format!("pkg:npm/c{i}@1.0.0") }))
            .collect();
        let err = s
            .scan(&input(Some(
                serde_json::json!({ "components": components }),
            )))
            .await
            .unwrap_err();
        assert!(matches!(err, ScannerError::Permanent(_)), "{err:?}");
        assert!(!err.is_deferrable());
        assert!(fake.asked.lock().unwrap().is_empty());
    }
}
