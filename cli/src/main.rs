mod api;
mod cli;
mod config;
mod contract;
mod gallery_proxy;
mod tui;

use anyhow::Result;
use clap::Parser;

use cli::{
    admin, auth, authz, config_cmd, download, mise, owner, package, proxy, publish, registry,
    security, setup, version, vsx, Cli, Command,
};
use config::ConfigFile;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Commands that need neither config nor a server connection
    if let Command::Config { cmd } = cli.command {
        return config_cmd::run(cmd);
    }
    if let Command::Completion { shell } = cli.command {
        use clap::CommandFactory;
        use clap_complete::generate;
        let mut cmd = Cli::command();
        generate(shell, &mut cmd, "batlehub-cli", &mut std::io::stdout());
        return Ok(());
    }
    let mut cfg = ConfigFile::load()?;

    if let Command::Setup { cmd } = cli.command {
        let profile = match cli.profile.as_deref() {
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
        return setup::run(cmd, resolved_server.as_deref(), resolved_token.as_deref()).await;
    }

    // Determine base URL for potential OIDC auto-refresh (before building the client)
    let base_url_for_refresh = cli
        .server
        .clone()
        .or_else(|| {
            cli.profile
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
        api::auth::resolve_token(&base_url_for_refresh, cli.profile.as_deref(), &mut cfg).await?
    };

    let resolved = cfg.resolve(
        cli.profile.as_deref(),
        cli.server.clone(),
        effective_token,
        cli.registry.clone(),
    );

    let client = api::BatleHubClient::new(&resolved.server_url, resolved.token.as_deref())?;

    match cli.command {
        Command::Registry { cmd } => registry::run(cmd, &client, cli.json).await?,
        Command::Package { cmd } => {
            package::run(cmd, &client, resolved.registry.as_deref(), cli.json).await?
        }
        Command::Version { cmd } => version::run(cmd, &client).await?,
        Command::Owners { cmd } => owner::run(cmd, &client, cli.json).await?,
        Command::Authz { cmd } => authz::run(cmd, &client, cli.json).await?,
        Command::Mise { cmd } => mise::run(cmd, &client, cli.json).await?,
        Command::Proxy { cmd } => proxy::run(cmd).await?,
        Command::Vsx { cmd } => vsx::run(cmd, cli.token.as_deref()).await?,
        Command::Why(args) => security::run_why(args, &client, cli.json).await?,
        Command::Audit { cmd } => security::run_audit(cmd, &client, cli.json).await?,
        Command::Verdicts { cmd } => security::run_verdicts(cmd, &client, cli.json).await?,
        Command::Wait(args) => {
            let code = security::run_wait(args, &client, cli.json).await?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Publish(args) => publish::run(args, &client, resolved.registry.as_deref()).await?,
        Command::Download(args) => {
            download::run(args, &client, resolved.registry.as_deref()).await?
        }
        Command::Auth { cmd } => {
            auth::run(
                cmd,
                &client,
                cli.json,
                cli.profile.as_deref(),
                cli.token.as_deref(),
            )
            .await?
        }
        Command::Admin { cmd } => admin::run(cmd, &client, cli.json).await?,
        Command::Tui => tui::run(client).await?,
        Command::Config { .. } | Command::Completion { .. } | Command::Setup { .. } => {
            unreachable!()
        }
    }

    Ok(())
}
