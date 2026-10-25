use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

use crate::api::client_setup::{
    self, check_server_scheme, ClientId, EnvShell, MavenMirror, Options, ShellRc, State, TargetId,
    TargetPlan,
};
use crate::api::ide::detect_ides;
use crate::api::registry::{host_of, RegistryInfo, RegistryTargets};
use crate::api::setup::{api_registry_type, scan_project_types};
use crate::api::BatleHubClient;

/// How long to wait for the registry list before falling back to placeholders.
/// `setup` is a "tell me what to paste" command; it must not sit on a server
/// that is down or unreachable from this network.
const REGISTRY_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Subcommand)]
pub enum SetupCommand {
    /// Scan a directory for known project manifests and print setup instructions
    Detect {
        /// Directory to scan (defaults to the current working directory)
        #[arg(long, short = 'd')]
        dir: Option<PathBuf>,

        /// How many subdirectory levels to scan (0 = root only, 1 = immediate subdirs, …)
        #[arg(long, default_value = "0")]
        depth: usize,

        /// Server URL to embed in the generated config snippets
        #[arg(long, env = "BATLEHUB_SERVER", default_value = "http://localhost:8080")]
        server: String,

        /// Do not contact the server; print `<registry>` placeholders instead of
        /// the configured registry names and their host-routed URLs
        #[arg(long)]
        offline: bool,

        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Detect the current editor/IDE and print how to point its extension or
    /// plugin ecosystem at BatleHub (VS Code / VSCodium → OpenVSX or VS Code
    /// Marketplace; JetBrains → JetBrains Marketplace)
    Ide {
        /// Server URL to embed in the generated config snippets
        #[arg(long, env = "BATLEHUB_SERVER", default_value = "http://localhost:8080")]
        server: String,

        /// Do not contact the server; print `<…>` registry placeholders
        #[arg(long)]
        offline: bool,

        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Write every installed client's user configuration from the server's
    /// registry list (RFC 0038). Without --yes, print the diff and write nothing
    Apply {
        /// Write the files; without it, only the diff is printed
        #[arg(long)]
        yes: bool,

        #[command(flatten)]
        select: Selection,

        #[command(flatten)]
        server: ServerArgs,

        /// What Maven's mirror replaces (default `central`, which leaves the
        /// repositories a POM declares alone). Kept by later runs once given
        #[arg(long, value_enum)]
        maven_mirror_of: Option<MavenMirror>,

        /// `auto` adds a line sourcing the env file to ~/.bashrc or ~/.zshenv
        /// (a conf.d file for fish); `none` leaves rc files alone
        #[arg(long, value_enum, default_value = "auto")]
        shell_rc: ShellRc,

        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Report, per client, whether this machine is configured. Exit 0 when
    /// nothing is left to do, 3 otherwise
    Status {
        #[command(flatten)]
        select: Selection,

        #[command(flatten)]
        server: ServerArgs,

        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Take every block and file `setup apply` wrote back out. Needs no server
    Remove {
        /// Remove; without it, only the diff is printed
        #[arg(long)]
        yes: bool,

        #[command(flatten)]
        select: Selection,

        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Print the variables of the clients configured only through their
    /// environment, for `eval "$(batlehub-cli setup env)"`
    Env {
        /// Syntax of the output
        #[arg(long, value_enum, default_value = "sh")]
        shell: EnvShell,

        #[command(flatten)]
        select: Selection,

        #[command(flatten)]
        server: ServerArgs,
    },
}

/// `--only` / `--skip`, shared by the RFC 0038 verbs.
#[derive(clap::Args)]
pub struct Selection {
    /// Only these clients
    #[arg(long, value_enum, value_delimiter = ',')]
    only: Vec<ClientId>,

    /// Every client but these
    #[arg(long, value_enum, value_delimiter = ',')]
    skip: Vec<ClientId>,
}

/// How the RFC 0038 verbs reach and trust the server.
#[derive(clap::Args)]
pub struct ServerArgs {
    /// Pick a registry when the server has several of one type; kept by later
    /// runs once given (repeatable)
    #[arg(long = "registry", value_name = "TYPE=NAME")]
    registry: Vec<String>,

    /// Accept a plain-HTTP server URL outside loopback
    #[arg(long)]
    insecure_http: bool,

    /// Accept a registry `public_url` that is not on the server's host or a
    /// subdomain of it
    #[arg(long)]
    allow_foreign_hosts: bool,
}

/// The registry list, or an error — never placeholders.
///
/// Unlike [`load_registries`], an unreachable server is a failure: a
/// placeholder written into `~/.npmrc` breaks npm. The list is asked for
/// anonymously, whatever token the profile holds: `apply` configures anonymous
/// reads, and the list only names the registries the caller may read, so a
/// closed registry is never written into a client that sends no credential.
async fn fetch_registries(server: &str) -> Result<Vec<RegistryInfo>> {
    let client = BatleHubClient::new(server, None)?;
    match tokio::time::timeout(REGISTRY_FETCH_TIMEOUT, client.list_registries()).await {
        Ok(Ok(registries)) => Ok(registries),
        Ok(Err(e)) => Err(e.context(format!("could not list registries on {server}"))),
        Err(_) => anyhow::bail!("{server} did not answer within 5s"),
    }
}

/// Best-effort registry list.
///
/// The instructions are still useful without it — that is the shape this
/// command shipped with — so a server that is down, unauthenticated or simply
/// not configured yet degrades to `<registry>` placeholders rather than to an
/// error. Only a *reachable* server can tell us that a registry lives on its own
/// host, and that is the difference between a snippet that works and one that
/// 404s, so it is worth asking for.
async fn load_registries(server: &str, token: Option<&str>, offline: bool) -> Vec<RegistryInfo> {
    if offline {
        return Vec::new();
    }
    let Ok(client) = BatleHubClient::new(server, token) else {
        return Vec::new();
    };
    match tokio::time::timeout(REGISTRY_FETCH_TIMEOUT, client.list_registries()).await {
        Ok(Ok(registries)) => registries,
        // Both arms are the same fallback; kept apart so the reason can be
        // reported, since "no registries" and "could not ask" look identical in
        // the output otherwise.
        Ok(Err(e)) => {
            eprintln!("Note: could not list registries on {server} ({e}); using placeholders.");
            Vec::new()
        }
        Err(_) => {
            eprintln!("Note: {server} did not answer within 5s; using placeholders.");
            Vec::new()
        }
    }
}

/// The `~/.netrc` block for `hosts`, or nothing when there is no host to name.
///
/// One stanza per host: `.netrc` is matched by hostname, so a host-routed
/// registry needs its own entry — credentials for the main host are not sent to
/// `npm1.batlehub.example.com`, and the install would 401.
///
/// The token is a placeholder even when one is configured: this prints to a
/// terminal (and often into a paste), and the value the reader must fill in is
/// theirs to choose — `batlehub-cli auth token` prints it when they want it.
fn netrc_block(hosts: &[String]) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Credentials — ~/.netrc (one stanza per host; chmod 600 ~/.netrc):\n\
         \n",
    );
    for host in hosts {
        out.push_str(&format!(
            "machine {host}\nlogin <user>\npassword <token>\n\n"
        ));
    }
    out.push_str("Get a token with: batlehub-cli auth login\n");
    Some(out)
}

/// Run a `setup` verb; the value is the process exit code (RFC 0038 §4.1).
pub async fn run(
    cmd: SetupCommand,
    global_server: Option<&str>,
    token: Option<&str>,
) -> Result<i32> {
    match cmd {
        SetupCommand::Detect {
            dir,
            depth,
            server,
            offline,
            json,
        } => {
            let server = global_server.unwrap_or(&server);
            run_detect(dir, depth, server, token, offline, json).await?;
            Ok(0)
        }
        SetupCommand::Ide {
            server,
            offline,
            json,
        } => {
            let server = global_server.unwrap_or(&server);
            run_ide(server, token, offline, json).await?;
            Ok(0)
        }
        SetupCommand::Apply {
            yes,
            select,
            server,
            maven_mirror_of,
            shell_rc,
            json,
        } => {
            let env = client_setup::Env::from_process()?;
            let plans = load_plan(
                global_server,
                &select,
                &server,
                maven_mirror_of,
                shell_rc,
                &env,
            )
            .await?;
            run_apply(&plans, &env, shell_rc, yes, json)
        }
        SetupCommand::Status {
            select,
            server,
            json,
        } => {
            let env = client_setup::Env::from_process()?;
            let plans =
                load_plan(global_server, &select, &server, None, ShellRc::Auto, &env).await?;
            run_status(&plans, json)
        }
        SetupCommand::Remove { yes, select, json } => {
            let env = client_setup::Env::from_process()?;
            run_remove(&env, &select, yes, json)
        }
        SetupCommand::Env {
            shell,
            select,
            server,
        } => {
            let env = client_setup::Env::from_process()?;
            let plans =
                load_plan(global_server, &select, &server, None, ShellRc::None, &env).await?;
            if let Some(p) = plans.iter().find(|p| p.target == TargetId::EnvSh) {
                print!("{}", client_setup::render_env(&p.env, shell));
            }
            Ok(0)
        }
    }
}

async fn load_plan(
    global_server: Option<&str>,
    select: &Selection,
    args: &ServerArgs,
    maven_mirror_of: Option<MavenMirror>,
    shell_rc: ShellRc,
    env: &client_setup::Env,
) -> Result<Vec<TargetPlan>> {
    let server = global_server.ok_or_else(|| {
        anyhow::anyhow!(
            "no server: pass --server, set BATLEHUB_SERVER, or set the profile's server_url"
        )
    })?;
    check_server_scheme(server, args.insecure_http)?;
    let registries = fetch_registries(server).await?;
    let opts = Options {
        server,
        picks: client_setup::parse_picks(&args.registry)?,
        maven_mirror_of,
        shell_rc,
        only: select.only.clone(),
        skip: select.skip.clone(),
        allow_foreign_hosts: args.allow_foreign_hosts,
    };
    client_setup::plan(&registries, &opts, env)
}

/// 3 when a client needs a human: a conflict to merge or a registry to pick.
fn needs_a_look(plans: &[TargetPlan]) -> bool {
    plans
        .iter()
        .any(|p| matches!(p.state, State::Conflict | State::Ambiguous))
}

fn clients_label(p: &TargetPlan) -> String {
    p.clients
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn print_row(p: &TargetPlan) {
    println!(
        "{:<18} {:<14} {:<16} {}",
        clients_label(p),
        p.state.as_str(),
        p.registries.join(","),
        p.path.display()
    );
    if let Some(detail) = &p.detail {
        println!("{:<18} └ {detail}", "");
    }
}

fn run_status(plans: &[TargetPlan], json: bool) -> Result<i32> {
    if json {
        println!("{}", serde_json::to_string_pretty(plans)?);
    } else {
        for p in plans {
            print_row(p);
        }
    }
    Ok(if plans.iter().all(|p| p.state.settled()) {
        0
    } else {
        3
    })
}

fn run_apply(
    plans: &[TargetPlan],
    env: &client_setup::Env,
    shell_rc: ShellRc,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let mut warnings: Vec<String> = Vec::new();
    for p in plans {
        match p.state {
            State::Conflict => warnings.push(format!(
                "{}: conflict, skipped — {}",
                clients_label(p),
                p.detail.as_deref().unwrap_or("")
            )),
            State::Ambiguous => warnings.push(format!(
                "{}: {}",
                clients_label(p),
                p.detail.as_deref().unwrap_or("")
            )),
            _ => {}
        }
    }
    let exports = plans
        .iter()
        .any(|p| p.target == TargetId::EnvSh && p.next.is_some());
    let known_shell = matches!(env.shell.as_deref(), Some("bash" | "zsh" | "fish"));
    if exports && shell_rc == ShellRc::Auto && !known_shell {
        warnings.push(format!(
            "login shell {} is not bash, zsh or fish: no rc file was changed;              add `eval \"$(batlehub-cli setup env)\"` to it",
            env.shell.as_deref().unwrap_or("unknown")
        ));
    }

    if yes {
        for p in plans.iter().filter(|p| p.writes()) {
            client_setup::apply_one(p)?;
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "written": yes,
                "targets": plans,
                "warnings": warnings,
            }))?
        );
    } else {
        for p in plans.iter().filter(|p| p.writes()) {
            if yes {
                let verb = if p.next.is_some() { "wrote" } else { "deleted" };
                println!("{verb} {} ({})", p.path.display(), clients_label(p));
            } else {
                print!(
                    "{}",
                    client_setup::diff(
                        &p.path,
                        p.current.as_deref().unwrap_or(""),
                        p.next.as_deref()
                    )
                );
            }
        }
        for p in plans.iter().filter(|p| p.state == State::Conflict) {
            if let Some(snippet) = &p.snippet {
                println!(
                    "\n{} — merge by hand into {}:\n{snippet}",
                    clients_label(p),
                    p.path.display()
                );
            }
        }
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let pending = plans.iter().filter(|p| p.writes()).count();
        if pending == 0 {
            println!("Nothing to write.");
        } else if !yes {
            println!("{pending} file(s) would change; run with --yes to write them.");
        }
    }
    Ok(if needs_a_look(plans) { 3 } else { 0 })
}

fn run_remove(env: &client_setup::Env, select: &Selection, yes: bool, json: bool) -> Result<i32> {
    let removals = client_setup::removals(env, &select.only, &select.skip)?;
    if yes {
        for r in &removals {
            client_setup::remove_one(r)?;
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "removed": yes, "files": removals })
            )?
        );
        return Ok(0);
    }
    for r in &removals {
        if yes {
            let verb = if r.deletes { "deleted" } else { "restored" };
            println!("{verb} {}", r.path.display());
        } else {
            print!(
                "{}",
                client_setup::diff(&r.path, &r.current, r.next.as_deref())
            );
        }
    }
    if removals.is_empty() {
        println!("Nothing managed by batlehub setup on this machine.");
    } else if !yes {
        println!(
            "{} file(s) would change; run with --yes to remove.",
            removals.len()
        );
    }
    Ok(0)
}

