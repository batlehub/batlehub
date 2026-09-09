pub mod http_client;
pub mod offline;
pub mod readme_image;
pub mod ssrf;
pub use http_client::{
    apply_upstream_options, apply_upstream_tls, basic_auth_get, cache_control, parse_http_date,
    percent_encode, to_registry_error, upstream_auth_headers, UpstreamHttpOptions,
};
pub use readme_image::HttpReadmeImageFetcher;

pub mod fanout;
pub use fanout::FanoutRegistryClient;

pub mod forge_api;
pub use forge_api::BudgetedApi;

#[cfg(feature = "registry-github")]
pub mod github;
#[cfg(feature = "registry-github")]
pub use github::GithubRegistryClient;

#[cfg(feature = "registry-forgejo")]
pub mod forgejo;
#[cfg(feature = "registry-forgejo")]
pub use forgejo::ForgejoRegistryClient;

#[cfg(feature = "registry-gitlab")]
pub mod gitlab;
#[cfg(feature = "registry-gitlab")]
pub use gitlab::GitlabRegistryClient;

#[cfg(any(
    feature = "registry-deb",
    feature = "registry-rpm",
    feature = "registry-pacman",
    feature = "registry-jetbrains",
    feature = "registry-generic"
))]
pub mod path_proxy;
#[cfg(any(
    feature = "registry-deb",
    feature = "registry-rpm",
    feature = "registry-pacman",
    feature = "registry-jetbrains",
    feature = "registry-generic"
))]
pub use path_proxy::PathProxyRegistryClient;

#[cfg(feature = "registry-npm")]
pub mod npm;
#[cfg(feature = "registry-npm")]
pub use npm::NpmRegistryClient;

#[cfg(feature = "registry-nuget")]
pub mod nuget;
#[cfg(feature = "registry-nuget")]
pub use nuget::NugetRegistryClient;

#[cfg(feature = "registry-cargo")]
pub mod cargo;
#[cfg(feature = "registry-cargo")]
pub use cargo::CargoRegistryClient;

#[cfg(feature = "registry-openvsx")]
pub mod openvsx;
#[cfg(feature = "registry-openvsx")]
pub use openvsx::OpenVsxRegistryClient;

#[cfg(feature = "registry-goproxy")]
pub mod goproxy;
#[cfg(feature = "registry-goproxy")]
pub use goproxy::GoProxyRegistryClient;

#[cfg(feature = "registry-jetbrains-marketplace")]
pub mod jetbrains_marketplace;
#[cfg(feature = "registry-jetbrains-marketplace")]
pub use jetbrains_marketplace::JetbrainsMarketplaceRegistryClient;

#[cfg(feature = "registry-vscode-marketplace")]
pub mod vscode_marketplace;
#[cfg(feature = "registry-vscode-marketplace")]
pub use vscode_marketplace::VsCodeMarketplaceRegistryClient;

#[cfg(feature = "registry-maven")]
pub mod maven;
#[cfg(feature = "registry-maven")]
pub use maven::MavenRegistryClient;

#[cfg(feature = "registry-terraform")]
pub mod terraform;
#[cfg(feature = "registry-terraform")]
pub use terraform::TerraformRegistryClient;

#[cfg(feature = "registry-rubygems")]
pub mod rubygems;
#[cfg(feature = "registry-rubygems")]
pub use rubygems::RubyGemsRegistryClient;

#[cfg(feature = "registry-composer")]
pub mod composer;
#[cfg(feature = "registry-composer")]
pub use composer::ComposerRegistryClient;

#[cfg(feature = "registry-pypi")]
pub mod pypi;
#[cfg(feature = "registry-pypi")]
pub use pypi::PypiRegistryClient;

#[cfg(feature = "registry-conda")]
pub mod conda;
#[cfg(feature = "registry-conda")]
pub use conda::CondaRegistryClient;

#[cfg(feature = "registry-nodedist")]
pub mod nodedist;
#[cfg(feature = "registry-nodedist")]
pub use nodedist::NodeDistRegistryClient;

#[cfg(feature = "registry-sdkman")]
pub mod sdkman;
#[cfg(feature = "registry-sdkman")]
pub use sdkman::SdkmanRegistryClient;

#[cfg(all(test, feature = "registry-github", feature = "registry-forgejo"))]
mod provenance_tests {
    //! RFC 0019 decision 8 and §10: `PROVENANCE_UNVERIFIABLE` is GitLab's
    //! release evidence and nothing else. The GitHub and Forgejo clients
    //! have every provenance path exercised here, and none of them may
    //! produce it — a guarantee a reader of either client would otherwise
    //! have to re-derive by reading both.

