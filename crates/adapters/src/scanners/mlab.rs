//! mlab.sh's CVE API as a [`FindingEnricher`] (RFC 0018 §6.3, phase 5).
//!
//! Enrichment, not detection: it runs after the scanners and attaches to
//! every `Vulnerability` finding that names a CVE — directly in its
//! `reference`, or through the `aliases` OSV records beside a GHSA id — the
//! CVSS score and vector, the EPSS probability and percentile, and whether
//! CISA lists the CVE as known-exploited. A KEV-listed CVE is raised to
//! `critical` whatever its CVSS: exploitation in the wild is the fact a
//! quarantine exists for. It never creates a finding, so it is never a
//! sensible member of `required_scanners` (config warns).
//!
//! `GET {base}/api/v1/cve/{id}` — observed against `https://vuln.mlab.sh` on
//! 2026-09-05: `cvss_score`, `cvss_severity`, `cvss_vector`, `epss_score`,
//! `epss_percentile`, `in_kev`, `kev_date_added`, `kev_due_date`. The
//! endpoint answers unauthenticated; a key, when configured, is sent as a
//! bearer token. One CVE id leaves this process per finding, nothing else.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, Severity};
use batlehub_core::ports::{FindingEnricher, ScannerError};

pub const NAME: &str = "mlab";
pub const DEFAULT_API: &str = "https://vuln.mlab.sh";

pub struct MlabEnricher {
    pub http: reqwest::Client,
    pub api_url: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

/// The CVE id a finding is about, if it is about one.
pub fn cve_of(f: &Finding) -> Option<String> {
    let is_cve = |s: &str| {
        let s = s.trim();
        s.len() > 8
            && s.starts_with("CVE-")
            && s[4..].chars().all(|c| c.is_ascii_digit() || c == '-')
    };
    if let Some(r) = f.reference.as_deref().filter(|r| is_cve(r)) {
        return Some(r.trim().to_owned());
    }
    f.raw
        .get("aliases")
        .and_then(|a| a.as_array())
        .and_then(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .find(|s| is_cve(s))
                .map(str::to_owned)
        })
}

impl MlabEnricher {
    async fn lookup(&self, cve: &str) -> Result<serde_json::Value, ScannerError> {
        let url = format!("{}/api/v1/cve/{cve}", self.api_url.trim_end_matches('/'));
        let mut req = self.http.get(&url).timeout(self.timeout);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ScannerError::Upstream(format!("mlab: {e}")))?;
        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(ScannerError::Upstream(
                "mlab: rate limited (429); no answer this scan — the policy's scanner_error mode applies until a rescan".into(),
            ));
        }
        if status.as_u16() == 404 {
            // A CVE the database does not know is not an error: nothing to
            // attach, the finding stands.
            return Ok(serde_json::Value::Null);
        }
        if !status.is_success() {
            return Err(ScannerError::Upstream(format!("mlab: HTTP {status}")));
        }
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| ScannerError::Output(format!("mlab: answer is not JSON: {e}")))
    }

    /// Apply one CVE record to one finding.
    fn apply(f: &mut Finding, record: &serde_json::Value) {
        let Some(obj) = record.as_object() else {
            return;
        };
        let pick = |k: &str| obj.get(k).cloned().unwrap_or(serde_json::Value::Null);
        let enrichment = serde_json::json!({
            "source": NAME,
            "cvss_score": pick("cvss_score"),
            "cvss_severity": pick("cvss_severity"),
            "cvss_vector": pick("cvss_vector"),
            "epss_score": pick("epss_score"),
            "epss_percentile": pick("epss_percentile"),
            "in_kev": pick("in_kev"),
            "kev_date_added": pick("kev_date_added"),
            "kev_due_date": pick("kev_due_date"),
        });
        if let Some(raw) = f.raw.as_object_mut() {
            raw.insert("enrichment".to_owned(), enrichment);
        } else {
            f.raw = serde_json::json!({ "enrichment": enrichment });
        }
        if obj.get("in_kev").and_then(|k| k.as_bool()) == Some(true) {
            f.severity = Severity::Critical;
            if !f.summary.contains("CISA KEV") {
                f.summary = format!("{} (CISA KEV: exploited in the wild)", f.summary);
            }
        }
    }
}

#[async_trait]
impl FindingEnricher for MlabEnricher {
    fn name(&self) -> &str {
        NAME
    }