async fn run_detect(
    dir: Option<PathBuf>,
    depth: usize,
    server: &str,
    token: Option<&str>,
    offline: bool,
    json: bool,
) -> Result<()> {
    let dir = match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };

    let registries = load_registries(server, token, offline).await;
    let targets = RegistryTargets::new(server, &registries);
    let detections = scan_project_types(&dir, &targets, depth);

    if json {
        let out: Vec<serde_json::Value> = detections
            .iter()
            .map(|d| {
                serde_json::json!({
                    "registry_type": d.registry_type,
                    "package_name": d.package_name,
                    "relative_path": d.relative_path,
                    "registry_name": d.registry_name,
                    "base_url": d.base_url,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if detections.is_empty() {
        println!("No known project manifests found in: {}", dir.display());
        println!(
            "Supported: Cargo.toml, go.mod, package.json, pyproject.toml, \
             pom.xml, composer.json, *.gemspec, *.nuspec, *.csproj, *.tf, environment.yml"
        );
        return Ok(());
    }

    for det in &detections {
        let name = det.package_name.as_deref().unwrap_or("<unknown>");
        if det.relative_path.is_empty() {
            println!("Detected: {} ({})", det.registry_type, name);
        } else {
            println!(
                "Detected: {} ({}) [{}]",
                det.registry_type, name, det.relative_path
            );
        }
        print_instructions(&det.instructions);
    }

    // Only the types actually detected: a `.netrc` that names hosts this
    // project never talks to is noise the reader has to audit.
    let types: Vec<&str> = detections
        .iter()
        .map(|d| api_registry_type(d.registry_type))
        .collect();
    if let Some(block) = netrc_block(&targets.netrc_hosts(&types)) {
        println!("{block}");
    }
    Ok(())
}

async fn run_ide(server: &str, token: Option<&str>, offline: bool, json: bool) -> Result<()> {
    let registries = load_registries(server, token, offline).await;
    let setups = detect_ides(server, &registries);

    if json {
        let out: Vec<serde_json::Value> = setups
            .iter()
            .map(|s| {
                serde_json::json!({
                    "ide": s.kind.label(),
                    "registry_type": s.registry_type,
                    "registry_name": s.registry_name,
                    "registry_configured": s.registry_configured,
                    "detected_via": s.detected_via,
                    "base_url": s.base_url,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if setups.is_empty() {
        println!("No IDE detected in this environment.");
        println!(
            "Detection uses $TERM_PROGRAM / VSCODE_* / JetBrains terminal variables, \
             the ~/.config/{{Code,VSCodium,JetBrains}} directories, and a ./.idea folder. \
             Run this from your editor's integrated terminal."
        );
        return Ok(());
    }

    for s in &setups {
        println!("Detected: {} (via {})", s.kind.label(), s.detected_via);
        print_instructions(&s.instructions);
    }

    let mut hosts: Vec<String> = Vec::new();
    for setup in &setups {
        let host = host_of(&setup.base_url);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    if let Some(block) = netrc_block(&hosts) {
        println!("{block}");
    }
    Ok(())
}

/// One detection's snippet, with the rule that separates it from the next.
fn print_instructions(instructions: &str) {
    println!();
    println!("{instructions}");
    println!();
    println!("{}", "─".repeat(60));
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netrc_block_is_empty_without_hosts() {
        assert!(netrc_block(&[]).is_none());
    }

    #[test]
    fn netrc_block_writes_one_stanza_per_host() {
        let block = netrc_block(&[
            "npm1.batlehub.example.com".to_owned(),
            "batlehub.example.com".to_owned(),
        ])
        .unwrap();
        assert_eq!(block.matches("machine ").count(), 2);
        assert!(block.contains("machine npm1.batlehub.example.com"));
        assert!(block.contains("machine batlehub.example.com"));
        // Never a real credential: this lands in terminal scrollback.
        assert!(block.contains("password <token>"));
    }

    /// `--offline` must not build a client at all — the fallback is the whole
    /// point of the flag, and it has to hold for an unroutable server.
    #[tokio::test]
    async fn load_registries_offline_skips_the_server() {
        assert!(load_registries("http://127.0.0.1:1", None, true)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn load_registries_falls_back_when_the_server_is_unreachable() {
        assert!(load_registries("http://127.0.0.1:1", None, false)
            .await
            .is_empty());
    }
}
