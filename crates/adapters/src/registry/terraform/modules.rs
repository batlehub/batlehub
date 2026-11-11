use super::{models, CoreError, PackageId, TerraformRegistryClient};
use crate::registry::encode_path;
use models::{TfModuleVersions, TfProviderVersions, TfVersionDetail};

/// Fetch the `published_at` timestamp for a specific version.
///
/// For **modules**: calls `GET /v1/modules/{ns}/{name}/{provider}/{version}`.
/// For **providers**: calls `GET /v1/providers/{ns}/{type}/{version}`.
///
/// Returns `None` when the endpoint is unsupported (404) or returns no timestamp.
pub(super) async fn fetch_version_published_at(
    client: &TerraformRegistryClient,
    pkg: &PackageId,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let base = client.base_url.trim_end_matches('/');
    let url = format!(
        "{base}/v1/{}/{}",
        encode_path(&pkg.name),
        encode_path(&pkg.version)
    );
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let detail: TfVersionDetail = resp.json().await.ok()?;
    detail
        .published_at
        .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
}

/// Parse the body of a `GET /v1/{package}/versions` response.
/// `package` must start with `"providers/"` or `"modules/"`.
pub(super) fn parse_versions(package: &str, body: &[u8]) -> Result<Vec<String>, CoreError> {
    if package.starts_with("providers/") {
        let parsed: TfProviderVersions = serde_json::from_slice(body)
            .map_err(|e| CoreError::Registry(format!("parsing provider versions: {e}")))?;
        Ok(parsed.versions.into_iter().map(|v| v.version).collect())
    } else if package.starts_with("modules/") {
        let parsed: TfModuleVersions = serde_json::from_slice(body)
            .map_err(|e| CoreError::Registry(format!("parsing module versions: {e}")))?;
        Ok(parsed
            .modules
            .into_iter()
            .flat_map(|m| m.versions.into_iter().map(|v| v.version))
            .collect())
    } else {
        Err(CoreError::Registry(format!(
            "terraform: package '{package}' must start with 'providers/' or 'modules/'"
        )))
    }
}
