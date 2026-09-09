//! The verdict endpoint (RFC 0018 §4.2), as `batlehub why` and `batlehub
//! wait` read it.

use anyhow::{bail, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use super::BatleHubClient;

/// One finding, as the endpoint serialises it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingView {
    pub scanner: String,
    pub kind: String,
    pub code: String,
    pub severity: String,
    #[serde(default)]
    pub reference: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub available_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinateView {
    pub registry: String,
    pub name: String,
    pub version: String,
}

/// `GET /api/v1/verdicts/{registry}/{name}/{version}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictView {
    pub package: CoordinateView,
    pub state: String,
    #[serde(default)]
    pub reason_codes: Vec<String>,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    pub policy_ref: String,
    #[serde(default)]
    pub available_at: Option<String>,
    pub evaluated_at: String,
    #[serde(default)]
    pub last_scanned_at: Option<String>,
    #[serde(default)]
    pub scanners_done: Vec<String>,
    #[serde(default)]
    pub findings_withheld: bool,
    /// RFC 0019 phase 2 — on a forge, what the ref the caller named resolved
    /// to. Absent for every package registry and for a coordinate that was
    /// already a commit.
    #[serde(default)]
    pub requested_ref: Option<String>,
    #[serde(default)]
    pub ref_kind: Option<String>,
    #[serde(default)]
    pub resolved_commit: Option<String>,
}

impl VerdictView {
    /// Whether the artifact is streamed under this verdict.
    pub fn is_served(&self) -> bool {
        matches!(self.state.as_str(), "allowed" | "warned")
    }

    /// Whether waiting can help (§4.2 *What a CI pipeline sees*): a hold that
    /// names a clock lifts on its own; `denied`, or a hold with no
    /// `available_at` (an unscanned version, an undated one), does not.
    pub fn waiting_helps(&self) -> bool {
        self.state == "quarantined"
            && (self.available_at.is_some()
                || self
                    .reason_codes
                    .iter()
                    .any(|c| c == "SCAN_PENDING" || c == "SCANNER_ERROR"))
    }

    /// The reason the wait cannot help, for the exit-1 message.
    pub fn why_waiting_cannot_help(&self) -> String {
        if self.state == "denied" {
            return format!("denied ({})", self.reason_codes.join(", "));
        }
        if self.reason_codes.iter().any(|c| c == "TIMESTAMP_MISSING") {
            return "held open-ended: the upstream did not date this version \
                    (TIMESTAMP_MISSING), and no derivation will supply one on a clock"
                .to_owned();
        }
        format!(
            "{} ({}) with no clock to wait on",
            self.state,
            self.reason_codes.join(", ")
        )
    }
}

/// `POST …/rescan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescanView {
    pub queued: bool,
    pub trigger: String,
}

/// `<registry>:<name>@<version>`, with the `@` that starts a scoped npm name
/// left alone: the version is what follows the *last* `@`.
pub fn parse_coordinate(spec: &str) -> Result<(String, String, String)> {
    let Some((registry, rest)) = spec.split_once(':') else {
        bail!("expected <registry>:<name>@<version>, got '{spec}'");
    };
    let Some(at) = rest.rfind('@').filter(|i| *i > 0) else {
        bail!("expected <registry>:<name>@<version>, got '{spec}' (no version)");
    };
    let (name, version) = (&rest[..at], &rest[at + 1..]);
    if registry.is_empty() || name.is_empty() || version.is_empty() {
        bail!("expected <registry>:<name>@<version>, got '{spec}'");
    }
    Ok((registry.to_owned(), name.to_owned(), version.to_owned()))
}

impl BatleHubClient {
    /// The verdict, or `None` when there is none to read — a version never
    /// seen, a registry without a security profile, or a caller without
    /// `quarantine:read`; the server does not say which.
    pub async fn get_verdict(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<VerdictView>> {
        let path = format!(
            "/api/v1/verdicts/{}/{}/{}",
            super::auth::percent_encode(registry),
            super::auth::percent_encode(name),
            super::auth::percent_encode(version)
        );
        let resp = self.send(self.request(Method::GET, &path)).await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(super::expect_ok(resp).await?))
    }

