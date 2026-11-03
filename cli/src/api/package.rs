use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::auth::percent_encode;
use super::BatleHubClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageListResponse {
    pub items: Vec<PackageSummary>,
    pub total: usize,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSummary {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
    pub status: PackageStatus,
    pub access_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PackageStatus {
    Available,
    Blocked { reason: String },
}

#[derive(Debug, Serialize)]
pub struct PackageQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub page: u64,
    pub per_page: u64,
}

/// One version's README, as the explore endpoint returns it.
///
/// Only the fields the CLI prints or reasons about: the response carries the
/// rendered HTML too, and a terminal has no use for it — `format=source` is
/// what this asks for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadmeResponse {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub requested_version: Option<String>,
    pub is_fallback: bool,
    pub format: String,
    pub source: String,
    pub package_level: bool,
    pub stored: bool,
    pub freshness: Option<String>,
    pub truncated: bool,
    pub source_text: Option<String>,
    pub extracted_at: String,
}

#[derive(Debug, Serialize)]
pub struct ReadmeQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Always `source`: markdown in a terminal is readable, and rendering it to
    /// ANSI is a separate concern (RFC 0007 §4.2).
    pub format: &'static str,
    /// `skip` maps to `--no-upstream`, for a caller who wants the answer this
    /// instance can give without asking anyone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<&'static str>,
}

impl BatleHubClient {
    pub async fn list_packages(&self, query: PackageQuery) -> Result<PackageListResponse> {
        self.get_with_params("/api/v1/packages", &query).await
    }

    /// One page of a package's versions from the explore detail endpoint,
    /// the upstream's included unless `upstream` is false (RFC 0040 §6.5).
    /// Read as JSON: the spec owns the row shape, so it is not re-declared.
    pub async fn package_versions_page(
        &self,
        registry: &str,
        name: &str,
        page: u64,
        per_page: Option<u64>,
        upstream: bool,
    ) -> Result<serde_json::Value> {
        #[derive(Serialize)]
        struct Q {
            page: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            per_page: Option<u64>,
            upstream: &'static str,
        }
        let path = format!(
            "/api/v1/explore/packages/{}/{}",
            percent_encode(registry),
            percent_encode(name)
        );
        let q = Q {
            page,
            per_page,
            upstream: if upstream { "auto" } else { "skip" },
        };
        self.get_with_params(&path, &q).await
    }

    /// One version's explore row (`selected`), the package's `default_version`
    /// when `version` is `None` (RFC 0040 §4.8 `package show`).
    pub async fn package_detail(
        &self,
        registry: &str,
        name: &str,
        version: Option<&str>,
    ) -> Result<serde_json::Value> {
        #[derive(Serialize)]
        struct Q<'a> {
            per_page: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            version: Option<&'a str>,
        }
        let path = format!(
            "/api/v1/explore/packages/{}/{}",
            percent_encode(registry),
            percent_encode(name)
        );
        self.get_with_params(
            &path,
            &Q {
                per_page: 1,
                version,
            },
        )
        .await
    }

    pub async fn package_readme(
        &self,
        registry: &str,
        name: &str,
        query: ReadmeQuery,
    ) -> Result<ReadmeResponse> {
        // The name is percent-encoded because a scoped npm package is
        // `@scope/pkg` and the slash would otherwise split the path segment.
        let path = format!(
            "/api/v1/explore/packages/{}/{}/readme",
            percent_encode(registry),
            percent_encode(name)
        );
        self.get_with_params(&path, &query).await
    }
}
