use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

use crate::api::client_setup::{
    self, check_server_scheme, Cached, ClientId, EnvShell, MavenMirror, Options, ShellRc, State,
    TargetId, TargetPlan,
};
use crate::api::ide::detect_ides;
use crate::api::registry::{host_of, RegistryInfo, RegistryTargets};
use crate::api::setup::{api_registry_type, scan_project_types};
use crate::api::BatleHubClient;

/// How long to wait for the registry list before falling back to placeholders.
/// `setup` is a "tell me what to paste" command; it must not sit on a server
/// that is down or unreachable from this network.
const REGISTRY_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `setup env` runs from a shell rc file, so every terminal waits on it: it
/// prints the `env.sh` on disk when it can, else gives the server this long
/// before falling back to that file anyway.
const ENV_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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

        /// As given to `apply`: with `none`, rc files are not checked
        #[arg(long, value_enum, default_value = "auto")]
        shell_rc: ShellRc,

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

        /// Ask the server even when the env.sh `apply` wrote can answer
        #[arg(long)]
        refresh: bool,
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
    //
    // Neither `--registry` nor a field named `registry`: both belong to the
    // global `-r/--registry` (and its `BATLEHUB_REGISTRY`), a bare registry
    // name. clap allows one long name per command, and derives the argument
    // id from the field name — a field called `registry` is filled with the
    // global value whatever its long name.
    #[arg(long, value_name = "TYPE=NAME")]
    pick: Vec<String>,

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
async fn fetch_registries(server: &str, timeout: std::time::Duration) -> Result<Vec<RegistryInfo>> {
    let client = BatleHubClient::for_trusted_document(server)?;
    match tokio::time::timeout(timeout, client.list_registries()).await {
        Ok(Ok(registries)) => Ok(registries),
        Ok(Err(e)) => Err(e.context(format!("could not list registries on {server}"))),
        Err(_) => anyhow::bail!("{server} did not answer within {}s", timeout.as_secs()),
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
    run_in(cmd, global_server, token, client_setup::Env::from_process).await
}

/// [`run`] on the machine `machine` describes — the process's, or a temp
/// directory's in tests.
async fn run_in(
    cmd: SetupCommand,
    global_server: Option<&str>,
    token: Option<&str>,
    // The flag is `probe_yarn`: false for `env`, which plans no yarn target,
    // and `remove`, which needs no version.
    machine: impl Fn(bool) -> Result<client_setup::Env>,
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
            let env = machine(true)?;
            if yes {
                client_setup::check_not_root_for_another(&env)?;
            }
            let plans = load_plan(
                global_server,
                &select,
                &server,
                maven_mirror_of,
                shell_rc,
                &env,
            )
            .await?;
            Ok(run_apply(&plans, &env, shell_rc, yes, json))
        }
        SetupCommand::Status {
            select,
            server,
            shell_rc,
            json,
        } => {
            let env = machine(true)?;
            let plans = load_plan(global_server, &select, &server, None, shell_rc, &env).await?;
            run_status(&plans, json)
        }
        SetupCommand::Remove { yes, select, json } => {
            // No yarn probe: remove takes out every block whatever yarn is.
            let env = machine(false)?;
            if yes {
                client_setup::check_not_root_for_another(&env)?;
            }
            run_remove(&env, &select, global_server, yes, json)
        }
        SetupCommand::Env {
            shell,
            select,
            server,
            refresh,
        } => {
            let env = machine(false)?;
            run_env(global_server, select, &server, shell, refresh, &env).await
        }
    }
}