    async fn enrich(&self, findings: &mut Vec<Finding>) -> Result<(), ScannerError> {
        // One lookup per CVE, however many findings cite it.
        let mut records: HashMap<String, serde_json::Value> = HashMap::new();
        for f in findings.iter() {
            if f.kind != FindingKind::Vulnerability {
                continue;
            }
            let Some(cve) = cve_of(f) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(slot) = records.entry(cve.clone()) {
                let record = self.lookup(&cve).await?;
                slot.insert(record);
            }
        }
        for f in findings.iter_mut() {
            if f.kind != FindingKind::Vulnerability {
                continue;
            }
            if let Some(record) = cve_of(f).and_then(|c| records.get(&c)) {
                if !record.is_null() {
                    Self::apply(f, record);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::ReasonCode;

    fn enricher(base: &str) -> MlabEnricher {
        MlabEnricher {
            http: reqwest::Client::new(),
            api_url: base.to_owned(),
            api_key: None,
            timeout: Duration::from_secs(5),
        }
    }

    fn vuln(reference: &str, aliases: &[&str]) -> Finding {
        Finding::new(
            "osv",
            FindingKind::Vulnerability,
            ReasonCode::Vulnerability,
            Severity::High,
            "GHSA-test",
        )
        .with_reference(reference)
        .with_raw(serde_json::json!({ "aliases": aliases }))
    }

    #[test]
    fn a_cve_is_read_from_the_reference_or_the_osv_aliases() {
        assert_eq!(
            cve_of(&vuln("CVE-2021-23337", &[])).as_deref(),
            Some("CVE-2021-23337")
        );
        assert_eq!(
            cve_of(&vuln("GHSA-35jh-r3h4-6jhm", &["CVE-2021-23337", "GHSA-x"])).as_deref(),
            Some("CVE-2021-23337")
        );
        assert_eq!(cve_of(&vuln("GHSA-only", &["GHSA-y"])), None);
    }

    #[tokio::test]
    async fn a_kev_listed_cve_is_raised_to_critical_and_the_record_attached_once_per_cve() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/api/v1/cve/CVE-2021-44228")
            .with_status(200)
            .with_body(
                r#"{"id":"CVE-2021-44228","cvss_score":10.0,"cvss_severity":"CRITICAL","cvss_vector":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H","epss_score":0.99999,"epss_percentile":1.0,"in_kev":true,"kev_date_added":"2021-12-10","kev_due_date":"2021-12-24"}"#,
            )
            .expect(1)
            .create_async()
            .await;
        let mut findings = vec![
            vuln("GHSA-jfh8-c2jp-5v3q", &["CVE-2021-44228"]),
            vuln("CVE-2021-44228", &[]),
            Finding::new(
                "postmortem",
                FindingKind::InstallHook,
                ReasonCode::InstallHook,
                Severity::Low,
                "hook",
            ),
        ];
        enricher(&server.url()).enrich(&mut findings).await.unwrap();
        m.assert_async().await;
        for f in &findings[..2] {
            assert_eq!(f.severity, Severity::Critical, "{f:?}");
            assert!(f.summary.contains("CISA KEV"));
            assert_eq!(f.raw["enrichment"]["epss_score"], 0.99999);
            assert_eq!(f.raw["enrichment"]["in_kev"], true);
        }
        assert_eq!(
            findings[2].severity,
            Severity::Low,
            "not a vulnerability: untouched"
        );
        assert!(findings[2].raw.get("enrichment").is_none());
    }

    #[tokio::test]
    async fn a_non_kev_cve_keeps_its_severity_and_an_unknown_one_is_left_alone() {
        let mut server = mockito::Server::new_async().await;
        let _known = server
            .mock("GET", "/api/v1/cve/CVE-2021-23337")
            .with_status(200)
            .with_body(r#"{"id":"CVE-2021-23337","cvss_score":7.2,"cvss_severity":"HIGH","epss_score":0.01,"epss_percentile":0.8,"in_kev":false}"#)
            .create_async()
            .await;
        let _unknown = server
            .mock("GET", "/api/v1/cve/CVE-2099-0001")
            .with_status(404)
            .create_async()
            .await;
        let mut findings = vec![vuln("CVE-2021-23337", &[]), vuln("CVE-2099-0001", &[])];
        enricher(&server.url()).enrich(&mut findings).await.unwrap();
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].raw["enrichment"]["cvss_score"], 7.2);
        assert!(findings[1].raw.get("enrichment").is_none());
    }

    #[tokio::test]
    async fn rate_limited_and_malformed_answers_leave_the_findings_as_they_were() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/v1/cve/CVE-2021-23337")
            .with_status(429)
            .create_async()
            .await;
        let mut findings = vec![vuln("CVE-2021-23337", &[])];
        match enricher(&server.url()).enrich(&mut findings).await {
            Err(ScannerError::Upstream(msg)) => assert!(msg.contains("429")),
            other => panic!("{other:?}"),
        }
        assert_eq!(findings[0].severity, Severity::High);
        server.reset();
        let _m = server
            .mock("GET", "/api/v1/cve/CVE-2021-23337")
            .with_status(200)
            .with_body("<html>")
            .create_async()
            .await;
        match enricher(&server.url()).enrich(&mut findings).await {
            Err(ScannerError::Output(_)) => {}
            other => panic!("{other:?}"),
        }
    }
}
