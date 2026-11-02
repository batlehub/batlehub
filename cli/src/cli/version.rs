use anyhow::Result;
use clap::Subcommand;

use crate::api::BatleHubClient;
use crate::confirm::confirm;
use crate::render::done;

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

pub async fn run(cmd: VersionCommand, client: &BatleHubClient) -> Result<()> {
    match cmd {
        VersionCommand::Yank {
            registry,
            name,
            version,
        } => {
            confirm("yank", &[format!("{registry}/{name}@{version}")], None)?;
            client.yank_version(&registry, &name, &version).await?;
            done(&format!("Yanked {registry}/{name}@{version}"));
        }
        VersionCommand::Unyank {
            registry,
            name,
            version,
        } => {
            client.unyank_version(&registry, &name, &version).await?;
            done(&format!("Unyanked {registry}/{name}@{version}"));
        }
        VersionCommand::Delete {
            registry,
            name,
            version,
        } => {
            let coord = format!("{registry}/{name}@{version}");
            // Two facts, and the second is the one people are surprised by:
            // the bytes go, *and* the number is spent (RFC 0016 §4.4).
            confirm(
                "delete",
                std::slice::from_ref(&coord),
                Some(&format!(
                    "the artifact is dropped and the version number is spent permanently — \
                     {version} can never be published again"
                )),
            )?;
            client.delete_version(&registry, &name, &version).await?;
            done(&format!("Deleted {coord}"));
        }
        VersionCommand::Pin {
            registry,
            name,
            version,
        } => {
            client
                .set_retention_pin(&registry, &name, &version, true)
                .await?;
            done(&format!(
                "Pinned {registry}/{name}@{version} — retention will never reclaim it"
            ));
        }
        VersionCommand::Unpin {
            registry,
            name,
            version,
        } => {
            client
                .set_retention_pin(&registry, &name, &version, false)
                .await?;
            done(&format!(
                "Unpinned {registry}/{name}@{version} — the registry's retention policy applies \
                 to it again"
            ));
        }
    }
    Ok(())
}
