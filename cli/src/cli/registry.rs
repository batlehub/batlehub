use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use comfy_table::Table;

use crate::api::suggest::{render_client_env, render_toml, suggest_registries, SuggestedRegistry};
use crate::api::BatleHubClient;

#[derive(Subcommand)]
pub enum RegistryCommand {
    /// List all accessible registries
    List,
    /// Show details for a single registry
    Info {
        /// Registry name
        name: String,
    },
    /// Suggest the registries a project needs, from its mise.lock and manifests
    Suggest {
        /// Directory to scan (defaults to the current working directory)
        #[arg(long, short = 'd')]
        dir: Option<PathBuf>,

        /// How many subdirectory levels to scan for manifests (0 = root only)
        #[arg(long, default_value = "0")]
        depth: usize,

        /// Also print the client-side environment variables for each toolchain
        #[arg(long)]
        client_env: bool,

        /// Also print a mise [settings.url_replacements] block routing mise
        /// through the proxy
        #[arg(long)]
        mise: bool,

        /// Comment out every line of the --mise block, for committing into a
        /// shared mise.toml
        #[arg(long, requires = "mise")]
        mise_commented: bool,

        /// Append RFC 0008's catch-all rule: anything no other rule matched
        /// is sent to the proxy's sink, which fetches nothing, answers 501
        /// and records the host. Turns an unmirrored host from a connect
        /// timeout into a line in the console.
        #[arg(long, requires = "mise")]
        mise_catch_all: bool,

        /// Include suggestions the server already has a registry for
        #[arg(long)]
        include_existing: bool,
    },
}

pub async fn run(cmd: RegistryCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        RegistryCommand::List => run_list(client, json).await,
        RegistryCommand::Info { name } => run_info(client, json, &name).await,
        RegistryCommand::Suggest {
            dir,
            depth,
            client_env,
            mise,
            mise_commented,
            mise_catch_all,
            include_existing,
        } => {
            run_suggest(
                client,
                json,
                SuggestOptions {
                    dir,
                    depth,
                    client_env,
                    mise,
                    mise_commented,
                    mise_catch_all,
                    include_existing,
                },
            )
            .await
        }
    }
}

async fn run_list(client: &BatleHubClient, json: bool) -> Result<()> {
    let registries = client.list_registries().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&registries)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header(["Name", "Type", "Mode"]);
    for r in &registries {
        table.add_row([&r.name, &r.registry_type, &r.mode]);
    }
    println!("{table}");
    println!("{} registry/registries", registries.len());
    Ok(())
}

async fn run_info(client: &BatleHubClient, json: bool, name: &str) -> Result<()> {
    let registries = client.list_registries().await?;
    let reg = registries
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow::anyhow!("registry '{name}' not found"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reg)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.add_row(["Name", &reg.name]);
    table.add_row(["Type", &reg.registry_type]);
    table.add_row(["Mode", &reg.mode]);
    println!("{table}");
    Ok(())
}

/// `registry suggest`'s flags, kept together so the handler takes one argument
/// rather than six positional ones that are easy to transpose.
struct SuggestOptions {
    dir: Option<std::path::PathBuf>,
    depth: usize,
    client_env: bool,
    mise: bool,
    mise_commented: bool,
    mise_catch_all: bool,
    include_existing: bool,
}

async fn run_suggest(client: &BatleHubClient, json: bool, opts: SuggestOptions) -> Result<()> {
    let dir = match opts.dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let suggestions = suggest_registries(&dir, opts.depth);

    // Scanning is purely local, so an unreachable server must not fail
    // the command — it only costs the "already configured" annotation.
    let existing: Vec<String> = if opts.include_existing {
        Vec::new()
    } else {
        client
            .list_registries()
            .await
            .map(|regs| regs.into_iter().map(|r| r.registry_type).collect())
            .unwrap_or_default()
    };

    let (wanted, already): (Vec<_>, Vec<_>) = suggestions
        .into_iter()
        .partition(|s| opts.include_existing || !existing.contains(&s.registry_type));

    if json {
        print_json(
            &wanted,
            &already,
            &client.base_url,
            opts.mise_commented,
            opts.mise_catch_all,
        )?;
    } else {
        print_human(
            &wanted,
            &already,
            &dir,
            &client.base_url,
            opts.client_env,
            opts.mise.then_some(opts.mise_commented),
            opts.mise_catch_all,
        );
    }
    Ok(())
}

