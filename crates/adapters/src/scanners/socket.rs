//! Socket.dev as an [`ArtifactScanner`] (RFC 0018 §6.3, phase 5): the
//! external, metered second opinion an estate opts into per registry.
//!
//! One call per coordinate: `POST {base}/v0/purl?alerts=true&compact=true`
//! with `{"components":[{"purl":…}]}`, authenticated the way Socket's own
//! CLI does — the API key as the Basic-auth user name and an empty
//! password. The answer is one JSON object per line (NDJSON), one per
//! artifact matched, each carrying `alerts: [{type, severity, category,
//! props}]`. The mapping below is by `type`, the one field Socket
//! documents as stable; a type this build does not know is reported as a
//! signal at the severity Socket gave it, never dropped.
//!
//! Coordinates only leave this process — never bytes, never a private
//! package's name: a registry in `local` mode is not scanned here (RFC 0018
//! §7), which `supports` cannot see and the worker enforces by not queueing
//! local coordinates at all.

use std::time::Duration;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, ReasonCode, RegistryKind, Severity};
use batlehub_core::ports::{ArtifactScanner, ScanInput, ScannerError};

pub const NAME: &str = "socket";
pub const DEFAULT_API: &str = "https://api.socket.dev";

/// Answers above this are not an answer.
const MAX_BODY: usize = 8 * 1024 * 1024;

pub struct SocketScanner {
    pub http: reqwest::Client,
    pub api_url: String,
    pub api_key: String,
    pub timeout: Duration,
}

impl SocketScanner {
    /// Socket's alert `type` → what it means to a verdict. The severity
    /// Socket attached (`low` … `critical`) is kept as the finding's own.
    fn classify(alert_type: &str) -> (FindingKind, ReasonCode) {
        match alert_type {
            "installScripts" | "gitDependency" | "httpDependency" => {
                (FindingKind::InstallHook, ReasonCode::InstallHook)
            }
            "didYouMean" | "typosquat" => (FindingKind::Typosquat, ReasonCode::TyposquatSuspect),
            "cve" | "vulnerability" | "criticalCVE" | "mediumCVE" | "mildCVE" => {
                (FindingKind::Vulnerability, ReasonCode::Vulnerability)
            }
            // `malware`, `obfuscatedFile`, `troll`, `telemetry`, `shellAccess`,
            // `networkAccess`, `envVars`, `filesystemAccess`, `binScriptConfusion`,
            // `shrinkwrap`, `unresolvedRequire`, … and anything newer.
            _ => (FindingKind::MalwareSignal, ReasonCode::MalwareSignal),
        }
    }

    fn map_artifact(&self, doc: &serde_json::Value) -> Vec<Finding> {
        let Some(alerts) = doc.get("alerts").and_then(|a| a.as_array()) else {
            return Vec::new();
        };
        alerts
            .iter()
            .filter_map(|a| {
                let ty = a.get("type").and_then(|t| t.as_str())?;
                let (kind, code) = Self::classify(ty);
                let severity = a
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .and_then(Severity::parse)
                    .unwrap_or(Severity::Medium);
                let category = a.get("category").and_then(|c| c.as_str()).unwrap_or("");
                let summary = if category.is_empty() {
                    format!("socket: {ty}")
                } else {
                    format!("socket: {ty} ({category})")
                };
                let mut f = Finding::new(NAME, kind, code, severity, summary).with_raw(a.clone());
                if let Some(note) = a
                    .get("props")
                    .and_then(|p| p.get("note").or_else(|| p.get("cveId")))
                    .and_then(|n| n.as_str())
                {
                    f.reference = Some(note.chars().take(120).collect());
                }
                Some(f)
            })
            .collect()
    }
}

#[async_trait]
impl ArtifactScanner for SocketScanner {
    fn name(&self) -> &str {
        NAME
    }

