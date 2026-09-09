//! GuardDog as an [`ArtifactScanner`] (RFC 0018 §6.3): an optional second
//! opinion on npm, PyPI and Go archives, where both it and postmortem are
//! configured. Not required for the default profile.
//!
//! ```text
//! guarddog <npm|pypi|go> scan <path> --output-format json
//! ```
//!
//! # Read, not observed
//!
//! GuardDog is not provisioned on the developer machine this was built on,
//! so the JSON shape below is DataDog's documented one — one object per
//! scanned package, `result.results` keyed by rule name, each a list of
//! `{location, code, message}` — and the mapping from rule names to finding
//! kinds is by the family a rule's name declares (`*-install-script*`,
//! `*obfuscat*`, `*exfiltrat*`, `*typosquat*`). The first CI run with the
//! binary on the worker image is what turns this into an observation; until
//! then the adapter answers `SCANNER_UNSUPPORTED` for an output it cannot
//! read rather than an empty success.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, ReasonCode, RegistryKind, Severity};
use batlehub_core::ports::{ArtifactScanner, ScanInput, ScannerError};

use super::extract::{extract_to, ExtractPolicy};
use super::subprocess::{self, Invocation, Sandbox};

pub const NAME: &str = "guarddog";

pub struct GuarddogScanner {
    pub command: PathBuf,
    /// The ecosystems this deployment enables it for; empty means all three.
    pub ecosystems: Vec<String>,
    pub sandbox: Sandbox,
    pub extract: ExtractPolicy,
    pub timeout: Duration,
}

impl GuarddogScanner {
    fn ecosystem(kind: RegistryKind) -> Option<&'static str> {
        match kind {
            RegistryKind::Npm => Some("npm"),
            RegistryKind::Pypi => Some("pypi"),
            RegistryKind::Goproxy => Some("go"),
            _ => None,
        }
    }

    /// The finding family a GuardDog rule belongs to, from its name.
    fn classify(rule: &str) -> (FindingKind, ReasonCode, Severity) {
        let r = rule.to_ascii_lowercase();
        if r.contains("install") || r.contains("exec-base64") || r.contains("cmd-overwrite") {
            (
                FindingKind::InstallHook,
                ReasonCode::InstallHook,
                Severity::High,
            )
        } else if r.contains("typosquat") {
            (
                FindingKind::Typosquat,
                ReasonCode::TyposquatSuspect,
                Severity::Medium,
            )
        } else if r.contains("exfiltrat") || r.contains("steganography") || r.contains("shady") {
            (
                FindingKind::MalwareSignal,
                ReasonCode::MalwareSignal,
                Severity::High,
            )
        } else {
            (
                FindingKind::MalwareSignal,
                ReasonCode::MalwareSignal,
                Severity::Medium,
            )
        }
    }

    pub fn map(doc: &serde_json::Value) -> Result<Vec<Finding>, ScannerError> {
        // One object, or a list of them when several paths were scanned.
        let objects: Vec<&serde_json::Value> = match doc {
            serde_json::Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        let mut out = Vec::new();
        let mut recognised = false;
        for obj in objects {
            let Some(results) = obj
                .get("result")
                .and_then(|r| r.get("results"))
                .and_then(|r| r.as_object())
            else {
                continue;
            };
            recognised = true;
            for (rule, hits) in results {
                let Some(hits) = hits.as_array() else {
                    continue;
                };
                for hit in hits {
                    let (kind, code, severity) = Self::classify(rule);
                    let message = hit.get("message").and_then(|m| m.as_str()).unwrap_or(rule);
                    let location = hit.get("location").and_then(|l| l.as_str()).unwrap_or("");
                    let summary = if location.is_empty() {
                        format!("{rule}: {message}")
                    } else {
                        format!("{rule}: {message} ({location})")
                    };
                    out.push(
                        Finding::new(NAME, kind, code, severity, summary)
                            .with_reference(rule.clone())
                            .with_raw(hit.clone()),
                    );
                }
            }
        }
        if !recognised {
            return Err(ScannerError::Output(
                "guarddog output carried no `result.results` object".into(),
            ));
        }
        Ok(out)
    }
}

