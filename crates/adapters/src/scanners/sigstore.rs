//! Provenance as an [`ArtifactScanner`] (RFC 0018 §6.3): npm's Sigstore
//! attestations, and whether Rekor's transparency log has them.
//!
//! npm announces a version's attestations in the packument —
//! `versions[v].dist.attestations = { url, provenance: { predicateType } }`
//! — and serves the bundles at that URL. Each bundle's
//! `verificationMaterial.tlogEntries[].logIndex` names an entry in Rekor;
//! an entry that Rekor does not have is a bundle nobody logged, which is
//! `PROVENANCE_INVALID`. A version with no attestations at all is
//! `PROVENANCE_MISSING` on the kinds `require_for` names, and nothing
//! elsewhere: most of the registry has never published provenance and a
//! finding on every one of them would be noise the operator learns to
//! ignore.
//!
//! This is an *existence and inclusion* check, not a full Sigstore
//! verification: the certificate chain, the SCT and the DSSE signature are
//! not verified here (that is `npm audit signatures`' job, and the client's
//! own cosign). What it answers is the question the quarantine asks — was a
//! provenance published and logged for this version — with the log as the
//! witness.
//!
//! The lookups are made by the worker's own HTTP client, not under the
//! sandbox: nothing hostile is opened, and the coordinates are public.

use std::time::Duration;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, ReasonCode, RegistryKind, Severity};
use batlehub_core::ports::{ArtifactScanner, ScanInput, ScannerError};

pub const NAME: &str = "sigstore";
pub const DEFAULT_REKOR: &str = "https://rekor.sigstore.dev";

pub struct SigstoreScanner {
    pub http: reqwest::Client,
    pub rekor_url: String,
    /// The kinds a missing attestation is a finding on (`require_for`).
    pub require_for: Vec<RegistryKind>,
    pub timeout: Duration,
}

/// What the packument says about a version's attestations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announced {
    None,
    Some { url: String },
}

impl SigstoreScanner {
    /// Read `versions[v].dist.attestations` out of a packument.
    pub fn announced(packument: &serde_json::Value, version: &str) -> Announced {
        let att = packument
            .get("versions")
            .and_then(|v| v.get(version))
            .and_then(|v| v.get("dist"))
            .and_then(|d| d.get("attestations"));
        match att.and_then(|a| a.get("url")).and_then(|u| u.as_str()) {
            Some(url) if !url.is_empty() => Announced::Some {
                url: url.to_owned(),
            },
            _ => Announced::None,
        }
    }

    /// The Rekor log indexes the attestation bundles name.
    pub fn log_indexes(bundles: &serde_json::Value) -> Vec<u64> {
        let mut out = Vec::new();
        for a in bundles
            .get("attestations")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
        {
            for entry in a
                .get("bundle")
                .and_then(|b| b.get("verificationMaterial"))
                .and_then(|m| m.get("tlogEntries"))
                .and_then(|t| t.as_array())
                .into_iter()
                .flatten()
            {
                let idx = entry.get("logIndex").and_then(|i| match i {
                    serde_json::Value::Number(n) => n.as_u64(),
                    serde_json::Value::String(s) => s.parse().ok(),
                    _ => None,
                });
                if let Some(i) = idx {
                    out.push(i);
                }
            }
        }
        out
    }

    async fn rekor_has(&self, index: u64) -> Result<bool, ScannerError> {
        let url = format!(
            "{}/api/v1/log/entries?logIndex={index}",
            self.rekor_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .get(&url)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| ScannerError::Upstream(format!("rekor: {e}")))?;
        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            other => Err(ScannerError::Upstream(format!("rekor answered {other}"))),
        }
    }
}

#[async_trait]
impl ArtifactScanner for SigstoreScanner {
    fn name(&self) -> &str {
        NAME
    }

    /// npm is the one registry that announces attestations in its listing;
    /// the generic cosign-bundle check for `X-Artifact-Signature` waits on a
    /// registry that carries one.
    fn supports(&self, kind: RegistryKind) -> bool {
        kind == RegistryKind::Npm
    }

