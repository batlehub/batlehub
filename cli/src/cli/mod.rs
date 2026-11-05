pub mod admin;
pub mod auth;
pub mod authz;
pub mod check;
pub mod completion;
pub mod config_cmd;
pub mod download;
pub mod groups;
pub mod java;
pub mod journeys;
pub mod lifecycle;
pub mod mise;
pub mod owner;
pub mod package;
pub mod proxy;
pub mod publish;
pub mod registry;
pub mod security;
pub mod setup;
pub mod vsx;
pub mod why;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "batlehub-cli",
    about = "BatleHub CLI — interact with a BatleHub registry server",
    version
)]
pub struct Cli {
    /// Config profile to use (from ~/.config/batlehub/config.toml)
    #[arg(long, short = 'P', global = true, env = "BATLEHUB_PROFILE")]
    pub profile: Option<String>,

    /// Override server URL
    #[arg(long, global = true, env = "BATLEHUB_SERVER")]
    pub server: Option<String>,

    /// Override auth token
    #[arg(long, global = true, env = "BATLEHUB_TOKEN")]
    pub token: Option<String>,

    /// Default registry: for a bare package name and for every registry filter
    #[arg(long, short = 'r', global = true, env = "BATLEHUB_REGISTRY")]
    pub registry: Option<String>,

    /// Output shape: table (default), json, csv (listings) or raw (`auth token`)
    #[arg(long, global = true, env = "BATLEHUB_FORMAT", value_enum)]
    pub format: Option<crate::render::Format>,

    /// Same as `--format json`
    #[arg(long, global = true)]
    pub json: bool,

