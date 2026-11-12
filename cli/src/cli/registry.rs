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
    Show {
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

        /// Print the one-rule auto-wire block instead (RFC 0045): every host
        /// this instance mirrors goes through its mise-versions wire. Needs a
        /// mise-versions registry with auto_wire enabled.
        #[arg(long, requires = "mise")]
        auto_wire: bool,

        /// Include suggestions the server already has a registry for
        #[arg(long)]
        include_existing: bool,
    },
}

pub async fn run(cmd: RegistryCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        RegistryCommand::List => run_list(client, json).await,
        RegistryCommand::Show { name } => run_info(client, json, &name).await,
        RegistryCommand::Suggest {
            dir,
            depth,
            client_env,
            mise,
            mise_commented,
            mise_catch_all,
            auto_wire,
            include_existing,
        } => {
            if auto_wire {
                return run_auto_wire(client, json, mise_commented, mise_catch_all).await;
            }
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
        crate::render::data(&registries)?;
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
        crate::render::data(&reg)?;
        return Ok(());
    }
    let mut table = Table::new();
    table.add_row(["Name", &reg.name]);
    table.add_row(["Type", &reg.registry_type]);
    table.add_row(["Mode", &reg.mode]);
    println!("{table}");
    Ok(())
}

/// `GET /api/v1/mise/auto-wire`, as the CLI reads it.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct MiseAutoWire {
    pub pattern: String,
    pub replacement: String,
    pub hosts: Vec<String>,
    #[serde(default)]
    pub hidden_hosts: usize,
    pub air_gap_pattern: String,
    pub registry_floating: bool,
    #[serde(default)]
    pub overlay_tools: Vec<MiseOverlayTool>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct MiseOverlayTool {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

async fn run_auto_wire(
    client: &BatleHubClient,
    json: bool,
    commented: bool,
    catch_all: bool,
) -> Result<()> {
    use crate::exit::{ApiError, CliError, ExitCode};
    let aw: MiseAutoWire = match client.get("/api/v1/mise/auto-wire").await {
        Ok(aw) => aw,
        Err(e) => {
            let Some(api) = e.downcast_ref::<ApiError>() else {
                return Err(e);
            };
            if api.status == 404 && api.code.as_deref() == Some("no_wire") {
                return Err(CliError::new(
                    ExitCode::NotFound,
                    "this instance has no mise-versions registry with auto_wire enabled",
                )
                .hint("the classic block is still available: batlehub-cli registry suggest --mise")
                .into());
            }
            if api.status == 404 {
                return Err(CliError::new(
                    ExitCode::NotFound,
                    "this server does not offer auto-wire (GET /api/v1/mise/auto-wire answered 404)",
                )
                .hint("upgrade the server, or use the classic block: batlehub-cli registry suggest --mise")
                .into());
            }
            return Err(e);
        }
    };
    if aw.hosts.is_empty() && !catch_all {
        return Err(CliError::new(
            ExitCode::Refused,
            "no host is wired for your account on this instance",
        )
        .hint("ask an administrator for read access to the registries mise should use")
        .into());
    }
    if json {
        crate::render::data(&aw)?;
        return Ok(());
    }
    print!(
        "{}",
        render_auto_wire(&aw, &client.base_url, catch_all, commented)
    );
    Ok(())
}

/// The auto-wire block (§4.5): one header, `registry_floating` only with the
/// overlay on, one `url_replacements` entry.
pub fn render_auto_wire(
    aw: &MiseAutoWire,
    server: &str,
    catch_all: bool,
    commented: bool,
) -> String {
    let server = server.trim_end_matches('/');
    let mut out = String::new();
    if catch_all {
        out.push_str(
            "# Generated by `batlehub-cli registry suggest --mise --auto-wire --mise-catch-all`.\n",
        );
        out.push_str(
            "# Put it in ONE config file (see the connected header). Air gap: every host\n",
        );
        out.push_str(
            "# goes to this instance. Hosts it does not mirror answer 501 and are recorded\n",
        );
        out.push_str(&format!(
            "# for the next bundle. Troubleshooting: {server}/docs/use/mise#auto-wire\n"
        ));
    } else {
        out.push_str("# Generated by `batlehub-cli registry suggest --mise --auto-wire`.\n");
        if aw.hidden_hosts > 0 {
            out.push_str(&format!(
                "# Filtered for your account: {} wired host(s) are hidden from it. Do not commit\n\
                 # this block to a shared mise.toml: teammates would fetch those hosts directly.\n",
                aw.hidden_hosts
            ));
        }
        out.push_str(
            "# Put it in ONE config file: ~/.config/mise/config.toml for every project, or\n",
        );
        out.push_str(
            "# a project's mise.toml. mise does not merge url_replacements across files: a\n",
        );
        out.push_str(
            "# project file that still has the classic block replaces this one entirely.\n",
        );
        out.push_str("# If the file already has a [settings] table, merge these keys into it.\n");
        out.push_str(
            "# Hosts not listed are fetched directly. Regenerate when the instance mirrors\n",
        );
        out.push_str(&format!(
            "# a new host. Troubleshooting: {server}/docs/use/mise#auto-wire\n"
        ));
    }
    if aw.registry_floating {
        out.push_str("[settings]\n");
        if catch_all {
            // §15 Q3: the archive crosses the gap with the bundle.
            out.push_str(
                "# Overlay names resolve from the registry archive the last bundle carried\n",
            );
            out.push_str("# (`batlehub-cli mise export --include-mise-registry`).\n");
        }
        out.push_str(
            "# Also floats mise's aqua registry: aqua tools resolve against the current\n",
        );
        out.push_str("# aqua registry (served by this instance), not the copy baked into mise.\n");
        out.push_str("registry_floating = true\n");
    }
    let key = if catch_all {
        &aw.air_gap_pattern
    } else {
        &aw.pattern
    };
    out.push_str("[settings.url_replacements]\n");
    out.push_str(&format!(
        "{} = {}\n",
        toml_string(key),
        toml_string(&aw.replacement)
    ));
    if !catch_all && !aw.overlay_tools.is_empty() {
        let names: Vec<String> = aw
            .overlay_tools
            .iter()
            .map(|t| {
                if t.aliases.is_empty() {
                    t.name.clone()
                } else {
                    format!("{} (alias {})", t.name, t.aliases.join(", "))
                }
            })
            .collect();
        out.push_str(&format!(
            "\n# Overlay tools (mise ≥ 2026.8.0): {}\n",
            names.join(", ")
        ));
    }
    if commented {
        out = out
            .lines()
            .map(|l| {
                if l.is_empty() || l.starts_with('#') {
                    l.to_owned()
                } else {
                    format!("# {l}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
    }
    out
}

/// A TOML basic string: `\\` and `"` escaped.
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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
    crate::render::data(&out)?;
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
             *.csproj, *.tf, environment.yml, Chart.lock, Chart.yaml"
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

#[cfg(test)]
mod auto_wire_tests {
    use super::*;

    fn aw(hidden: usize, floating: bool) -> MiseAutoWire {
        MiseAutoWire {
            pattern: r"regex:^https?://(api\.github\.com|mise\.jdx\.dev)/(.*)".to_owned(),
            replacement: "https://hub.example.com/proxy/mv/mise-versions/_wire/$1/$2".to_owned(),
            hosts: vec!["api.github.com".to_owned(), "mise.jdx.dev".to_owned()],
            hidden_hosts: hidden,
            air_gap_pattern: "regex:^https?://([^/]+)/(.*)".to_owned(),
            registry_floating: floating,
            overlay_tools: vec![MiseOverlayTool {
                name: "acme-deploy".to_owned(),
                aliases: vec!["deploy".to_owned()],
            }],
        }
    }

    #[test]
    fn the_block_is_valid_toml_with_one_rule() {
        let out = render_auto_wire(&aw(0, true), "https://hub.example.com", false, false);
        let parsed: toml::Table = toml::from_str(&out).expect("the block is TOML");
        let settings = parsed["settings"].as_table().unwrap();
        assert_eq!(settings["registry_floating"].as_bool(), Some(true));
        let repl = settings["url_replacements"].as_table().unwrap();
        assert_eq!(repl.len(), 1);
        assert_eq!(
            repl[r"regex:^https?://(api\.github\.com|mise\.jdx\.dev)/(.*)"].as_str(),
            Some("https://hub.example.com/proxy/mv/mise-versions/_wire/$1/$2")
        );
        assert!(out.contains("acme-deploy (alias deploy)"));
    }

    #[test]
    fn overlay_off_prints_no_floating_and_a_filtered_block_says_so() {
        let out = render_auto_wire(&aw(2, false), "https://hub.example.com", false, false);
        assert!(!out.contains("registry_floating"));
        assert!(out.contains("2 wired host(s) are hidden"));
    }

    #[test]
    fn the_air_gap_block_has_its_own_header_and_the_catch_all() {
        let out = render_auto_wire(&aw(0, true), "https://hub.example.com", true, false);
        assert!(out.contains("Air gap: every host"));
        assert!(!out.contains("fetched directly"));
        // §15 Q3: the bundle carries the archive, so the overlay floats here too.
        assert!(out.contains("--include-mise-registry"));
        assert!(
            !render_auto_wire(&aw(0, false), "https://hub.example.com", true, false)
                .contains("registry_floating")
        );
        let parsed: toml::Table = toml::from_str(&out).expect("TOML");
        assert_eq!(
            parsed["settings"]["registry_floating"].as_bool(),
            Some(true)
        );
        assert!(parsed["settings"]["url_replacements"]
            .as_table()
            .unwrap()
            .contains_key("regex:^https?://([^/]+)/(.*)"));
    }

    #[test]
    fn a_commented_block_has_no_live_line() {
        let out = render_auto_wire(&aw(0, true), "https://hub.example.com", false, true);
        assert!(
            out.lines().all(|l| l.is_empty() || l.starts_with('#')),
            "{out}"
        );
    }
}
