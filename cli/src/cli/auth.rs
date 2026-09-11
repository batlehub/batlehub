use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use comfy_table::Table;

use crate::api::{
    auth::{parse_oidc_paste, CreateTokenRequest, CreateTokenResponse, TokenListItem},
    BatleHubClient,
};
use crate::config::ConfigFile;
use crate::contract::{self, ContractFile, Entry, Kind};

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Show the current identity
    Whoami,
    /// Print a valid credential, or manage API tokens with a subcommand
    ///
    /// With no subcommand this prints a credential for the configured server,
    /// refreshing it first if it is close to expiry — the one command whose
    /// job is to emit a secret, and the one a broker shells out to
    /// (RFC 0011 §4.1.3). `auth token list|create|revoke` are the personal
    /// access tokens, unchanged.
    Token {
        #[command(subcommand)]
        cmd: Option<TokenCommand>,
        #[command(flatten)]
        args: PrintTokenArgs,
    },
    /// Log in via OIDC (browser) or Kubernetes service account; saves token to config
    Login {
        /// OIDC provider name (defaults to the first configured provider)
        #[arg(long)]
        provider: Option<String>,
        /// Path to a Kubernetes service account token file to use instead of OIDC
        #[arg(long)]
        kubernetes_token_path: Option<String>,
        /// Config profile to save credentials into (defaults to 'default')
        #[arg(long)]
        profile: Option<String>,
    },
    /// Refresh if needed, then update the credential contract file that
    /// non-CLI consumers read
    ///
    /// The file is `$BATLEHUB_HOME/state/vsx-token.json` (RFC 0011 §4.1). It
    /// is written atomically and only this server's entry is touched, so a
    /// laptop pointed at three Batlehubs keeps three credentials in one file.
    WriteTokenFile {
        /// Write somewhere other than the default contract path.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Record the credential as a *path to read* rather than as the value
        /// itself — for a projected Kubernetes token, or any secret something
        /// else keeps fresh. The file stops being a place a credential rests.
        #[arg(long, value_name = "PATH")]
        from_file: Option<String>,
    },
    /// Show every registry in the contract file and whether its credential
    /// resolves right now
    ///
    /// The state is a resolution performed now, never a cached opinion: a
    /// stale "ok" from before a token file rotated is the failure being
    /// debugged.
    Status {
        /// Read a contract file other than the default one — the same
        /// `--path` `write-token-file` writes to.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Discard the stored credential: the profile's tokens and this server's
    /// contract entry
    ///
    /// **Local only.** There is no server-side session and no refresh-token
    /// revocation endpoint, so a discarded OIDC refresh token stays valid at the
    /// identity provider until it expires. Revoke it there if that matters.
    Logout {
        /// Config profile to clear (defaults to 'default')
        #[arg(long)]
        profile: Option<String>,
        /// Clear a contract file other than the default one — the same `--path`
        /// `write-token-file` writes to.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Leave the contract file alone and clear only the profile.
        #[arg(long)]
        keep_contract: bool,
    },
    /// Manually refresh a cached OIDC access token using the stored refresh token
    Refresh {
        /// OIDC provider name (defaults to the first configured provider)
        #[arg(long)]
        provider: Option<String>,
        /// Config profile whose refresh token to use (defaults to 'default')
        #[arg(long)]
        profile: Option<String>,
    },
}

#[derive(Args)]
pub struct PrintTokenArgs {
    /// `raw` prints the credential and nothing else, for `$(…)` and for a
    /// broker reading stdout. `json` adds what it is and when it expires.
    #[arg(long, default_value = "raw")]
    pub output: String,
    /// Refresh when less than this is left, in seconds. The default is the
    /// threshold the CLI already refreshes on for every other command, so
    /// this introduces no second notion of freshness.
    #[arg(long, default_value_t = 120)]
    pub min_ttl: i64,
}