fn print_json(
    wanted: &[SuggestedRegistry],
    already: &[SuggestedRegistry],
    server_url: &str,
    mise_commented: bool,
    mise_catch_all: bool,
) -> Result<()> {
    let to_json = |s: &SuggestedRegistry| {
        serde_json::json!({
            "name": s.name,
            "type": s.registry_type,
            "upstreams": s.upstreams,
            "path_allow": s.path_allow,
            "sources": s.sources,
            "note": s.note,
            "proxy_url": s.proxy_url(server_url),
            "client_env": s.resolved_env(server_url)
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
            "mise_url_replacements": s.mise_url_replacements(server_url)
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        })
    };
    let out = serde_json::json!({
        "suggested": wanted.iter().map(to_json).collect::<Vec<_>>(),
        "already_configured": already.iter().map(to_json).collect::<Vec<_>>(),
        "toml": render_toml(wanted),
        "mise_toml": crate::api::suggest::render_mise_toml(
            wanted,
            server_url,
            mise_commented,
            mise_catch_all,
        ),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn print_human(
    wanted: &[SuggestedRegistry],
    already: &[SuggestedRegistry],
    dir: &std::path::Path,
    server_url: &str,
    client_env: bool,
    // `Some(commented)` when `--mise` was passed.
    mise: Option<bool>,
    // RFC 0008 §4.4's catch-all, appended last.
    mise_catch_all: bool,
) {
    if wanted.is_empty() && already.is_empty() {
        println!("No package sources found in: {}", dir.display());
        println!(
            "Looked at: mise.lock, mise.toml, .nvmrc, .sdkmanrc, Cargo.toml, go.mod, \
             package.json, pyproject.toml, pom.xml, composer.json, *.gemspec, *.nuspec, \
             *.csproj, *.tf, environment.yml"
        );
        return;
    }

    if !already.is_empty() {
        println!(
            "Already covered by an existing registry of the same type: {}",
            already
                .iter()
                .map(|s| s.registry_type.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("(pass --include-existing to emit these too)");
        println!();
    }

    if wanted.is_empty() {
        println!("Every detected source is already covered by a configured registry.");
        return;
    }

    print_suggestion_table(wanted);
    println!();
    println!("Add to config.toml:");
    println!();
    println!("{}", render_toml(wanted));

    if let Some(commented) = mise {
        println!();
        println!("Route mise through the proxy:");
        println!();
        println!(
            "{}",
            crate::api::suggest::render_mise_toml(wanted, server_url, commented, mise_catch_all)
        );
    }

    if client_env {
        println!();
        println!("Point clients at the proxy:");
        println!();
        println!("{}", render_client_env(wanted, server_url));
    }

    print_flag_hints(client_env, mise);
}

fn print_suggestion_table(wanted: &[SuggestedRegistry]) {
    let mut table = Table::new();
    table.set_header(["Name", "Type", "Upstream", "Detected from"]);
    for s in wanted {
        let upstream = if s.upstreams.is_empty() {
            "(adapter default)".to_owned()
        } else {
            s.upstreams.join(", ")
        };
        table.add_row([
            s.name.clone(),
            s.registry_type.clone(),
            upstream,
            s.sources.join(", "),
        ]);
    }
    println!("{table}");
}

/// Name the flags that would have printed more, and only those.
fn print_flag_hints(client_env: bool, mise: Option<bool>) {
    let mut hints = Vec::new();
    if !client_env {
        hints.push("--client-env for the matching client environment variables");
    }
    if mise.is_none() {
        hints.push("--mise for a mise [settings.url_replacements] block");
    }
    if !hints.is_empty() {
        println!("Re-run with {}.", hints.join(", "));
    }
}
