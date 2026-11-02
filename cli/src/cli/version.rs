use anyhow::Result;
use clap::Subcommand;

use super::lifecycle::{run as run_lifecycle, Verb};
use crate::api::BatleHubClient;
use crate::render::deprecated;

#[derive(Subcommand)]
pub enum VersionCommand {
    /// Yank a specific version (marks it unavailable but keeps it). Destructive:
    /// asks on a terminal, refuses without one unless given `--yes`
    Yank {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
        /// Version string
        version: String,
    },
    /// Unyank a previously yanked version
    Unyank {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
        /// Version string
        version: String,
    },
    /// Delete a version's artifact; the version number is then spent forever.
    /// Destructive: asks on a terminal, refuses without one unless given `--yes`
    Delete {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
        /// Version string
        version: String,
    },
    /// Pin a version so a retention run never reclaims it
    Pin {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
        /// Version string
        version: String,
    },
    /// Release a retention pin, letting the registry's policy apply again
    Unpin {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
        /// Version string
        version: String,
    },
}

/// The deprecated `version <verb> REG NAME VER`: warns, then runs
/// `package <verb> REG/NAME@VER`.
pub async fn run(cmd: VersionCommand, client: &BatleHubClient) -> Result<()> {
    let (verb, word, registry, name, version) = match cmd {
        VersionCommand::Yank {
            registry,
            name,
            version,
        } => (Verb::Yank, "yank", registry, name, version),
        VersionCommand::Unyank {
            registry,
            name,
            version,
        } => (Verb::Unyank, "unyank", registry, name, version),
        VersionCommand::Delete {
            registry,
            name,
            version,
        } => (Verb::Delete, "delete", registry, name, version),
        VersionCommand::Pin {
            registry,
            name,
            version,
        } => (Verb::Pin, "pin", registry, name, version),
        VersionCommand::Unpin {
            registry,
            name,
            version,
        } => (Verb::Unpin, "unpin", registry, name, version),
    };
    deprecated(
        &format!("version {word} <registry> <name> <version>"),
        &format!("package {word} <registry>/<name>@<version>"),
    );
    run_lifecycle(
        verb,
        &[format!("{registry}/{name}@{version}")],
        None,
        client,
    )
    .await
}