#[derive(Subcommand)]
pub enum TokenCommand {
    /// List your active API tokens
    List,
    /// Create a new API token (requires OIDC session)
    Create {
        /// Display name for the token
        #[arg(long, short = 'n')]
        name: String,
        /// Lifetime in days (1–90)
        #[arg(long, short = 'd', default_value = "30")]
        days: u64,
        /// Role: user or admin
        #[arg(long, default_value = "user")]
        role: String,
        /// Groups to snapshot onto the token (comma-separated). Each must be
        /// one you already hold — `auth whoami` prints them as the server
        /// resolves them, which is the reliable way to spell one.
        #[arg(long, value_delimiter = ',', conflicts_with = "all_groups")]
        groups: Vec<String>,
        /// Snapshot every group you hold right now.
        ///
        /// Sugar, resolved here rather than on the server: it reads `auth
        /// whoami` and sends the resulting list, so the token is minted from a
        /// list you can also print, and there is one way for the server to be
        /// told what a token carries.
        #[arg(long)]
        all_groups: bool,
    },
    /// Revoke a token by its UUID
    Revoke {
        /// Token UUID
        id: uuid::Uuid,
    },
}

fn mask_token(t: &str) -> String {
    if t.len() <= 8 {
        return "****".to_string();
    }
    format!("{}…{}", &t[..4], &t[t.len() - 4..])
}

pub async fn run(
    cmd: AuthCommand,
    client: &BatleHubClient,
    json: bool,
    global_profile: Option<&str>,
    // `--token` / `BATLEHUB_TOKEN`, as the user gave it. An override is an
    // override: when one is present it *is* the credential, and neither the
    // profile store nor a refresh has anything to say about it.
    token_override: Option<&str>,
) -> Result<()> {
    match cmd {
        AuthCommand::Whoami => {
            let me = client.whoami().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&me)?);
            } else {
                let mut table = Table::new();
                table.add_row(["User ID", me.user_id.as_deref().unwrap_or("(anonymous)")]);
                table.add_row(["Role", &me.role]);
                table.add_row(["Provider", me.auth_provider.as_deref().unwrap_or("-")]);
                if !me.groups.is_empty() {
                    table.add_row(["Groups", &me.groups.join(", ")]);
                }
                println!("{table}");
            }
        }
        AuthCommand::Token { cmd, args } => match cmd {
            Some(cmd) => handle_token_command(cmd, client, json).await?,
            // No subcommand: emit a credential. Everything else that reports
            // on credentials renders a summary type that cannot hold one.
            None => handle_print_token(args, client, global_profile, token_override).await?,
        },

        AuthCommand::Login {
            provider,
            kubernetes_token_path,
            profile,
        } => {
            handle_auth_login(
                client,
                provider,
                kubernetes_token_path,
                profile,
                global_profile,
            )
            .await?
        }

        AuthCommand::Logout {
            profile,
            path,
            keep_contract,
        } => handle_auth_logout(client, profile, path, keep_contract, global_profile)?,

        AuthCommand::Refresh { provider, profile } => {
            handle_auth_refresh(client, provider, profile, global_profile).await?
        }

        AuthCommand::WriteTokenFile { path, from_file } => {
            handle_write_token_file(path, from_file, client, global_profile, token_override).await?
        }
        AuthCommand::Status { path } => handle_status(path, json)?,
    }
    Ok(())
}

// ── RFC 0011 §4.1.3: the three verbs the contract file needs ─────────────────

