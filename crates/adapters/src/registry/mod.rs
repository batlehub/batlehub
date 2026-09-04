pub mod http_client;
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