#[async_trait]
impl ArtifactScanner for GuarddogScanner {
    fn name(&self) -> &str {
        NAME
    }

    fn supports(&self, kind: RegistryKind) -> bool {
        match Self::ecosystem(kind) {
            Some(eco) => self.ecosystems.is_empty() || self.ecosystems.iter().any(|e| e == eco),
            None => false,
        }
    }

    fn needs_artifact(&self) -> bool {
        true
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let Some(artifact) = input.artifact.as_ref() else {
            return Err(ScannerError::Unsupported(
                "guarddog needs the artifact bytes and none were provided".into(),
            ));
        };
        let eco = Self::ecosystem(input.kind).ok_or_else(|| {
            ScannerError::Unsupported(format!("guarddog does not scan {}", input.kind))
        })?;
        let work = subprocess::work_dir("guarddog")?;
        let tree = work.path().join("package");
        extract_to(artifact, &tree, &self.extract)?;
        let out = subprocess::run(
            &self.sandbox,
            &Invocation {
                command: self.command.clone(),
                args: vec![
                    eco.to_owned(),
                    "scan".to_owned(),
                    tree.to_string_lossy().into_owned(),
                    "--output-format".to_owned(),
                    "json".to_owned(),
                ],
                work_dir: work.path().to_path_buf(),
                needs_network: false,
                timeout: self.timeout,
            },
        )
        .await?;
        match out.status {
            // GuardDog exits 1 when it found something, like postmortem.
            Some(0) | Some(1) => {}
            Some(code) => {
                return Err(ScannerError::Crashed(format!(
                    "guarddog exited {code}: {}",
                    out.stderr_tail.trim()
                )))
            }
            None => return Err(ScannerError::Crashed("guarddog was killed".into())),
        }
        let doc = subprocess::parse_json(&out.stdout)?;
        Self::map(&doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_output_maps_by_rule_family() {
        let doc = serde_json::json!({
            "dependency": "evil-pad",
            "version": "1.3.1",
            "result": {
                "issues": 2,
                "errors": {},
                "results": {
                    "npm-install-script": [
                        { "location": "package.json", "code": "curl … | sh",
                          "message": "install script runs a network command" }
                    ],
                    "npm-exfiltrate-sensitive-data": [
                        { "location": "setup.js", "code": "…", "message": "reads AWS keys" }
                    ],
                    "npm-obfuscation": []
                },
                "path": "/work/package"
            }
        });
        let findings = GuarddogScanner::map(&doc).unwrap();
        assert_eq!(findings.len(), 2);
        assert!(findings
            .iter()
            .any(|f| f.code == ReasonCode::InstallHook && f.severity == Severity::High));
        assert!(findings.iter().any(|f| f.code == ReasonCode::MalwareSignal
            && f.reference.as_deref() == Some("npm-exfiltrate-sensitive-data")));
    }

    #[test]
    fn an_unrecognised_shape_is_an_output_error_not_a_clean_answer() {
        let err = GuarddogScanner::map(&serde_json::json!({ "ok": true })).unwrap_err();
        assert!(matches!(err, ScannerError::Output(_)));
    }

    #[test]
    fn coverage_follows_the_configured_ecosystems() {
        let s = GuarddogScanner {
            command: PathBuf::from("guarddog"),
            ecosystems: vec!["npm".into()],
            sandbox: Sandbox::default(),
            extract: ExtractPolicy::default(),
            timeout: Duration::from_secs(60),
        };
        assert!(s.supports(RegistryKind::Npm));
        assert!(!s.supports(RegistryKind::Pypi), "not enabled here");
        assert!(!s.supports(RegistryKind::Cargo), "never covered");
    }
}
