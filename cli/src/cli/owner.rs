use anyhow::Result;
use clap::Subcommand;
use comfy_table::Table;

use crate::api::{owner::AddOwnerRequest, BatleHubClient};
use crate::coordinate::Coordinate;
use crate::render::ctx;

/// `registry/name`, or a bare name in the default registry. Ownership is per
/// package, so a version is refused rather than silently dropped.
async fn package(client: &BatleHubClient, raw: &str) -> Result<(String, String)> {
    let c = Coordinate::resolve(client, raw, ctx().registry.as_deref()).await?;
    if c.version.is_some() {
        return Err(crate::exit::CliError::new(
            crate::exit::ExitCode::Usage,
            format!("owners are per package; write `{}/{}`", c.registry, c.name),
        )
        .into());
    }
    Ok((c.registry, c.name))
}

#[derive(Subcommand)]
pub enum OwnerCommand {
    /// The owners of a package: `registry/name`
    List { coordinate: String },
    /// Add an owner to a package
    Add {
        coordinate: String,
        /// Principal ID (user ID or group name)
        principal: String,
        /// Principal type: user or group
        #[arg(long, default_value = "user")]
        r#type: String,
        /// Ownership role: admin or maintainer
        #[arg(long, default_value = "maintainer")]
        role: String,
    },
    /// Remove an owner from a package
    Remove {
        coordinate: String,
        /// Principal ID (user ID or group name)
        principal: String,
        /// Principal type: user or group
        #[arg(long, default_value = "user")]
        r#type: String,
    },
}

pub async fn run(cmd: OwnerCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        OwnerCommand::List { coordinate } => {
            let (registry, name) = package(client, &coordinate).await?;
            let owners = client.list_owners(&registry, &name).await?;
            if json {
                crate::render::data(&owners)?;
            } else {
                let mut table = Table::new();
                table.set_header(["Type", "Principal", "Role", "Granted By"]);
                for o in &owners {
                    table.add_row([
                        &o.principal_type,
                        &o.principal_id,
                        &o.role,
                        o.granted_by.as_deref().unwrap_or("-"),
                    ]);
                }
                println!("{table}");
                println!("{} owner(s)", owners.len());
            }
        }
        OwnerCommand::Add {
            coordinate,
            principal,
            r#type,
            role,
        } => {
            let (registry, name) = package(client, &coordinate).await?;
            client
                .add_owner(
                    &registry,
                    &name,
                    AddOwnerRequest {
                        principal_type: r#type.clone(),
                        principal_id: principal.clone(),
                        role: role.clone(),
                        granted_by: None,
                    },
                )
                .await?;
            println!("Added {type} '{principal}' as {role} on {registry}/{name}");
        }
        OwnerCommand::Remove {
            coordinate,
            principal,
            r#type,
        } => {
            let (registry, name) = package(client, &coordinate).await?;
            client
                .remove_owner(&registry, &name, &r#type, &principal)
                .await?;
            println!("Removed {type} '{principal}' from {registry}/{name}");
        }
    }
    Ok(())
}