    fn supports(&self, kind: RegistryKind) -> bool {
        matches!(
            kind,
            RegistryKind::Npm
                | RegistryKind::Pypi
                | RegistryKind::Goproxy
                | RegistryKind::Maven
                | RegistryKind::Rubygems
                | RegistryKind::Cargo
                | RegistryKind::Nuget
        )
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let url = format!(
            "{}/v0/purl?alerts=true&compact=true",
            self.api_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.api_key, Some(""))
            .timeout(self.timeout)
            .json(&serde_json::json!({ "components": [{ "purl": input.purl }] }))
            .send()
            .await
            .map_err(|e| ScannerError::Upstream(format!("socket: {e}")))?;
        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(ScannerError::Upstream(
                "socket: rate limited (429); no answer this scan — the policy's scanner_error mode applies until a rescan".into(),
            ));
        }
        if !status.is_success() {
            return Err(ScannerError::Upstream(format!("socket: HTTP {status}")));
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| ScannerError::Upstream(format!("socket: reading the answer: {e}")))?;
        if body.len() > MAX_BODY {
            return Err(ScannerError::Output(format!(
                "socket: answer of {} bytes is over the {MAX_BODY}-byte cap",
                body.len()
            )));
        }
        let text = std::str::from_utf8(&body)
            .map_err(|_| ScannerError::Output("socket: answer is not UTF-8".into()))?;
        let mut findings = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let doc: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                ScannerError::Output(format!("socket: a line of the answer is not JSON: {e}"))
            })?;
            findings.extend(self.map_artifact(&doc));
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{PackageId, PackageMetadata};

    fn scanner(base: &str) -> SocketScanner {
        SocketScanner {
            http: reqwest::Client::new(),
            api_url: base.to_owned(),
            api_key: "sk-test".into(),
            timeout: Duration::from_secs(5),
        }
    }

    fn input() -> ScanInput {
        ScanInput {
            package: PackageMetadata::minimal(
                PackageId::new("npm-sec", "left-pad", "1.3.1"),
                serde_json::Value::Null,
            ),
            kind: RegistryKind::Npm,
            purl: "pkg:npm/left-pad@1.3.1".into(),
            artifact: None,
            sbom: None,
            listing: None,
        }
    }

    #[tokio::test]
    async fn alerts_map_by_type_and_the_key_travels_as_basic_auth() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/v0/purl?alerts=true&compact=true")
            .match_header("authorization", "Basic c2stdGVzdDo=")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "components": [{ "purl": "pkg:npm/left-pad@1.3.1" }]
            })))
            .with_status(200)
            .with_body(concat!(
                r#"{"type":"npm","name":"left-pad","version":"1.3.1","alerts":[{"type":"installScripts","severity":"high","category":"supplyChainRisk"},{"type":"didYouMean","severity":"middle","category":"supplyChainRisk","props":{"note":"leftpad"}},{"type":"malware","severity":"critical","category":"supplyChainRisk"},{"type":"someNewAlert","severity":"low","category":"quality"}]}"#,
                "\n",
                r#"{"type":"npm","name":"other","version":"1.0.0","alerts":[]}"#,
                "\n"
            ))
            .create_async()
            .await;
        let findings = scanner(&server.url()).scan(&input()).await.unwrap();
        m.assert_async().await;
        assert_eq!(findings.len(), 4, "{findings:?}");
        let codes: Vec<ReasonCode> = findings.iter().map(|f| f.code).collect();
        assert!(codes.contains(&ReasonCode::InstallHook));
        assert!(codes.contains(&ReasonCode::TyposquatSuspect));
        assert!(codes.contains(&ReasonCode::MalwareSignal));
        assert!(findings
            .iter()
            .any(|f| f.code == ReasonCode::MalwareSignal && f.severity == Severity::Critical));
        assert!(findings
            .iter()
            .any(|f| f.summary.contains("someNewAlert") && f.severity == Severity::Low));
        assert!(findings.iter().all(|f| f.scanner == "socket"));
        assert_eq!(
            findings
                .iter()
                .find(|f| f.code == ReasonCode::TyposquatSuspect)
                .and_then(|f| f.reference.as_deref()),
            Some("leftpad")
        );
    }

    #[tokio::test]
    async fn rate_limited_and_malformed_answers_are_not_answers() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/v0/purl?alerts=true&compact=true")
            .with_status(429)
            .create_async()
            .await;
        match scanner(&server.url()).scan(&input()).await {
            Err(ScannerError::Upstream(msg)) => assert!(msg.contains("429"), "{msg}"),
            other => panic!("{other:?}"),
        }
        server.reset();
        let _m = server
            .mock("POST", "/v0/purl?alerts=true&compact=true")
            .with_status(200)
            .with_body("{\"alerts\": [\n<html>not json")
            .create_async()
            .await;
        match scanner(&server.url()).scan(&input()).await {
            Err(ScannerError::Output(_)) => {}
            other => panic!("{other:?}"),
        }
    }
}