/// `setup env`: the variables of the env-only clients `--only`/`--skip`
/// select. Every env-only client is planned (the selection filters
/// variables, not the file), and nothing is printed when `env.sh` is in
/// conflict or ambiguous — `eval` would override what `apply` refused to.
/// The `env.sh` `apply` wrote is printed as is when it is unedited, for this
/// server — or for whatever server when none is set, as in a shell that never
/// exported one — and not overridden (see [`client_setup::cached_env_for`]),
/// so a shell start costs no request; a registry renamed since is picked up
/// at the next `apply`, or with `--refresh` or `--pick`, which always ask.
/// A server that cannot answer — unreachable, slow, an error status, an
/// unreadable or empty list — falls back to that `env.sh` under the same
/// rules, with a note on stderr; an override prints nothing (exit 3), and a
/// mistake of the caller's (`--pick`) is an error.
async fn run_env(
    global_server: Option<&str>,
    select: Selection,
    args: &ServerArgs,
    shell: EnvShell,
    refresh: bool,
    env: &client_setup::Env,
) -> Result<i32> {
    let wanted: Vec<ClientId> = ClientId::ENV_ONLY
        .into_iter()
        .filter(|c| (select.only.is_empty() || select.only.contains(c)) && !select.skip.contains(c))
        .collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let selected = |vars: &[(String, String)]| -> Vec<(String, String)> {
        vars.iter()
            .filter(|(k, _)| client_setup::env_var_client(k).is_some_and(|c| wanted.contains(&c)))
            .cloned()
            .collect()
    };
    let refused = |why: &str| {
        eprintln!("note: {why}; nothing printed");
        Ok(3)
    };
    if !refresh && args.pick.is_empty() {
        match client_setup::cached_env_for(env, global_server) {
            Cached::Usable(vars) => {
                print!("{}", client_setup::render_env(&selected(&vars), shell));
                return Ok(0);
            }
            // With a server the plan decides: a stale value of this instance's
            // is not an override there.
            Cached::Overridden(why) if global_server.is_none() => return refused(&why),
            _ => {}
        }
    }
    let server = require_server(global_server)?;
    check_server_scheme(server, args.insecure_http)?;
    // An empty list is a server half-started (apply refuses it too), not an
    // instruction to unset every variable.
    let fetched = fetch_registries(server, ENV_FETCH_TIMEOUT)
        .await
        .and_then(|r| {
            if r.is_empty() {
                anyhow::bail!("{server} lists no registry")
            }
            Ok(r)
        });
    let registries = match fetched {
        Ok(r) => r,
        // The fallback keeps the rules of the cached path: this server's
        // URLs only, no override, and a `--pick` needs the list.
        Err(e) => match client_setup::cached_env_for(env, Some(server)) {
            Cached::Usable(vars) if args.pick.is_empty() => {
                eprintln!("note: {e:#}; printing the env.sh written last time");
                print!("{}", client_setup::render_env(&selected(&vars), shell));
                return Ok(0);
            }
            Cached::Overridden(why) => return refused(&format!("{e:#}; {why}")),
            _ => return Err(e),
        },
    };
    let all = Selection {
        only: ClientId::ENV_ONLY.to_vec(),
        skip: Vec::new(),
    };
    let opts = options(server, &all, args, None, ShellRc::None)?;
    let plans = client_setup::plan(&registries, &opts, env)?;
    let Some(p) = plans.iter().find(|p| p.target == TargetId::EnvSh) else {
        return Ok(0);
    };
    if matches!(p.state, State::Conflict | State::Ambiguous) {
        eprintln!(
            "note: {} is {}: {}; nothing printed",
            p.path.display(),
            p.state.as_str(),
            p.detail.as_deref().unwrap_or("")
        );
        return Ok(3);
    }
    print!("{}", client_setup::render_env(&selected(&p.env), shell));
    Ok(0)
}

fn require_server(global_server: Option<&str>) -> Result<&str> {
    global_server.ok_or_else(|| {
        anyhow::anyhow!(
            "no server: pass --server, set BATLEHUB_SERVER, or set the profile's server_url"
        )
    })
}

fn options<'a>(
    server: &'a str,
    select: &Selection,
    args: &ServerArgs,
    maven_mirror_of: Option<MavenMirror>,
    shell_rc: ShellRc,
) -> Result<Options<'a>> {
    Ok(Options {
        server,
        picks: client_setup::parse_picks(&args.pick)?,
        maven_mirror_of,
        shell_rc,
        only: select.only.clone(),
        skip: select.skip.clone(),
        allow_foreign_hosts: args.allow_foreign_hosts,
        insecure_http: args.insecure_http,
    })
}

/// The plan `apply` and `status` work from. An empty registry list is
/// refused: a server half-started, or with every registry closed to
/// anonymous reads, would otherwise have an unattended `apply --yes` take out
/// every block on the machine — that is `setup remove`'s job, asked for.
async fn load_plan(
    global_server: Option<&str>,
    select: &Selection,
    args: &ServerArgs,
    maven_mirror_of: Option<MavenMirror>,
    shell_rc: ShellRc,
    env: &client_setup::Env,
) -> Result<Vec<TargetPlan>> {
    let server = require_server(global_server)?;
    check_server_scheme(server, args.insecure_http)?;
    let opts = options(server, select, args, maven_mirror_of, shell_rc)?;
    let registries = fetch_registries(server, REGISTRY_FETCH_TIMEOUT).await?;
    if registries.is_empty() {
        anyhow::bail!(
            "{server} lists no registry this machine may read anonymously; nothing was planned \
             (`setup remove` takes every block out, if that is what you want)"
        );
    }
    client_setup::plan(&registries, &opts, env)
}