/// Resolve a credential for this server, refreshing when less than `min_ttl`
/// is left.
///
/// Built on the same `resolve_token` every other command already goes
/// through, so what this prints is what the CLI itself would send — the
/// alternative is a second freshness rule that disagrees with the first on
/// exactly the boundary nobody tests.
async fn resolve_fresh(
    client: &BatleHubClient,
    profile: Option<&str>,
    min_ttl: i64,
    token_override: Option<&str>,
) -> Result<(Option<String>, Option<chrono::DateTime<chrono::Utc>>, Kind)> {
    if let Some(t) = token_override {
        // Nothing to refresh and nothing to look up: the caller handed us the
        // credential. Its kind is still worth recording, so the contract file
        // says what a reader is holding.
        return Ok((Some(t.to_owned()), None, kind_of(t, false)));
    }
    let mut cfg = ConfigFile::load()?;
    // A wider `--min-ttl` than the shipped 120 s threshold: nudge the stored
    // expiry so the shared resolver considers it expiring, rather than
    // teaching it a second rule.
    if min_ttl > 120 {
        let p = match profile {
            Some(n) => cfg.profiles.entry(n.to_owned()).or_default(),
            None => &mut cfg.default,
        };
        if let Some(exp) = p.oidc_expires_at {
            if chrono::Utc::now().timestamp() >= exp - min_ttl {
                p.oidc_expires_at = Some(chrono::Utc::now().timestamp());
            }
        }
    }
    let token = crate::api::auth::resolve_token(&client.base_url, profile, &mut cfg).await?;

    let p = match profile {
        Some(n) => cfg.profiles.get(n).cloned().unwrap_or_default(),
        None => cfg.default.clone(),
    };
    let kind = kind_of(
        token.as_deref().unwrap_or_default(),
        p.kubernetes_token_path.is_some(),
    );
    let expires_at = match kind {
        // A PAT's expiry is the server's to know; a mounted token's is
        // whatever wrote it. Claiming either here would be inventing one.
        Kind::Oidc => p
            .oidc_expires_at
            .and_then(|e| chrono::DateTime::from_timestamp(e, 0)),
        _ => None,
    };
    Ok((token, expires_at, kind))
}

/// What the credential is, from what it looks like. The `bh_pat_` prefix is
/// the server's own dispatch rule, so this agrees with the thing that will
/// judge it rather than guessing separately.
fn kind_of(token: &str, from_mounted_file: bool) -> Kind {
    if from_mounted_file {
        Kind::Kubernetes
    } else if token.starts_with("bh_pat_") {
        Kind::Pat
    } else {
        Kind::Oidc
    }
}

async fn handle_print_token(
    args: PrintTokenArgs,
    client: &BatleHubClient,
    profile: Option<&str>,
    token_override: Option<&str>,
) -> Result<()> {
    let (token, expires_at, kind) =
        resolve_fresh(client, profile, args.min_ttl, token_override).await?;
    let Some(token) = token else {
        // Non-zero, because a caller substituting this into a header needs to
        // know it got nothing — and a broker reading stdout would otherwise
        // send an empty bearer.
        bail!(
            "no credential for {}: run `batlehub-cli auth login`",
            client.base_url
        );
    };
    // An expired credential is not a credential. `resolve_token` refreshes only
    // when a refresh token exists, and on a failed refresh it warns and falls
    // back to the *stored* value — so without this check a stale access token
    // was printed with exit 0 and the broker substituting stdout into a header
    // sent a bearer that comes back 401, indistinguishable from an
    // authorization bug. The promise this command makes is a non-zero exit.
    if let Some(exp) = expires_at {
        let left = (exp - chrono::Utc::now()).num_seconds();
        if left < args.min_ttl.max(0) {
            bail!(
                "the credential for {} has {}s left (less than the {}s asked for) and could not be \
                 refreshed: run `batlehub-cli auth login`",
                client.base_url,
                left,
                args.min_ttl
            );
        }
    }
    match args.output.as_str() {
        "raw" => println!("{token}"),
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "registry": contract::normalize_origin(&client.base_url),
                "token": token,
                "kind": kind.as_str(),
                "expires_at": expires_at,
            }))?
        ),
        other => bail!("--output is `raw` or `json`, not `{other}`"),
    }
    Ok(())
}

