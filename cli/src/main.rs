mod api;
mod cli;
mod config;
mod confirm;
mod contract;
mod coordinate;
mod coverage;
mod exit;
mod gallery_proxy;
mod render;
mod tui;

use anyhow::Result;
use clap::Parser;

use cli::groups::{self, Group};
use cli::{
    admin, auth, authz, completion, config_cmd, download, java, mise, owner, package, proxy,
    publish, registry, security, setup, version, vsx, Cli, Command,
};
use config::ConfigFile;
use exit::{CliError, ExitCode};
use render::Format;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let format = if cli.json {
        Format::Json
    } else {
        cli.format.unwrap_or_default()
    };
    match run(cli, format).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let report = exit::Report::from_error(&e);
            // A script parses a failure the way it parses a success.
            if format == Format::Json && !report.reported {
                println!("{}", report.to_json());
            }
            let text = report.to_stderr();
            if !text.is_empty() {
                eprintln!("{text}");
            }
            std::process::ExitCode::from(report.exit)
        }
    }
}

async fn run(cli: Cli, format: Format) -> Result<()> {
    // `raw` has no record separator: only a command that prints one value
    // takes it (§4.11).
    let prints_one_value = matches!(
        cli.command,
        Command::Auth {
            cmd: cli::auth::AuthCommand::Token { cmd: None, .. }
        } | Command::Profile {
            cmd: cli::config_cmd::ProfileCommand::Show { key: Some(_), .. }
        }
    );
    if format == Format::Raw && !prints_one_value {
        return Err(CliError::new(
            ExitCode::Usage,
            "--format raw is only for a command that prints one value (`auth token`)",
        )
        .into());
    }
    let context = |registry: Option<String>| render::Context {
        format,
        registry,
        yes: cli.yes,
    };

    // Commands that need neither config nor a server connection
    if let Command::Profile { cmd } = cli.command {
        render::init(context(cli.registry.clone()));
        return config_cmd::run_profile(cmd, cli.profile.as_deref());
    }
    if let Command::Config { cmd } = cli.command {
        render::init(context(cli.registry.clone()));
        return config_cmd::run(cmd, cli.profile.as_deref());
    }
    // The Java engine runs locally: no server, no token, no config.
    if let Command::Java(args) = cli.command {
        return java::run(args);
    }
    if let Command::Completion { shell } = cli.command {
        print!("{}", completion::script(shell));
        return Ok(());
    }
    let mut cfg = ConfigFile::load()?;
    // `-P`, else the file's `current` (`profile use`), else `default`.
    let profile_name: Option<String> = cfg.selected(cli.profile.as_deref()).map(str::to_owned);
    {
        let profile = match profile_name.as_deref() {
            Some(n) => cfg.profiles.get(n),
            None => Some(&cfg.default),
        };
        render::init(context(
            cli.registry
                .clone()
                .or_else(|| profile.and_then(|p| p.registry.clone())),
        ));
    }

    if let Command::Setup { cmd } = cli.command {
        let profile = match profile_name.as_deref() {
            Some(n) => cfg.profiles.get(n),
            None => Some(&cfg.default),
        };
        let resolved_server = cli
            .server
            .clone()
            .or_else(|| profile.and_then(|p| p.server_url.clone()));
        // The stored token as-is, with no OIDC refresh: `setup` only reads the
        // registry list, and must keep working with no server to refresh
        // against. An expired token just means fewer registries are listed.
        let resolved_token = cli
            .token
            .clone()
            .or_else(|| profile.and_then(|p| p.token.clone()));
        let code =
            match setup::run(cmd, resolved_server.as_deref(), resolved_token.as_deref()).await {
                Ok(code) => code,
                Err(e) => {
                    let report = exit::Report::from_error(&e);
                    eprintln!("{}", report.to_stderr());
                    i32::from(report.exit)
                }
            };
        // Exit here rather than return: dropping the runtime waits for its
        // blocking threads, and a `getaddrinfo` stuck on a dead resolver keeps
        // `eval "$(batlehub-cli setup env)"` — every new terminal — waiting
        // long after the timeout gave up on it.
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::process::exit(code);
    }

    // Determine base URL for potential OIDC auto-refresh (before building the client)
    let base_url_for_refresh = cli
        .server
        .clone()
        .or_else(|| {
            profile_name
                .as_deref()
                .and_then(|n| cfg.profiles.get(n))
                .or(Some(&cfg.default))
                .and_then(|p| p.server_url.clone())
        })
        .unwrap_or_else(|| "http://localhost:8080".to_string());

    // If the user supplied --token, use it directly; otherwise auto-resolve
    // (reads K8s token file or refreshes expiring OIDC token).
    //
    // `logout` is exempt: `resolve_token` can perform a network refresh, so a
    // logout against a server that is down would fail before clearing anything
    // — and refreshing a credential that is about to be discarded is work for
    // nothing even when the server answers.
    let logging_out = matches!(
        cli.command,
        Command::Auth {
            cmd: cli::auth::AuthCommand::Logout { .. }
        }
    );
    let effective_token = if let Some(ref t) = cli.token {
        Some(t.clone())
    } else if logging_out {
        None
    } else {
        api::auth::resolve_token(&base_url_for_refresh, profile_name.as_deref(), &mut cfg).await?
    };

    let resolved = cfg.resolve(
        profile_name.as_deref(),
        cli.server.clone(),
        effective_token,
        cli.registry.clone(),
    );

    let client = api::BatleHubClient::new(&resolved.server_url, resolved.token.as_deref())?;

    let json = render::ctx().structured();
    match cli.command {
        Command::Registry { cmd } => registry::run(cmd, &client, json).await?,
        Command::Package { cmd } => {
            package::run(cmd, &client, resolved.registry.as_deref(), json).await?
        }
        Command::Owner { cmd } => owner::run(cmd, &client, json).await?,
        Command::Visibility { cmd } => groups::run(Group::Visibility(cmd), &client, json).await?,
        Command::Grant { cmd } => groups::run(Group::Grant(cmd), &client, json).await?,
        Command::Namespace { cmd } => groups::run(Group::Namespace(cmd), &client, json).await?,
        Command::User { cmd } => groups::run(Group::User(cmd), &client, json).await?,
        Command::Quota { cmd } => groups::run(Group::Quota(cmd), &client, json).await?,
        Command::IpBlock { cmd } => groups::run(Group::IpBlock(cmd), &client, json).await?,
        Command::Server { cmd } => groups::run(Group::Server(cmd), &client, json).await?,
        Command::Cache { cmd } => groups::run(Group::Cache(cmd), &client, json).await?,
        Command::Import {
            registry,
            tag,
            repo,
        } => {
            groups::run(
                Group::Import {
                    registry,
                    tag,
                    repo,
                },
                &client,
                json,
            )
            .await?
        }
        Command::Retention { cmd } => groups::run(Group::Retention(cmd), &client, json).await?,
        Command::Banner { cmd } => groups::run(Group::Banner(cmd), &client, json).await?,
        Command::Exposure { cmd } => groups::run(Group::Exposure(cmd), &client, json).await?,
        Command::Bundle { cmd } => groups::run(Group::Bundle(cmd), &client, json).await?,
        Command::AirGap { cmd } => groups::run(Group::AirGap(cmd), &client, json).await?,
        Command::Flag { cmd } => groups::run(Group::Flag(cmd), &client, json).await?,
        Command::Gdpr { cmd } => groups::run(Group::Gdpr(cmd), &client, json).await?,
        Command::Sbom { cmd } => groups::run(Group::Sbom(cmd), &client, json).await?,
        Command::Notification { cmd } => {
            groups::run(Group::Notification(cmd), &client, json).await?
        }
        Command::Audit { cmd } => groups::run(Group::Audit(cmd), &client, json).await?,
        Command::Verdict { cmd } => security::run_verdicts(cmd, &client, json).await?,
        Command::Me { cmd } => cli::journeys::run_me(cmd, &client).await?,
        Command::Policy { cmd } => cli::journeys::run_policy(cmd, &client).await?,
        Command::Exemption { cmd } => cli::journeys::run_exemption(cmd, &client).await?,
        Command::Upstream { cmd } => cli::journeys::run_upstream(cmd, &client).await?,
        // ── deprecated spellings ──
        Command::Version { cmd } => version::run(cmd, &client).await?,
        Command::Owners { cmd } => owner::run_legacy(cmd, &client, json).await?,
        Command::Verdicts { cmd } => {
            render::deprecated("verdicts", "verdict");
            security::run_verdicts(cmd, &client, json).await?
        }
        Command::Admin { cmd } => {
            let (old, new) = groups::admin_replacement(&cmd);
            render::deprecated(old, new);
            admin::run(cmd, &client, json).await?
        }
        Command::Authz { cmd } => authz::run(cmd, &client, json).await?,
        Command::Mise { cmd } => mise::run(cmd, &client, json).await?,
        Command::Proxy { cmd } => proxy::run(cmd).await?,
        Command::Vsx { cmd } => vsx::run(cmd, cli.token.as_deref()).await?,
        Command::Why(args) => cli::why::run(args, &client).await?,
        Command::Wait(args) => security::run_wait(args, &client, json).await?,
        Command::Publish(args) => publish::run(args, &client, resolved.registry.as_deref()).await?,
        Command::Download(args) => {
            download::run(args, &client, resolved.registry.as_deref()).await?
        }
        Command::Auth { cmd } => {
            auth::run(
                cmd,
                &client,
                json,
                profile_name.as_deref(),
                cli.token.as_deref(),
            )
            .await?
        }
        Command::Tui => tui::run(client).await?,
        Command::Config { .. }
        | Command::Profile { .. }
        | Command::Completion { .. }
        | Command::Setup { .. }
        | Command::Java(_) => {
            unreachable!()
        }
    }

    Ok(())
}
