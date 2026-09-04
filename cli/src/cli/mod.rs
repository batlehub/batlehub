pub mod admin;
pub mod auth;
pub mod authz;
pub mod config_cmd;
pub mod download;
pub mod mise;
pub mod owner;
pub mod package;
pub mod publish;
pub mod registry;
pub mod security;
pub mod setup;
pub mod version;

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

    /// Default registry name
    #[arg(long, short = 'r', global = true, env = "BATLEHUB_REGISTRY")]
    pub registry: Option<String>,

    /// Output raw JSON instead of pretty tables
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// List and inspect registries
    Registry {
        #[command(subcommand)]
        cmd: registry::RegistryCommand,
    },
    /// List and inspect packages
    Package {
        #[command(subcommand)]
        cmd: package::PackageCommand,
    },
    /// Yank, delete, or pin specific versions against retention
    Version {
        #[command(subcommand)]
        cmd: version::VersionCommand,
    },
    /// Manage package owners
    Owners {
        #[command(subcommand)]
        cmd: owner::OwnerCommand,
    },
    /// Air-gap commands: plan a mise.lock as a bill of materials (RFC 0008)
    Mise {
        #[command(subcommand)]
        cmd: mise::MiseCommand,
    },
    /// Publish an artifact to a local/hybrid registry
    Publish(publish::PublishArgs),
    /// Download a file through the proxy cache (warms path-addressed registries)
    Download(download::DownloadArgs),
    /// Authentication commands (tokens, whoami)
    Auth {
        #[command(subcommand)]
        cmd: auth::AuthCommand,
    },
    /// Explain authorization decisions and inspect shadow mode (RFC 0015 §4.8)
    Authz {
        #[command(subcommand)]
        cmd: authz::AuthzCommand,
    },
    /// Admin operations (quota, ip-block, config, cache, banner, audit)
    Admin {
        #[command(subcommand)]
        cmd: admin::AdminCommand,
    },
    /// Manage CLI configuration
    Config {
        #[command(subcommand)]
        cmd: config_cmd::ConfigCommand,
    },
    /// Detect project type and print registry setup instructions
    Setup {
        #[command(subcommand)]
        cmd: setup::SetupCommand,
    },
    /// Explain why a version is held, denied or warned (RFC 0018)
    Why(security::WhyArgs),
    /// Wait for a held version to become servable; exit 1 when waiting cannot help, 2 on timeout
    Wait(security::WaitArgs),
    /// Launch interactive TUI
    Tui,
    /// Print shell completion script to stdout
    Completion {
        /// Target shell
        #[arg(value_enum)]
        shell: Shell,
    },
}