async fn handle_write_token_file(
    path: Option<PathBuf>,
    from_file: Option<String>,
    client: &BatleHubClient,
    profile: Option<&str>,
    token_override: Option<&str>,
) -> Result<()> {
    let path = path.unwrap_or_else(contract::contract_path);
    let registry = contract::normalize_origin(&client.base_url);

    let entry = match from_file {
        // The credential stays where it is and something else keeps it
        // fresh: the file records a path, not a secret.
        Some(p) => {
            let entry = Entry::from_file(p, Kind::Kubernetes);
            // Resolve it once now. A path that yields nothing is legitimate
            // — a projected token in a pod that has not started yet — so
            // this is a warning and not a refusal, but learning it here beats
            // learning it from an editor that quietly shows no extensions.
            let r = entry.resolve();
            match r.token {
                Some(_) => {}
                None => eprintln!(
                    "Warning: that path yields no credential right now ({}).                      The entry is written anyway; `auth status` re-checks it.",
                    r.detail.as_deref().unwrap_or("no reason given")
                ),
            }
            entry
        }
        None => {
            let (token, expires_at, kind) =
                resolve_fresh(client, profile, 120, token_override).await?;
            let Some(token) = token else {
                bail!("no credential for {registry}: run `batlehub-cli auth login`");
            };
            Entry::literal(token, kind, expires_at)
        }
    };
    contract::validate_entry(&entry)?;

    // Read-modify-write: this writer owns one entry and must preserve every
    // other, including fields it does not know about. `try_load` rather than
    // `load`, because overwriting a file we could not parse would discard
    // another registry's credential.
    let mut doc = ContractFile::try_load(&path)
        .map_err(|e| anyhow::anyhow!("{} is not a usable contract file: {e}", path.display()))?;
    // Whether this replaces something matters to a reader of the output: a
    // "written" line that silently replaced another credential for the same
    // origin is the one case where this command is not additive.
    let replaced = doc.entry(&registry).is_some();
    doc.set_entry(&registry, entry);
    doc.save(&path)?;

    println!(
        "{registry} {} in {}",
        if replaced { "replaced" } else { "written" },
        path.display()
    );
    Ok(())
}

fn handle_status(path: Option<PathBuf>, json: bool) -> Result<()> {
    let path = path.unwrap_or_else(contract::contract_path);
    let doc = ContractFile::load(&path);
    let rows: Vec<_> = doc
        .registries
        .iter()
        .map(|(origin, e)| e.summary(origin))
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("No credentials in {}.", path.display());
        println!("`batlehub-cli auth login` then `auth write-token-file` puts one there.");
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header([
        "Registry",
        "Kind",
        "Token source",
        "State",
        "Expires",
        "Refresh",
    ]);
    for r in &rows {
        table.add_row([
            r.registry.as_str(),
            r.kind,
            r.source.as_str(),
            r.state,
            r.expires_in.as_str(),
            r.refresh.as_str(),
        ]);
    }
    println!("{table}");
    for r in rows.iter().filter(|r| r.detail.is_some()) {
        // `unset` and `refused` look identical from the editor and want
        // opposite fixes, so the reason is printed rather than left to be
        // guessed at.
        println!("  {}: {}", r.registry, r.detail.as_deref().unwrap_or(""));
    }
    println!(
        "
Contract file: {}",
        path.display()
    );
    Ok(())
}

async fn handle_token_command(
    cmd: TokenCommand,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    match cmd {
        TokenCommand::List => {
            let tokens = client.list_tokens().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&tokens)?);
            } else {
                print_tokens_table(&tokens);
            }
        }
        TokenCommand::Create {
            name,
            days,
            role,
            groups,
            all_groups,
        } => {
            let groups = if all_groups {
                client.whoami().await?.groups
            } else {
                groups
            };
            let resp = client
                .create_token(CreateTokenRequest {
                    name: name.clone(),
                    expires_in_days: days,
                    role: role.clone(),
                    groups,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_created_token(&name, &role, &resp);
            }
        }
        TokenCommand::Revoke { id } => {
            client.revoke_token(id).await?;
            println!("Revoked token {id}");
        }
    }
    Ok(())
}

fn print_tokens_table(tokens: &[TokenListItem]) {
    let mut table = Table::new();
    table.set_header(["ID", "Name", "Role", "Expires", "Groups"]);
    for t in tokens {
        // A snapshot goes stale silently, so the listing shows it: this is
        // where an owner sees that a token still carries a team they left.
        let groups = if t.groups.is_empty() {
            "-".to_owned()
        } else {
            t.groups.join(", ")
        };
        table.add_row([
            &t.id.to_string(),
            &t.name,
            &t.role,
            &t.expires_at.format("%Y-%m-%d").to_string(),
            &groups,
        ]);
    }
    println!("{table}");
    println!("{} token(s)", tokens.len());
}