    fn needs_listing(&self) -> bool {
        true
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let Some(listing) = input.listing.as_ref().and_then(|l| l.body.as_json()) else {
            return Err(ScannerError::Unsupported(
                "sigstore needs the packument and it was not provided".into(),
            ));
        };
        let version = input.package.id.version.as_str();
        let url = match Self::announced(listing, version) {
            Announced::None => {
                if self.require_for.contains(&input.kind) {
                    return Ok(vec![Finding::new(
                        NAME,
                        FindingKind::Provenance,
                        ReasonCode::ProvenanceMissing,
                        Severity::High,
                        format!("no provenance attestation is published for version {version}"),
                    )]);
                }
                return Ok(Vec::new());
            }
            Announced::Some { url } => url,
        };
        let bundles: serde_json::Value = self
            .http
            .get(&url)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| ScannerError::Upstream(format!("attestations: {e}")))?
            .error_for_status()
            .map_err(|e| ScannerError::Upstream(format!("attestations: {e}")))?
            .json()
            .await
            .map_err(|e| ScannerError::Output(format!("attestations are not JSON: {e}")))?;
        let indexes = Self::log_indexes(&bundles);
        if indexes.is_empty() {
            return Ok(vec![Finding::new(
                NAME,
                FindingKind::Provenance,
                ReasonCode::ProvenanceInvalid,
                Severity::High,
                "the attestation bundle names no transparency-log entry",
            )
            .with_raw(bundles)]);
        }
        let mut findings = Vec::new();
        for index in indexes {
            if !self.rekor_has(index).await? {
                findings.push(Finding::new(
                    NAME,
                    FindingKind::Provenance,
                    ReasonCode::ProvenanceInvalid,
                    Severity::Critical,
                    format!("Rekor has no entry at log index {index}, which the bundle cites"),
                ));
            }
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcements_are_read_from_the_versions_dist_block() {
        let packument = serde_json::json!({
            "versions": {
                "1.0.0": { "dist": { "tarball": "x" } },
                "1.1.0": { "dist": { "tarball": "y", "attestations": {
                    "url": "https://registry.npmjs.org/-/npm/v1/attestations/left-pad@1.1.0",
                    "provenance": { "predicateType": "https://slsa.dev/provenance/v1" } } } }
            }
        });
        assert_eq!(
            SigstoreScanner::announced(&packument, "1.0.0"),
            Announced::None
        );
        assert_eq!(
            SigstoreScanner::announced(&packument, "1.1.0"),
            Announced::Some {
                url: "https://registry.npmjs.org/-/npm/v1/attestations/left-pad@1.1.0".into()
            }
        );
        assert_eq!(
            SigstoreScanner::announced(&packument, "9.9.9"),
            Announced::None
        );
    }

    #[test]
    fn log_indexes_are_read_from_every_bundles_tlog_entries() {
        let bundles = serde_json::json!({
            "attestations": [
                { "predicateType": "https://slsa.dev/provenance/v1",
                  "bundle": { "verificationMaterial": { "tlogEntries": [ { "logIndex": "12345" } ] } } },
                { "predicateType": "https://github.com/npm/attestation/tree/main/specs/publish/v0.1",
                  "bundle": { "verificationMaterial": { "tlogEntries": [ { "logIndex": 67890 } ] } } }
            ]
        });
        assert_eq!(SigstoreScanner::log_indexes(&bundles), [12345, 67890]);
        assert!(SigstoreScanner::log_indexes(&serde_json::json!({})).is_empty());
    }

    #[tokio::test]
    async fn a_missing_attestation_is_a_finding_only_where_required() {
        let listing = batlehub_core::ports::VersionDocument::json(serde_json::json!({
            "versions": { "1.0.0": { "dist": { "tarball": "x" } } }
        }));
        let meta = batlehub_core::entities::PackageMetadata::minimal(
            batlehub_core::entities::PackageId::new("npm", "left-pad", "1.0.0"),
            serde_json::Value::Null,
        );
        let input = |listing: batlehub_core::ports::VersionDocument| ScanInput {
            package: meta.clone(),
            kind: RegistryKind::Npm,
            purl: "pkg:npm/left-pad@1.0.0".into(),
            artifact: None,
            sbom: None,
            listing: Some(listing),
        };
        let required = SigstoreScanner {
            http: reqwest::Client::new(),
            rekor_url: DEFAULT_REKOR.into(),
            require_for: vec![RegistryKind::Npm],
            timeout: Duration::from_secs(5),
        };
        let f = required.scan(&input(listing.clone())).await.unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, ReasonCode::ProvenanceMissing);

        let optional = SigstoreScanner {
            require_for: vec![],
            ..required
        };
        assert!(optional.scan(&input(listing)).await.unwrap().is_empty());
    }
}