/// 3 when a client needs a human: a conflict to merge, a registry to pick
/// (ambiguous), a registry gone (orphaned, kept until `remove` or `--pick`),
/// or env-only variables no rc file will source.
fn needs_a_look(plans: &[TargetPlan], env: &client_setup::Env, shell_rc: ShellRc) -> bool {
    unwired(plans, env, shell_rc)
        || plans.iter().any(|p| {
            matches!(
                p.state,
                State::Conflict | State::Ambiguous | State::Orphaned
            )
        })
}

/// `env.sh` is written but no rc block or fish file sources it, and no
/// supported shell is known to add one to.
fn unwired(plans: &[TargetPlan], env: &client_setup::Env, shell_rc: ShellRc) -> bool {
    let exports = plans
        .iter()
        .any(|p| p.target == TargetId::EnvSh && p.next.is_some());
    let known_shell = matches!(env.shell.as_deref(), Some("bash" | "zsh" | "fish"));
    // An rc block or fish file already there keeps sourcing it, `$SHELL` or not.
    let sourced = plans
        .iter()
        .any(|p| matches!(p.target, TargetId::ShellRc | TargetId::Fish));
    exports && shell_rc == ShellRc::Auto && !known_shell && !sourced
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

/// What `apply` warns about: the clients it skipped, and an env file no rc
/// file will source.
fn apply_warnings(plans: &[TargetPlan], env: &client_setup::Env, shell_rc: ShellRc) -> Vec<String> {
    let mut warnings: Vec<String> = plans
        .iter()
        .filter_map(|p| {
            let detail = p.detail.as_deref().unwrap_or("");
            match p.state {
                State::Conflict => Some(format!(
                    "{}: conflict, skipped — {detail}",
                    clients_label(p)
                )),
                State::Ambiguous => Some(format!("{}: {detail}", clients_label(p))),
                // A registry gone, switched to another: said, not only in status.
                _ if p.detail.is_some() => Some(format!("{}: {detail}", clients_label(p))),
                _ => None,
            }
        })
        .collect();
    if unwired(plans, env, shell_rc) {
        warnings.push(format!(
            "login shell {} is not bash, zsh or fish: no rc file was changed; \
             add `eval \"$(batlehub-cli setup env)\"` to it and pass --shell-rc none",
            env.shell.as_deref().unwrap_or("unknown")
        ));
    }
    warnings
}

/// Write every file the plan changes. A file that cannot be written does not
/// stop the others: each one is reported, and the exit code is 1 when any
/// failed, else 3 when a client needs a look, else 0.
fn run_apply(
    plans: &[TargetPlan],
    env: &client_setup::Env,
    shell_rc: ShellRc,
    yes: bool,
    json: bool,
) -> i32 {
    let warnings = apply_warnings(plans, env, shell_rc);
    let mut errors: Vec<String> = Vec::new();
    let mut failed: Vec<serde_json::Value> = Vec::new();
    let mut written: Vec<&TargetPlan> = Vec::new();
    if yes {
        for p in plans.iter().filter(|p| p.writes()) {
            match client_setup::apply_one(p) {
                Ok(()) => written.push(p),
                Err(e) => {
                    failed.push(serde_json::json!({
                        "path": p.path.to_string_lossy(),
                        "error": format!("{e:#}"),
                    }));
                    errors.push(format!("{e:#}"));
                }
            }
        }
    }
    if json {
        // `targets` is the plan as computed, before writing; `written` lists
        // the files actually written or deleted.
        let out = serde_json::json!({
            "applied": yes,
            "targets": plans,
            "written": written
                .iter()
                .map(|p| p.path.to_string_lossy())
                .collect::<Vec<_>>(),
            "warnings": warnings,
            "errors": failed,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string())
        );
    } else {
        print_apply(plans, &written, &warnings, &errors, yes);
    }
    if !errors.is_empty() {
        1
    } else if needs_a_look(plans, env, shell_rc) {
        3
    } else {
        0
    }
}

/// The human output of `apply`: what was written, or the diff of what would
/// be, then the snippets to merge by hand, the warnings and the failures.
fn print_apply(
    plans: &[TargetPlan],
    written: &[&TargetPlan],
    warnings: &[String],
    errors: &[String],
    yes: bool,
) {
    if yes {
        for p in written {
            let verb = if p.next.is_some() { "wrote" } else { "deleted" };
            println!("{verb} {} ({})", p.path.display(), clients_label(p));
        }
    } else {
        for p in plans.iter().filter(|p| p.writes()) {
            let current = p.current.as_deref().unwrap_or("");
            print!(
                "{}",
                client_setup::diff(&p.path, current, p.next.as_deref())
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
    for w in warnings {
        eprintln!("warning: {w}");
    }
    for e in errors {
        eprintln!("error: {e}");
    }
    let pending = plans.iter().filter(|p| p.writes()).count();
    if pending == 0 {
        println!("Nothing to write.");
    } else if !yes {
        println!("{pending} file(s) would change; run with --yes to write them.");
    }
}

/// Take every managed part out. Like `apply`, a file that cannot be written
/// does not stop the others; one left alone (edited, broken markers) is a
/// warning, and either makes the exit code non-zero (1 for a failure, 3 for a
/// file left alone) so a hook notices.
fn run_remove(
    env: &client_setup::Env,
    select: &Selection,
    server: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let (removals, warnings) = client_setup::removals(env, &select.only, &select.skip, server);
    let mut errors: Vec<String> = Vec::new();
    let mut done: Vec<&client_setup::Removal> = Vec::new();
    if yes {
        for r in &removals {
            match client_setup::remove_one(r) {
                Ok(()) => done.push(r),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
    }
    let code = if !errors.is_empty() {
        1
    } else if !warnings.is_empty() {
        3
    } else {
        0
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "removed": yes,
                "files": removals,
                "warnings": warnings,
                "errors": errors,
            }))?
        );
        return Ok(code);
    }
    if yes {
        for r in &done {
            let verb = if r.deletes { "deleted" } else { "restored" };
            println!("{verb} {}", r.path.display());
        }
    } else {
        for r in &removals {
            print!(
                "{}",
                client_setup::diff(&r.path, &r.current, r.next.as_deref())
            );
        }
    }
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    for e in &errors {
        eprintln!("error: {e}");
    }
    if removals.is_empty() && warnings.is_empty() {
        if select.only.is_empty() && select.skip.is_empty() {
            println!("Nothing managed by batlehub setup on this machine.");
        } else {
            println!("Nothing managed by batlehub setup for the selected clients.");
        }
    } else if removals.is_empty() {
        println!("Nothing removed.");
    } else if !yes {
        println!(
            "{} file(s) would change; run with --yes to remove.",
            removals.len()
        );
    }
    Ok(code)
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

    // ── setup apply | status | remove | env (RFC 0038) ──

    fn machine(installed: &[ClientId]) -> (tempfile::TempDir, client_setup::Env) {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path().to_owned();
        let env = client_setup::Env {
            config_dir: home.join(".config"),
            cargo_home: home.join(".cargo"),
            goenv: Some(home.join(".config/go/env")),
            pip_dir: home.join(".config/pip"),
            shell: Some("bash".to_owned()),
            // `node` is always installed, as `Env::from_process` has it.
            installed: installed.iter().copied().chain([ClientId::Node]).collect(),
            home,
            ..client_setup::Env::default()
        };
        (dir, env)
    }

    fn info(name: &str, ty: &str) -> RegistryInfo {
        RegistryInfo {
            name: name.to_owned(),
            registry_type: ty.to_owned(),
            mode: "proxy".to_owned(),
            public_url: None,
            upstream: None,
            installable: None,
            upstreams: Vec::new(),
        }
    }

    fn plans_for(regs: &[RegistryInfo], env: &client_setup::Env) -> Vec<TargetPlan> {
        let opts = Options {
            server: "https://hub.example.com",
            picks: Default::default(),
            maven_mirror_of: None,
            shell_rc: ShellRc::Auto,
            only: vec![],
            skip: vec![],
            allow_foreign_hosts: false,
            insecure_http: false,
        };
        client_setup::plan(regs, &opts, env).unwrap()
    }

    #[test]
    fn apply_writes_then_status_settles_then_remove_restores() {
        let (dir, env) = machine(&[ClientId::Npm, ClientId::Rustup]);
        let regs = vec![info("n", "npm"), info("r", "rustup")];

        // A dry run reports and writes nothing.
        assert_eq!(
            run_apply(&plans_for(&regs, &env), &env, ShellRc::Auto, false, false),
            0
        );
        assert!(!dir.path().join(".npmrc").exists());
        assert_eq!(run_status(&plans_for(&regs, &env), false).unwrap(), 3);

        assert_eq!(
            run_apply(&plans_for(&regs, &env), &env, ShellRc::Auto, true, true),
            0
        );
        assert!(dir.path().join(".npmrc").exists());
        assert_eq!(run_status(&plans_for(&regs, &env), true).unwrap(), 0);
        assert_eq!(
            run_apply(&plans_for(&regs, &env), &env, ShellRc::Auto, true, false),
            0
        );

        let select = Selection {
            only: vec![],
            skip: vec![],
        };
        assert_eq!(run_remove(&env, &select, None, false, false).unwrap(), 0);
        assert!(dir.path().join(".npmrc").exists(), "a dry remove removed");
        assert_eq!(run_remove(&env, &select, None, true, false).unwrap(), 0);
        assert!(!dir.path().join(".npmrc").exists());
        assert_eq!(run_remove(&env, &select, None, true, true).unwrap(), 0);
    }

    #[test]
    fn a_conflict_is_exit_3_and_warned() {
        let (dir, env) = machine(&[ClientId::Npm]);
        std::fs::write(dir.path().join(".npmrc"), "registry=https://x/\n").unwrap();
        let plans = plans_for(&[info("n", "npm")], &env);
        assert_eq!(run_apply(&plans, &env, ShellRc::Auto, true, false), 3);
        let warnings = apply_warnings(&plans, &env, ShellRc::Auto);
        assert!(warnings[0].contains("conflict, skipped"), "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".npmrc")).unwrap(),
            "registry=https://x/\n"
        );
    }

    /// A registry gone with several left orphans the block: `apply` says
    /// so, keeps the file (fail closed) and exits 3, a pick is needed.
    #[test]
    fn an_orphan_that_needs_a_pick_is_exit_3_and_said() {
        let (dir, env) = machine(&[ClientId::Npm]);
        let opts = |picks: &[&str]| Options {
            server: "https://hub.example.com",
            picks: client_setup::parse_picks(
                &picks.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
            .unwrap(),
            maven_mirror_of: None,
            shell_rc: ShellRc::Auto,
            only: vec![],
            skip: vec![],
            allow_foreign_hosts: false,
            insecure_http: false,
        };
        let abc = vec![info("a", "npm"), info("b", "npm"), info("c", "npm")];
        let plans = client_setup::plan(&abc, &opts(&["npm=b"]), &env).unwrap();
        assert_eq!(run_apply(&plans, &env, ShellRc::Auto, true, true), 0);
        let ac = vec![info("a", "npm"), info("c", "npm")];
        let plans = client_setup::plan(&ac, &opts(&[]), &env).unwrap();
        assert!(apply_warnings(&plans, &env, ShellRc::Auto)
            .iter()
            .any(|w| w.contains("b is gone")));
        assert_eq!(run_apply(&plans, &env, ShellRc::Auto, true, true), 3);
        assert!(std::fs::read_to_string(dir.path().join(".npmrc"))
            .unwrap()
            .contains("/proxy/b/"));
    }

    #[test]
    fn a_non_root_run_is_never_refused() {
        let (_dir, env) = machine(&[]);
        client_setup::check_not_root_for_another(&env).unwrap();
    }

    #[test]
    fn an_unknown_shell_is_told_to_eval_setup_env() {
        let (_dir, mut env) = machine(&[ClientId::Rustup]);
        env.shell = Some("nu".to_owned());
        let plans = plans_for(&[info("r", "rustup")], &env);
        let warnings = apply_warnings(&plans, &env, ShellRc::Auto);
        assert_eq!(
            warnings,
            [
                "login shell nu is not bash, zsh or fish: no rc file was changed; \
              add `eval \"$(batlehub-cli setup env)\"` to it and pass --shell-rc none"
            ]
        );
        assert!(apply_warnings(&plans, &env, ShellRc::None).is_empty());
    }

    #[tokio::test]
    async fn the_verbs_refuse_without_a_usable_server() {
        let select = Selection {
            only: vec![],
            skip: vec![],
        };
        let args = ServerArgs {
            pick: vec![],
            insecure_http: false,
            allow_foreign_hosts: false,
        };
        let (_dir, env) = machine(&[]);
        let err = load_plan(None, &select, &args, None, ShellRc::Auto, &env)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no server"), "{err}");
        let err = load_plan(
            Some("http://hub.example.com"),
            &select,
            &args,
            None,
            ShellRc::Auto,
            &env,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("plain HTTP"), "{err}");
        assert!(
            fetch_registries("http://127.0.0.1:1", REGISTRY_FETCH_TIMEOUT)
                .await
                .is_err()
        );
    }

    /// The dispatch, against a stub server, on a machine of its own: the
    /// real `$HOME` and `$PATH` (a rustup mirror exported on a dev box, a
    /// broken dotfile) must not decide the result.
    #[tokio::test]
    async fn run_dispatches_every_verb() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/v1/registries")
            .with_header("content-type", "application/json")
            .with_body(r#"[{"name":"r","type":"rustup","mode":"proxy"}]"#)
            .expect_at_least(3)
            .create_async()
            .await;
        let url = server.url();
        let (dir, env) = machine(&[ClientId::Rustup, ClientId::Node]);
        let machine = |_| Ok(env.clone());
        let select = || Selection {
            only: vec![ClientId::Rustup],
            skip: vec![],
        };
        let args = || ServerArgs {
            pick: vec![],
            insecure_http: false,
            allow_foreign_hosts: false,
        };

        let apply = SetupCommand::Apply {
            yes: false,
            select: select(),
            server: args(),
            maven_mirror_of: None,
            shell_rc: ShellRc::Auto,
            json: true,
        };
        assert_eq!(run_in(apply, Some(&url), None, machine).await.unwrap(), 0);
        assert!(
            !dir.path().join(".config/batlehub/env.sh").exists(),
            "a dry run wrote"
        );
        let status = || SetupCommand::Status {
            select: select(),
            server: args(),
            shell_rc: ShellRc::Auto,
            json: true,
        };
        assert_eq!(
            run_in(status(), Some(&url), None, machine).await.unwrap(),
            3
        );
        let apply = SetupCommand::Apply {
            yes: true,
            select: select(),
            server: args(),
            maven_mirror_of: None,
            shell_rc: ShellRc::Auto,
            json: false,
        };
        assert_eq!(run_in(apply, Some(&url), None, machine).await.unwrap(), 0);
        assert_eq!(
            run_in(status(), Some(&url), None, machine).await.unwrap(),
            0
        );
        let env_cmd = SetupCommand::Env {
            shell: EnvShell::Fish,
            select: select(),
            server: args(),
            refresh: false,
        };
        assert_eq!(run_in(env_cmd, Some(&url), None, machine).await.unwrap(), 0);
        let remove = SetupCommand::Remove {
            yes: true,
            select: select(),
            json: true,
        };
        assert_eq!(run_in(remove, Some(&url), None, machine).await.unwrap(), 0);
        assert!(!dir.path().join(".config/batlehub/env.sh").exists());
    }

    /// `status --shell-rc none` settles a machine set up that way.
    #[tokio::test]
    async fn status_takes_the_shell_rc_apply_was_given() {
        let (_dir, env) = machine(&[ClientId::Rustup, ClientId::Node]);
        let regs = vec![info("r", "rustup")];
        let opts = |shell_rc| Options {
            server: "https://hub.example.com",
            picks: Default::default(),
            maven_mirror_of: None,
            shell_rc,
            only: vec![],
            skip: vec![],
            allow_foreign_hosts: false,
            insecure_http: false,
        };
        let plans = client_setup::plan(&regs, &opts(ShellRc::None), &env).unwrap();
        run_apply(&plans, &env, ShellRc::None, true, true);
        let none = client_setup::plan(&regs, &opts(ShellRc::None), &env).unwrap();
        assert_eq!(run_status(&none, true).unwrap(), 0);
        let auto = client_setup::plan(&regs, &opts(ShellRc::Auto), &env).unwrap();
        assert_eq!(
            run_status(&auto, true).unwrap(),
            3,
            "auto wants the rc block"
        );
    }

    /// The list is trusted as a whole: a redirect off to plain HTTP (or to
    /// another host) is refused, not followed.
    #[tokio::test]
    async fn the_registry_list_follows_no_downgrading_redirect() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/v1/registries")
            .with_status(302)
            .with_header("location", "http://127.0.0.1:9/api/v1/registries")
            .create_async()
            .await;
        let err = fetch_registries(&server.url(), REGISTRY_FETCH_TIMEOUT)
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("refusing a redirect"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn load_registries_falls_back_when_the_server_is_unreachable() {
        assert!(load_registries("http://127.0.0.1:1", None, false)
            .await
            .is_empty());
    }
}
