use anyhow::Result;
use clap::Subcommand;
use comfy_table::Table;

use crate::api::{owner::AddOwnerRequest, BatleHubClient};

#[derive(Subcommand)]
pub enum OwnerCommand {
    /// List owners of a package
    List {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
    },
    /// Add an owner to a package
    Add {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
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
        /// Registry name
        registry: String,
        /// Package name
        name: String,
        /// Principal ID (user ID or group name)
        principal: String,
        /// Principal type: user or group
        #[arg(long, default_value = "user")]
        r#type: String,
    },
}

pub async fn run(cmd: OwnerCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        OwnerCommand::List { registry, name } => {
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
            registry,
            name,
            principal,
            r#type,
            role,
        } => {
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
            registry,
            name,
            principal,
            r#type,
        } => {
            client
                .remove_owner(&registry, &name, &r#type, &principal)
                .await?;
            println!("Removed {type} '{principal}' from {registry}/{name}");
        }
    }
    Ok(())
}
