use anyhow::Result;
use clap::Subcommand;

use crate::config::ConfigFile;

/// `profile …` (RFC 0040 §4.2): the local file, by the name the server's
/// configuration (`server config`) does not use.
#[derive(Subcommand)]
pub enum ProfileCommand {
    /// The profiles in the file, and which one is in use
    List,
    /// One profile — by default the one in use — with the token masked
    Show {
        /// Profile name
        name: Option<String>,
        /// Print only this key's value
        #[arg(long)]
        key: Option<String>,
    },
    /// Write a key (server_url, token or registry) in the `-P` profile
    Set {
        /// server_url | token | registry
        key: String,
        value: String,
    },
    /// Make a profile the one used when `-P` is not given
    Use { name: String },
    /// Interactive setup of the default profile
    Init,
}

const KEYS: &str = "server_url, token or registry";

/// `global_profile` is `-P`/`BATLEHUB_PROFILE`; the file's `current` key
/// answers when it is absent.
pub fn run_profile(cmd: ProfileCommand, global_profile: Option<&str>) -> Result<()> {
    let mut cfg = ConfigFile::load()?;
    let in_use = cfg.selected(global_profile);
    match cmd {
        ProfileCommand::Init => interactive_init()?,
        ProfileCommand::List => {
            let mut names = vec!["default".to_owned()];
            let mut others: Vec<String> = cfg.profiles.keys().cloned().collect();
            others.sort();
            names.extend(others);
            let current = in_use.unwrap_or("default");
            if crate::render::ctx().structured() {
                crate::render::data(
                    &names
                        .iter()
                        .map(|n| serde_json::json!({"name": n, "current": n == current}))
                        .collect::<Vec<_>>(),
                )?;
            } else {
                for n in &names {
                    let mark = if n == current { "*" } else { " " };
                    println!("{mark} {n}");
                }
            }
        }
        ProfileCommand::Show { name, key } => {
            let name = name.as_deref().or(in_use);
            let resolved = cfg.resolve(name, None, None, None);
            let token = resolved
                .token
                .as_deref()
                .map(mask_token)
                .unwrap_or_default();
            if let Some(key) = key {
                let value = match key.as_str() {
                    "server_url" => resolved.server_url,
                    "token" => token,
                    "registry" => resolved.registry.unwrap_or_default(),
                    other => anyhow::bail!("unknown key '{other}'; expected {KEYS}"),
                };
                println!("{value}");
            } else if crate::render::ctx().structured() {
                crate::render::data(&serde_json::json!({
                    "name": name.unwrap_or("default"),
                    "server_url": resolved.server_url,
                    "token": (!token.is_empty()).then_some(token),
                    "registry": resolved.registry,
                    "path": ConfigFile::config_path(),
                }))?;
            } else {
                println!("profile:    {}", name.unwrap_or("default"));
                println!("server_url: {}", resolved.server_url);
                println!(
                    "token:      {}",
                    if token.is_empty() {
                        "(not set)"
                    } else {
                        &token
                    }
                );
                println!(
                    "registry:   {}",
                    resolved.registry.as_deref().unwrap_or("(not set)")
                );
                println!();
                println!("Config file: {}", ConfigFile::config_path().display());
            }
        }
        ProfileCommand::Set { key, value } => {
            let target = in_use.map(str::to_owned);
            let entry = match target.as_deref() {
                Some(name) => cfg.profiles.entry(name.to_string()).or_default(),
                None => &mut cfg.default,
            };
            match key.as_str() {
                "server_url" => entry.server_url = Some(value.clone()),
                "token" => {
                    entry.token = Some(value.clone());
                    crate::render::warn(
                        "a static token is stored as-is: `auth login` refreshes its credential, \
                         this one is never refreshed",
                    );
                }
                "registry" => entry.registry = Some(value.clone()),
                other => anyhow::bail!("unknown key '{other}'; expected {KEYS}"),
            }
            cfg.save()?;
            let shown = if key == "token" {
                mask_token(&value)
            } else {
                value
            };
            println!(
                "Set {key} = {shown} in profile {}",
                target.as_deref().unwrap_or("default")
            );
        }
        ProfileCommand::Use { name } => {
            if name != "default" && !cfg.profiles.contains_key(&name) {
                return Err(crate::exit::CliError::new(
                    crate::exit::ExitCode::NotFound,
                    format!("no profile named `{name}`"),
                )
                .hint("`batlehub-cli profile list` names them")
                .into());
            }
            cfg.current = (name != "default").then(|| name.clone());
            cfg.save()?;
            println!("Using profile {name}");
        }
    }
    Ok(())
}

fn interactive_init() -> Result<()> {
    use std::io::{self, Write};

    println!("BatleHub CLI — initial setup");
    println!("(press Enter to accept the default shown in brackets)");
    println!();

    let server_url = prompt("Server URL", "http://localhost:8080")?;
    let token = prompt("Auth token", "")?;
    let registry = prompt("Default registry (optional)", "")?;

    let mut cfg = ConfigFile::load().unwrap_or_default();
    cfg.default.server_url = Some(server_url);
    if !token.is_empty() {
        cfg.default.token = Some(token);
    }
    if !registry.is_empty() {
        cfg.default.registry = Some(registry);
    }
    cfg.save()?;
    println!();
    println!("Config written to {}", ConfigFile::config_path().display());

    fn prompt(label: &str, default: &str) -> io::Result<String> {
        if default.is_empty() {
            print!("{label}: ");
        } else {
            print!("{label} [{default}]: ");
        }
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            Ok(default.to_string())
        } else {
            Ok(trimmed.to_string())
        }
    }

    Ok(())
}

fn mask_token(t: &str) -> String {
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let first: String = chars[..4].iter().collect();
    let last: String = chars[chars.len() - 4..].iter().collect();
    format!("{first}…{last}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_token_short_token_returns_stars() {
        assert_eq!(mask_token("short"), "****");
        assert_eq!(mask_token("exactly8"), "****");
    }

    #[test]
    fn mask_token_long_ascii_token_shows_first_and_last_four() {
        assert_eq!(mask_token("abcdefghijklmnop"), "abcd…mnop");
    }

    #[test]
    fn mask_token_does_not_panic_on_multibyte_chars_near_boundary() {
        // A multi-byte character sitting right at the 4-byte offset from
        // either end used to panic with byte-index slicing; char-based
        // slicing must handle it cleanly instead.
        let token = "€€€€abcdefgh€€€€";
        let masked = mask_token(token);
        assert!(masked.starts_with('€'));
        assert!(masked.ends_with('€'));
    }
}
