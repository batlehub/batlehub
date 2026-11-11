//! mise's versions host, `mise-versions.jdx.dev` (RFC 0043).
//!
//! One document this client fetches: `data/<tool>.toml`, the version list mise
//! asks for before any backend's own. There is no artifact — the bytes mise
//! installs come from the registry that serves the tool's backend — so
//! `resolve_metadata` and `fetch_artifact` answer `NotFound`. The rest of the
//! host's surface (the install beacon, the GitHub mirror, the python index) is
//! answered in the web handler and never reaches here.
//!
//! The client's own headers are never forwarded: this client builds its own
//! requests, so mise's `x-mise-ci` and User-Agent stop at this instance.

use async_trait::async_trait;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::mise_versions::versions,
};

use super::http_client::{
    basic_auth_get, encode_path, fetch_text_document, new_http_client, UpstreamHttpOptions,
};

/// The host mise asks when nothing is configured.
pub const DEFAULT_BASE: &str = "https://mise-versions.jdx.dev";

pub struct MiseVersionsRegistryClient {
    http: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
}

impl MiseVersionsRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        Ok(Self {
            http: new_http_client(Some(5), opts)?,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            basic_auth: opts.basic_auth.clone(),
        })
    }

    async fn version_list(&self, tool: &str) -> Result<VersionDocument, CoreError> {
        let url = format!("{}/data/{}.toml", self.base_url, encode_path(tool));
        fetch_text_document(
            basic_auth_get(&self.http, &self.basic_auth, &url),
            &format!("version list for '{tool}'"),
            "application/toml",
        )
        .await
    }
}

#[async_trait]
impl RegistryClient for MiseVersionsRegistryClient {
    fn registry_type(&self) -> &str {
        "mise-versions"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Err(CoreError::NotFound(format!(
            "'{}' is a version list; its bytes come from the tool's own registry",
            pkg.name
        )))
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(CoreError::NotFound(format!(
            "'{}' is a version list; its bytes come from the tool's own registry",
            pkg.name
        )))
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        match kind {
            DocumentKind::Versions => self.version_list(package).await,
            other => Err(CoreError::NotSupported(format!(
                "the mise versions host has no '{other}' listing document"
            ))),
        }
    }

    /// Every version in the list, oldest first as the trait asks — the host
    /// writes them oldest first already.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let doc = self.version_list(package).await?;
        Ok(versions(doc.body.as_text().unwrap_or_default())
            .into_iter()
            .map(|(v, _)| v)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    const JQ: &str = "[versions]\n\"1.8.1\" = { created_at = 2025-07-01T11:50:06.000Z }\n\"1.8.2\" = { created_at = 2026-06-20T14:11:27.000Z }\n";

    #[tokio::test]
    async fn the_version_list_is_relayed_as_toml_text() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/data/jq.toml")
            .with_header("content-type", "application/toml")
            .with_body(JQ)
            .create_async()
            .await;
        let c = MiseVersionsRegistryClient::new(server.url(), &Default::default()).unwrap();
        let doc = c
            .fetch_version_document("jq", DocumentKind::Versions)
            .await
            .unwrap();
        assert_eq!(doc.content_type, "application/toml");
        assert_eq!(doc.body.as_text(), Some(JQ));
        assert_eq!(c.list_versions("jq").await.unwrap(), ["1.8.1", "1.8.2"]);
    }

    #[tokio::test]
    async fn an_unknown_tool_is_not_found_and_there_is_no_artifact() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/data/nope.toml")
            .with_status(404)
            .with_body("Tool \"data/nope\" not found")
            .create_async()
            .await;
        let c = MiseVersionsRegistryClient::new(server.url(), &Default::default()).unwrap();
        let err = c
            .fetch_version_document("nope", DocumentKind::Versions)
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "{err:?}");
        let pkg = PackageId::new("mv", "jq", "1.8.2");
        assert!(matches!(
            c.fetch_artifact(&pkg).await,
            Err(CoreError::NotFound(_))
        ));
        assert!(matches!(
            c.fetch_version_document("jq", DocumentKind::LATEST).await,
            Err(CoreError::NotSupported(_))
        ));
    }
}