    /// Who pulled the version inside `since` (RFC 0018 §4.2): the JSON
    /// report, or the CSV as the server renders it.
    pub async fn pullers(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        since: &str,
        csv: bool,
    ) -> Result<String> {
        let path = format!(
            "/api/v1/verdicts/{}/{}/{}/pullers?since={}&format={}",
            super::auth::percent_encode(registry),
            super::auth::percent_encode(name),
            super::auth::percent_encode(version),
            super::auth::percent_encode(since),
            if csv { "csv" } else { "json" }
        );
        let resp = self.send(self.request(Method::GET, &path)).await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("{status}: {body}");
        }
        Ok(body)
    }

    /// What one identity pulled inside `since` (RFC 0018 §4.2): the transpose
    /// of [`Self::pullers`].
    ///
    /// Returns the body verbatim, as its sibling does, so `--csv` prints what
    /// the server rendered rather than something the CLI re-derived — the two
    /// could then disagree, and the CSV is the artefact an auditor keeps.
    pub async fn audit_pulls(
        &self,
        identity: &str,
        registry: Option<&str>,
        package: Option<&str>,
        since: &str,
        csv: bool,
    ) -> Result<String> {
        let mut path = format!(
            "/api/v1/audit/pulls?identity={}&since={}&format={}",
            super::auth::percent_encode(identity),
            super::auth::percent_encode(since),
            if csv { "csv" } else { "json" }
        );
        if let Some(r) = registry {
            path.push_str(&format!("&registry={}", super::auth::percent_encode(r)));
        }
        if let Some(p) = package {
            path.push_str(&format!("&package={}", super::auth::percent_encode(p)));
        }
        let resp = self.send(self.request(Method::GET, &path)).await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("{status}: {body}");
        }
        Ok(body)
    }

    /// The admin listing (RFC 0018 phase 5), raw: the caller prints it.
    pub async fn list_verdicts(
        &self,
        registry: &str,
        state: Option<&str>,
        limit: Option<u64>,
    ) -> Result<serde_json::Value> {
        let mut path = format!(
            "/api/v1/admin/verdicts?registry={}",
            super::auth::percent_encode(registry)
        );
        if let Some(s) = state {
            path.push_str(&format!("&state={}", super::auth::percent_encode(s)));
        }
        if let Some(l) = limit {
            path.push_str(&format!("&limit={l}"));
        }
        let resp = self.send(self.request(Method::GET, &path)).await?;
        super::expect_ok(resp).await
    }

    /// `POST /api/v1/admin/verdicts/{rescan|backfill}`.
    pub async fn bulk_scan(
        &self,
        op: &str,
        registry: &str,
        state: Option<&str>,
    ) -> Result<serde_json::Value> {
        let path = format!("/api/v1/admin/verdicts/{op}");
        let body = serde_json::json!({ "registry": registry, "state": state });
        let resp = self
            .send(self.request(Method::POST, &path).json(&body))
            .await?;
        super::expect_ok(resp).await
    }

    pub async fn rescan(&self, registry: &str, name: &str, version: &str) -> Result<RescanView> {
        let path = format!(
            "/api/v1/verdicts/{}/{}/{}/rescan",
            super::auth::percent_encode(registry),
            super::auth::percent_encode(name),
            super::auth::percent_encode(version)
        );
        self.post_no_body_json(&path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_splits_on_the_last_at() {
        assert_eq!(
            parse_coordinate("npm:left-pad@1.3.1").unwrap(),
            ("npm".into(), "left-pad".into(), "1.3.1".into())
        );
        assert_eq!(
            parse_coordinate("npm:@scope/name@1.0.0").unwrap(),
            ("npm".into(), "@scope/name".into(), "1.0.0".into())
        );
        assert_eq!(
            parse_coordinate("maven:com.acme:lib@1.0").unwrap(),
            ("maven".into(), "com.acme:lib".into(), "1.0".into())
        );
        assert!(parse_coordinate("left-pad@1.3.1").is_err());
        assert!(parse_coordinate("npm:left-pad").is_err());
        assert!(parse_coordinate("npm:@scope/name").is_err());
    }

    fn view(state: &str, codes: &[&str], available: Option<&str>) -> VerdictView {
        VerdictView {
            package: CoordinateView {
                registry: "r".into(),
                name: "p".into(),
                version: "1".into(),
            },
            state: state.into(),
            reason_codes: codes.iter().map(|c| (*c).to_owned()).collect(),
            findings: vec![],
            policy_ref: "r/default".into(),
            available_at: available.map(str::to_owned),
            evaluated_at: "2026-09-04T00:00:00Z".into(),
            last_scanned_at: None,
            scanners_done: vec![],
            findings_withheld: false,
            requested_ref: None,
            ref_kind: None,
            resolved_commit: None,
        }
    }

    #[test]
    fn waiting_helps_only_on_a_clock_or_a_pending_scan() {
        assert!(view(
            "quarantined",
            &["MIN_AGE_NOT_MET"],
            Some("2026-09-05T00:00:00Z")
        )
        .waiting_helps());
        assert!(view("quarantined", &["SCAN_PENDING"], None).waiting_helps());
        assert!(!view("quarantined", &["TIMESTAMP_MISSING"], None).waiting_helps());
        assert!(!view("denied", &["BLOCK_LIST"], None).waiting_helps());
        assert!(view("warned", &["VULNERABILITY"], None).is_served());
    }
}