    use batlehub_core::entities::ForgeProvenance;
    use batlehub_core::ports::ForgeRegistry;

    use super::forgejo::ForgejoRegistryClient;
    use super::github::GithubRegistryClient;
    use super::http_client::UpstreamHttpOptions;

    /// Every answer the two clients can give, for a commit and for an asset.
    async fn answers(server: &mut mockito::Server, verification: &str) -> Vec<ForgeProvenance> {
        let commit_body = format!(r#"{{"sha":"abc","commit":{{"verification":{verification}}}}}"#);
        let _gh = server
            .mock("GET", "/repos/o/r/commits/abc")
            .with_status(200)
            .with_body(&commit_body)
            .expect_at_least(0)
            .create_async()
            .await;
        let _fj = server
            .mock("GET", "/api/v1/repos/o/r/git/commits/abc")
            .with_status(200)
            .with_body(&commit_body)
            .expect_at_least(0)
            .create_async()
            .await;
        let opts = UpstreamHttpOptions::default();
        let gh = GithubRegistryClient::new(server.url(), &opts).unwrap();
        let fj = ForgejoRegistryClient::new(server.url(), &opts).unwrap();
        vec![
            gh.provenance("o/r", "abc", None).await.unwrap(),
            fj.provenance("o/r", "abc", None).await.unwrap(),
        ]
    }

    #[tokio::test]
    async fn neither_github_nor_forgejo_ever_reports_unverifiable() {
        for verification in [
            r#"{"verified":true,"reason":"valid"}"#,
            r#"{"verified":false,"reason":"gpg.error.not_signed_commit"}"#,
            r#"{"verified":false,"reason":"unknown_key"}"#,
            "null",
        ] {
            let mut server = mockito::Server::new_async().await;
            for answer in answers(&mut server, verification).await {
                assert!(
                    !matches!(answer, ForgeProvenance::Unverifiable { .. }),
                    "{verification} produced {answer:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_verified_signature_is_verified_and_an_unsigned_commit_is_missing() {
        let mut server = mockito::Server::new_async().await;
        for answer in answers(&mut server, r#"{"verified":true,"reason":"valid"}"#).await {
            assert!(
                matches!(answer, ForgeProvenance::Verified { .. }),
                "{answer:?}"
            );
        }
        let mut server = mockito::Server::new_async().await;
        for answer in answers(
            &mut server,
            r#"{"verified":false,"reason":"gpg.error.not_signed_commit"}"#,
        )
        .await
        {
            assert_eq!(answer, ForgeProvenance::Missing);
        }
        // A signature that failed is a different fact from no signature.
        let mut server = mockito::Server::new_async().await;
        for answer in answers(&mut server, r#"{"verified":false,"reason":"unknown_key"}"#).await {
            assert!(
                matches!(answer, ForgeProvenance::Invalid { .. }),
                "{answer:?}"
            );
        }
    }

    /// GitHub's attestation store, as api.github.com answers it: `200` with
    /// an empty array when there is none (confirmed 2026-09-04), and `404`
    /// on a GitHub Enterprise below 3.13 — both `Missing`.
    #[tokio::test]
    async fn an_asset_attestation_is_missing_when_empty_and_when_absent() {
        let mut server = mockito::Server::new_async().await;
        let digest = format!("sha256:{}", "a".repeat(64));
        let _empty = server
            .mock("GET", format!("/repos/o/r/attestations/{digest}").as_str())
            .with_status(200)
            .with_body(r#"{"attestations":[]}"#)
            .create_async()
            .await;
        let gh = GithubRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        assert_eq!(
            gh.provenance("o/r", "abc", Some(&digest)).await.unwrap(),
            ForgeProvenance::Missing
        );

        let mut server = mockito::Server::new_async().await;
        let _ghes = server
            .mock("GET", format!("/repos/o/r/attestations/{digest}").as_str())
            .with_status(404)
            .create_async()
            .await;
        let gh = GithubRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        assert_eq!(
            gh.provenance("o/r", "abc", Some(&digest)).await.unwrap(),
            ForgeProvenance::Missing,
            "a GHES instance with no attestation endpoint reports missing, not an error"
        );

        let mut server = mockito::Server::new_async().await;
        let _one = server
            .mock("GET", format!("/repos/o/r/attestations/{digest}").as_str())
            .with_status(200)
            .with_body(r#"{"attestations":[{"bundle":{}}]}"#)
            .create_async()
            .await;
        let gh = GithubRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        assert!(matches!(
            gh.provenance("o/r", "abc", Some(&digest)).await.unwrap(),
            ForgeProvenance::Verified { .. }
        ));
    }
}