fn print_created_token(name: &str, role: &str, resp: &CreateTokenResponse) {
    println!(
        "Created token '{name}' (role: {role}, expires: {})",
        resp.expires_at.format("%Y-%m-%d")
    );
    // Printed even when empty: "carries no groups" is the answer that surprises
    // someone whose pipeline then cannot see a team package, and it is cheaper
    // to read here than to diagnose.
    if resp.groups.is_empty() {
        println!("Groups: none — this token sees only public and internal packages");
    } else {
        println!("Groups: {}", resp.groups.join(", "));
    }
    println!();
    println!("Token (store this — it will not be shown again):");
    println!("  {}", resp.token);
}

async fn handle_auth_login(
    client: &BatleHubClient,
    provider: Option<String>,
    kubernetes_token_path: Option<String>,
    profile: Option<String>,
    global_profile: Option<&str>,
) -> Result<()> {
    let target_profile = profile.as_deref().or(global_profile);
    let mut cfg = ConfigFile::load()?;

    if let Some(k8s_path) = kubernetes_token_path {
        let entry = match target_profile {
            Some(n) => cfg.profiles.entry(n.to_string()).or_default(),
            None => &mut cfg.default,
        };
        entry.kubernetes_token_path = Some(k8s_path.clone());
        entry.token = None;
        entry.oidc_refresh_token = None;
        entry.oidc_expires_at = None;
        cfg.save()?;
        println!("Kubernetes token path saved: {k8s_path}");
        println!("The token will be read fresh from this path on each request.");
        return Ok(());
    }

    let providers = client.list_oidc_providers().await.unwrap_or_default();
    if providers.is_empty() {
        anyhow::bail!(
            "OIDC is not configured on this server. \
            Use `auth token create` for static tokens, or \
            `auth login --kubernetes-token-path <path>` for Kubernetes."
        );
    }

    let csrf = uuid::Uuid::new_v4().to_string();
    let login_url = client.oidc_login_url(&csrf, provider.as_deref()).await?;

    println!("Open this URL in your browser:");
    println!();
    println!("  {login_url}");
    println!();
    println!("After login you will land on a URL containing oidc_access_token=…");
    println!("Paste the full URL (or just the token value):");
    print!("> ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("No input provided — login cancelled.");
    }

    let paste = parse_oidc_paste(input);

    // The `csrf` value above was generated, sent, and until now never checked.
    // The server confirms the state is one it issued and has not already
    // redeemed, but it cannot tell that the login started in *this* process —
    // this comparison is the half only the client can do.
    //
    // A bare token carries no state, so it cannot be checked. That is accepted
    // rather than refused: it is the escape hatch for a user whose browser
    // mangled the URL, and it is their own terminal they would be attacking.
    match paste.state.as_deref() {
        Some(state) if state == csrf => {}
        Some(_) => anyhow::bail!(
            "The pasted URL is from a different sign-in than the one just started. \
             Run `auth login` again and paste the URL it sends you to."
        ),
        None => eprintln!(
            "Warning: pasted a bare token, so this sign-in could not be matched \
             to the one just started. Paste the full URL to have it checked."
        ),
    }

    let entry = match target_profile {
        Some(n) => cfg.profiles.entry(n.to_string()).or_default(),
        None => &mut cfg.default,
    };
    let access_token = paste.access_token;
    entry.token = Some(access_token.clone());
    entry.oidc_refresh_token = paste.refresh_token;
    entry.oidc_expires_at = paste.expires_at;
    // Recorded, because `resolve_token` sends it with every refresh. Without it
    // the server falls back to `flows.first()`, so on a deployment with more
    // than one provider the refresh token goes to the wrong token endpoint,
    // comes back `400`, and the CLI keeps presenting an expired access token.
    // The server's own echo wins over `--provider`: it names the flow that
    // actually issued this token.
    entry.oidc_provider = paste.provider.or_else(|| provider.clone());
    entry.kubernetes_token_path = None;
    cfg.save()?;

    println!(
        "Logged in. Token saved to profile '{}'.",
        target_profile.unwrap_or("default")
    );
    println!("  {}", mask_token(&access_token));
    Ok(())
}