    /// Answer yes to the confirmation a destructive command asks for
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Profiles: the server, credential and default registry this CLI uses
    Profile {
        #[command(subcommand)]
        cmd: config_cmd::ProfileCommand,
    },
    /// List, show and suggest registries
    Registry {
        #[command(subcommand)]
        cmd: registry::RegistryCommand,
    },
    /// Packages and their versions: list, consult, and the version lifecycle
    Package {
        #[command(subcommand)]
        cmd: package::PackageCommand,
    },
    /// A package's owners
    Owner {
        #[command(subcommand)]
        cmd: owner::OwnerCommand,
    },
    /// A package's visibility
    Visibility {
        #[command(subcommand)]
        cmd: groups::VisibilityCmd,
    },
    /// Package- and version-tier grants (RFC 0017)
    Grant {
        #[command(subcommand)]
        cmd: groups::GrantCmd,
    },
    /// Team namespace claims
    Namespace {
        #[command(subcommand)]
        cmd: groups::NamespaceCmd,
    },
    /// Publish an artifact to a local/hybrid registry
    Publish(publish::PublishArgs),
    /// Download a file through the proxy cache (warms path-addressed registries)
    Download(download::DownloadArgs),
    /// Why a version is not pullable, or not yet: every cause, in the order the download checks them
    Why(why::WhyArgs),
    /// Would this project's pinned dependencies install? Reads the lockfiles here; exit 4 on any ⛔
    Check(check::CheckArgs),
    /// Your own state: identity, quota, downloads, advisories, namespaces
    Me {
        #[command(subcommand)]
        cmd: journeys::MeCmd,
    },
    /// Wait for a held version to become servable; exit 4 when waiting cannot help, 8 on timeout
    Wait(security::WaitArgs),
    /// Verdicts: who pulled a version, and the registry's stored verdicts (RFC 0018)
    Verdict {
        #[command(subcommand)]
        cmd: security::VerdictsCommand,
    },
    /// The audit log: query, purge, export, verify, and what an identity pulled
    Audit {
        #[command(subcommand)]
        cmd: groups::AuditCmd,
    },
    /// Authorization: check, explain, and shadow mode (RFC 0015 §4.8)
    Authz {
        #[command(subcommand)]
        cmd: authz::AuthzCommand,
    },
    /// Authentication: log in and out, tokens, whoami
    Auth {
        #[command(subcommand)]
        cmd: auth::AuthCommand,
    },
    /// Detect project type and configure package managers for this server
    Setup {
        #[command(subcommand)]
        cmd: setup::SetupCommand,
    },
    /// Air-gap commands: plan a mise.lock as a bill of materials (RFC 0008)
    Mise {
        #[command(subcommand)]
        cmd: mise::MiseCommand,
    },
    /// The local gallery proxy for editors that cannot send a credential
    /// (RFC 0011 §4.4)
    Proxy {
        #[command(subcommand)]
        cmd: proxy::ProxyCommand,
    },
    /// A registry's VSIX signature: a seed for `[registries.vsx_signing]`,
    /// and verifying a download against the served archive and key.
    Vsx {
        #[command(subcommand)]
        cmd: vsx::VsxCommand,
    },
    /// Package and version policy [admin]
    Policy {
        #[command(subcommand)]
        cmd: journeys::PolicyCmd,
    },
    /// Gate exemptions [admin]
    Exemption {
        #[command(subcommand)]
        cmd: journeys::ExemptionCmd,
    },
    /// The upstream audit: what upstream no longer lists (RFC 0014) [admin]
    Upstream {
        #[command(subcommand)]
        cmd: journeys::UpstreamCmd,
    },
    /// Blocked users [admin]
    User {
        #[command(subcommand)]
        cmd: groups::UserCmd,
    },
    /// Rate-limit quotas [admin]
    Quota {
        #[command(subcommand)]
        cmd: groups::QuotaCmd,
    },
    /// Blocked IP addresses [admin]
    IpBlock {
        #[command(subcommand)]
        cmd: admin::IpBlockCommand,
    },
    /// The server: configuration, statistics, health [admin]
    Server {
        #[command(subcommand)]
        cmd: groups::ServerCmd,
    },
    /// A registry's cache [admin]
    Cache {
        #[command(subcommand)]
        cmd: groups::CacheCmd,
    },
    /// Run a registry's configured release imports now (RFC 0021) [admin]
    Import {
        /// The registry that will hold the versions
        registry: String,
        /// Import this tag instead of what the configuration selects
        #[arg(long)]
        tag: Option<String>,
        /// Only the imports whose `repo` is this one
        #[arg(long)]
        repo: Option<String>,
    },
    /// Retention over a registry's published versions [admin]
    Retention {
        #[command(subcommand)]
        cmd: groups::RetentionCmd,
    },
    /// The global banner [admin]
    Banner {
        #[command(subcommand)]
        cmd: groups::BannerCmd,
    },
    /// Who pulled a flagged version (RFC 0002) [admin]
    Exposure {
        #[command(subcommand)]
        cmd: groups::ExposureCmd,
    },
    /// Imported air-gap bundles (RFC 0008) [admin]
    Bundle {
        #[command(subcommand)]
        cmd: groups::BundleCmd,
    },
    /// The air gap's miss log (RFC 0008) [admin]
    AirGap {
        #[command(subcommand)]
        cmd: groups::AirGapCmd,
    },
    /// Vulnerability flags (RFC 0002): list [admin]; create and delete as a flag source
    Flag {
        #[command(subcommand)]
        cmd: journeys::FlagCmd,
    },
    /// Data-subject requests (RFC 0036) [admin]
    Gdpr {
        #[command(subcommand)]
        cmd: groups::GdprCmd,
    },
    /// Software bills of materials [admin]
    Sbom {
        #[command(subcommand)]
        cmd: groups::SbomCmd,
    },
    /// Notification channels and subscriptions [admin]
    Notification {
        #[command(subcommand)]
        cmd: admin::NotificationsCommand,
    },
    /// The Java engine: inspect, fix, generate and rename Java over a real
    /// language server, with no editor (`java --help` for its verbs)
    Java(java::JavaArgs),
    /// Launch interactive TUI
    Tui,
    /// Print shell completion script to stdout
    Completion {
        /// Target shell
        #[arg(value_enum)]
        shell: Shell,
    },
}