/// RFC 0011 §4.1.3's `auth logout`.
///
/// Two stores hold a credential and this clears both: the profile in
/// `~/.config/batlehub/config.toml`, and this server's entry in the contract
/// file the editor reads. Neither is a server round trip — the function is
/// synchronous on purpose, so logging out of a server that is down still works.
///
/// It never deletes a file an entry *points at*. A `from = "file"` entry names a
/// path the CLI does not own — a projected Kubernetes token — and removing that
/// would break the workload the credential belongs to rather than this CLI's
/// view of it (§4.5.1).
fn handle_auth_logout(
    client: &BatleHubClient,
    profile: Option<String>,
    path: Option<PathBuf>,
    keep_contract: bool,
    global_profile: Option<&str>,
) -> Result<()> {
    let target_profile = profile.as_deref().or(global_profile);

    // ── the profile store ───────────────────────────────────────────────────
    let mut cfg = ConfigFile::load()?;
    let entry = match target_profile {
        Some(n) => cfg.profiles.get_mut(n),
        None => Some(&mut cfg.default),
    };
    let cleared_profile = match entry {
        Some(p) => {
            // Every field a login or a refresh can write. Listing them rather
            // than assigning `Profile::default()` keeps `server_url` and
            // `registry`, which are settings and not credentials — a logout
            // that forgot which server you talk to would be a worse command.
            let had = p.token.is_some()
                || p.oidc_refresh_token.is_some()
                || p.kubernetes_token_path.is_some();
            p.token = None;
            p.oidc_refresh_token = None;
            p.oidc_expires_at = None;
            p.oidc_provider = None;
            p.kubernetes_token_path = None;
            had
        }
        // A `--profile` naming one that was never created holds no credential,
        // which is the state logout is trying to reach.
        None => false,
    };
    cfg.save()?;

    // ── the contract file ───────────────────────────────────────────────────
    let registry = contract::normalize_origin(&client.base_url);
    let cleared_contract = if keep_contract {
        false
    } else {
        let path = path.unwrap_or_else(contract::contract_path);
        // `try_load`, as the writer does: rewriting a file we could not parse
        // would discard another registry's credential.
        match ContractFile::try_load(&path) {
            Ok(mut doc) => {
                let removed = doc.clear_entry(&registry);
                if removed {
                    doc.save(&path)?;
                }
                removed
            }
            // A contract file that does not parse is not something to silently
            // rewrite, and not a reason to fail a logout that already cleared
            // the profile.
            Err(e) => {
                eprintln!(
                    "Warning: left {} alone — it is not a usable contract file: {e}",
                    path.display()
                );
                false
            }
        }
    };

    let name = target_profile.unwrap_or("default");
    match (cleared_profile, cleared_contract) {
        (false, false) => println!("Nothing to clear for profile '{name}'."),
        (true, false) => println!("Cleared the credential in profile '{name}'."),
        (false, true) => println!("Cleared the contract entry for {registry}."),
        (true, true) => println!(
            "Cleared the credential in profile '{name}' and the contract entry for {registry}."
        ),
    }
    if cleared_profile {
        println!("  The identity provider was not told: revoke the session there if it matters.");
    }
    Ok(())
}

async fn handle_auth_refresh(
    client: &BatleHubClient,
    provider: Option<String>,
    profile: Option<String>,
    global_profile: Option<&str>,
) -> Result<()> {
    let target_profile = profile.as_deref().or(global_profile);
    let mut cfg = ConfigFile::load()?;

    let refresh_token = {
        let entry = match target_profile {
            Some(n) => cfg.profiles.get(n),
            None => Some(&cfg.default),
        };
        entry
            .and_then(|p| p.oidc_refresh_token.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No OIDC refresh token stored for profile '{}'. \
                    Run `auth login` first.",
                    target_profile.unwrap_or("default")
                )
            })?
    };

    let (access_token, new_refresh, expires_in) = client
        .oidc_refresh(&refresh_token, provider.as_deref())
        .await?;

    let entry = match target_profile {
        Some(n) => cfg.profiles.entry(n.to_string()).or_default(),
        None => &mut cfg.default,
    };
    entry.token = Some(access_token);
    if let Some(rt) = new_refresh {
        entry.oidc_refresh_token = Some(rt);
    }
    if let Some(exp) = expires_in {
        entry.oidc_expires_at = Some(chrono::Utc::now().timestamp() + exp as i64);
    }
    cfg.save()?;
    println!("Token refreshed successfully.");
    Ok(())
}
