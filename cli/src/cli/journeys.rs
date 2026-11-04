//! The journeys of RFC 0040 §4.9: endpoints the console had and the CLI did
//! not — policy and exemptions, the staged configuration, raising and revoking
//! a flag, the caller's own state, the upstream audit and namespace signing
//! keys. `package block/unblock` lives with the other version verbs
//! (`lifecycle.rs`).

use anyhow::{Context as _, Result};
use clap::Subcommand;
use serde_json::{json, Value};

use super::admin::FlagsCommand;
use crate::api::journeys::{DisappearedQuery, DownloadsQuery};
use crate::api::BatleHubClient;
use crate::confirm::confirm;
use crate::coordinate::Coordinate;
use crate::render::{ctx, data, show};

async fn coord(client: &BatleHubClient, raw: &str) -> Result<Coordinate> {
    Ok(Coordinate::resolve(client, raw, ctx().registry.as_deref()).await?)
}

/// A JSON document from a file, or from stdin for `-`.
fn read_json(path: &str) -> Result<Value> {
    let text = if path == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?
    };
    serde_json::from_str(&text).with_context(|| format!("{path} is not JSON"))
}

/// Done, on stderr, so stdout stays data (§4.3).
fn done(message: &str) {
    if !ctx().structured() {
        eprintln!("{message}");
    }
}

// ── policy · exemption ───────────────────────────────────────────────────────

#[derive(clap::Args)]
pub struct ExemptionArgs {
    /// The gate to exempt the version from, e.g. `cve_gate`
    #[arg(long)]
    pub gate: String,
    /// When the exemption lapses, RFC 3339 (`2026-12-31T00:00:00Z`)
    #[arg(long)]
    pub until: String,
    /// Why the gate does not apply; kept with the exemption
    #[arg(long)]
    pub reason: String,
}

#[derive(Subcommand)]
pub enum PolicyCmd {
    /// A package's policy (`registry/name`) or a version's (`registry/name@version`) [admin]
    Show { coordinate: String },
    /// Write a node's policy from a JSON file (`-` for stdin; absent fields
    /// inherit), or with `--gate` exempt a version from one gate [admin]
    Set {
        coordinate: String,
        #[arg(
            long,
            short = 'f',
            required_unless_present = "gate",
            conflicts_with = "gate"
        )]
        file: Option<String>,
        #[arg(long, requires_all = ["until", "reason"])]
        gate: Option<String>,
        /// With `--gate`: when the exemption lapses, RFC 3339
        #[arg(long)]
        until: Option<String>,
        /// With `--gate`: why the gate does not apply
        #[arg(long)]
        reason: Option<String>,
    },
    /// Delete a node's policy, or one gate exemption with `--gate` (destructive: asks first) [admin]
    Delete {
        coordinate: String,
        #[arg(long)]
        gate: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ExemptionCmd {
    /// The gate exemptions of the `-r` registry, expired ones included [admin]
    List,
    /// Exempt a version from one gate until a date [admin]
    Add {
        /// `registry/name@version`
        coordinate: String,
        #[command(flatten)]
        args: ExemptionArgs,
    },
    /// Remove a version's exemption from one gate [admin]
    Remove {
        coordinate: String,
        #[arg(long)]
        gate: String,
    },
}

pub async fn run_policy(cmd: PolicyCmd, client: &BatleHubClient) -> Result<()> {
    match cmd {
        PolicyCmd::Show { coordinate } => {
            let c = coord(client, &coordinate).await?;
            let v = client
                .policy_show(&c.registry, &c.name, c.version.as_deref())
                .await?;
            show(&v, &Value::Null, &[])
        }
        PolicyCmd::Set {
            coordinate,
            gate: Some(gate),
            until,
            reason,
            ..
        } => {
            let args = ExemptionArgs {
                gate,
                until: until.unwrap_or_default(),
                reason: reason.unwrap_or_default(),
            };
            run_exemption(ExemptionCmd::Add { coordinate, args }, client).await
        }
        PolicyCmd::Set {
            coordinate, file, ..
        } => {
            let c = coord(client, &coordinate).await?;
            let body = read_json(file.as_deref().unwrap_or("-"))?;
            client
                .policy_set(&c.registry, &c.name, c.version.as_deref(), &body)
                .await?;
            done(&format!("Policy set on {c}"));
            Ok(())
        }
        PolicyCmd::Delete { coordinate, gate } => {
            let c = coord(client, &coordinate).await?;
            match gate {
                Some(gate) => {
                    let shown = c.to_string();
                    let item = c.require_version()?;
                    confirm(
                        &format!("remove the {gate} exemption of"),
                        std::slice::from_ref(&shown),
                        None,
                    )?;
                    client.exemption_delete(&item, &gate).await?;
                    done(&format!("Removed the {gate} exemption of {shown}"));
                }
                None => {
                    confirm(
                        "delete the policy of",
                        &[c.to_string()],
                        Some("the node inherits the tier above again"),
                    )?;
                    client
                        .policy_delete(&c.registry, &c.name, c.version.as_deref())
                        .await?;
                    done(&format!("Deleted the policy of {c}"));
                }
            }
            Ok(())
        }
    }
}

pub async fn run_exemption(cmd: ExemptionCmd, client: &BatleHubClient) -> Result<()> {
    match cmd {
        ExemptionCmd::List => {
            let v = client.exemptions(&ctx().require_registry(None)?).await?;
            show(
                &v,
                &v,
                &[
                    "package",
                    "version",
                    "gate",
                    "exempt_until",
                    "expired",
                    "reason",
                ],
            )
        }
        ExemptionCmd::Add { coordinate, args } => {
            let c = coord(client, &coordinate).await?;
            let shown = c.to_string();
            let item = c.require_version()?;
            let v = client
                .exemption_set(&item, &args.gate, &args.until, &args.reason)
                .await?;
            if ctx().structured() {
                return data(&v);
            }
            println!("Exempted {shown} from {} until {}", args.gate, args.until);
            if v["self_approved"] == true {
                eprintln!("note: self-approved — you also published or own this version");
            }
            Ok(())
        }
        ExemptionCmd::Remove { coordinate, gate } => {
            let c = coord(client, &coordinate).await?;
            let shown = c.to_string();
            client
                .exemption_delete(&c.require_version()?, &gate)
                .await?;
            done(&format!("Removed the {gate} exemption of {shown}"));
            Ok(())
        }
    }
}

// ── server config pending · apply · discard · warnings ───────────────────────

/// The staged-configuration verbs, run by `groups.rs`'s `server config`.
pub enum PendingCmd {
    Pending,
    Apply,
    Discard,
    Warnings,
}

/// What a reload changes, one line per non-empty field of the diff.
fn print_diff(diff: &Value) {
    let Value::Object(o) = diff else { return };
    let mut any = false;
    for (k, v) in o {
        let line = match v {
            Value::Array(a) if !a.is_empty() => a
                .iter()
                .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_owned))
                .collect::<Vec<_>>()
                .join(", "),
            Value::Bool(true) => "yes".to_owned(),
            Value::String(s) => s.clone(),
            _ => continue,
        };
        any = true;
        println!("  {k}: {line}");
    }
    if !any {
        println!("  (no change)");
    }
}

fn print_warnings(warnings: &Value) {
    for w in warnings.as_array().into_iter().flatten() {
        println!(
            "warning [{}] {}: {}",
            w["code"].as_str().unwrap_or("?"),
            w["path"].as_str().unwrap_or(""),
            w["message"].as_str().unwrap_or("")
        );
    }
}

pub async fn run_pending(cmd: PendingCmd, client: &BatleHubClient) -> Result<()> {
    match cmd {
        PendingCmd::Pending => {
            let v = client.config_pending().await?;
            if ctx().structured() {
                return data(&v);
            }
            println!(
                "Pending {} ({}), staged {}, expires {}",
                v["id"].as_str().unwrap_or("?"),
                v["source"].as_str().unwrap_or("?"),
                v["created_at"].as_str().unwrap_or("?"),
                v["expires_at"].as_str().unwrap_or("?")
            );
            print_diff(&v["diff"]);
            print_warnings(&v["warnings"]);
            Ok(())
        }
        PendingCmd::Apply => {
            confirm("apply", &["the pending configuration".to_owned()], None)?;
            let v = client.config_apply().await?;
            if ctx().structured() {
                return data(&v);
            }
            println!("Applied:");
            print_diff(&v["diff"]);
            print_warnings(&v["warnings"]);
            Ok(())
        }
        PendingCmd::Discard => {
            confirm("discard", &["the pending configuration".to_owned()], None)?;
            client.config_discard().await?;
            done("Discarded the pending configuration");
            Ok(())
        }
        PendingCmd::Warnings => {
            let v = client.config_warnings().await?;
            show(&v, &v["warnings"], &["code", "path", "message"])
        }
    }
}

// ── flag ─────────────────────────────────────────────────────────────────────

/// The source's shared secret: from the environment, so it stays out of the
/// shell history and the process list.
#[derive(clap::Args)]
pub struct FlagSecret {
    /// The `[[flag_sources]]` name the flag is raised as
    #[arg(long)]
    pub source: String,
    #[arg(long, env = "BATLEHUB_FLAG_SECRET", hide_env_values = true)]
    pub secret: String,
}

#[derive(Subcommand)]
pub enum FlagCmd {
    #[command(flatten)]
    Listing(FlagsCommand),
    /// Raise a flag as a `[[flag_sources]]` entry: `registry/name@version`, or
    /// `registry/name` for every version (RFC 0002)
    Create {
        coordinate: String,
        #[command(flatten)]
        signer: FlagSecret,
        /// The source's own id for it; the same id updates the flag
        #[arg(long)]
        external_id: String,
        /// inform, warn, gate or hard_block (capped by the source's ceiling)
        #[arg(long)]
        effect: String,
        #[arg(long)]
        summary: String,
        /// low, medium, high or critical
        #[arg(long)]
        severity: Option<String>,
        /// e.g. malware, vulnerability, license
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        url: Option<String>,
        /// RFC 3339
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// Revoke a flag the source raised (destructive: asks first)
    Delete {
        external_id: String,
        #[command(flatten)]
        signer: FlagSecret,
    },
}

pub async fn run_flag(cmd: FlagCmd, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        FlagCmd::Listing(_) => unreachable!("`flag list` runs through groups.rs"),
        FlagCmd::Create {
            coordinate,
            signer,
            external_id,
            effect,
            summary,
            severity,
            kind,
            url,
            expires_at,
        } => {
            let c = coord(client, &coordinate).await?;
            let mut flag = json!({
                "external_id": external_id,
                "registry": c.registry,
                "package_name": c.name,
                "effect": effect,
                "summary": summary,
                "severity": severity,
                "url": url,
                "expires_at": expires_at,
            });
            match &c.version {
                Some(v) => flag["version"] = json!(v),
                None => flag["version_range"] = json!("*"),
            }
            if let Some(k) = kind {
                flag["kind"] = json!(k);
            }
            let body = serde_json::to_vec(&json!({ "flags": [flag] }))?;
            let v = client
                .flag_push(&signer.source, &signer.secret, body)
                .await?;
            if json {
                data(&v)?;
            } else {
                let item = &v["items"][0];
                match item["status"].as_str() {
                    Some("accepted") => {
                        let verb = if item["created"] == true {
                            "Raised"
                        } else {
                            "Updated"
                        };
                        println!(
                            "{verb} {external_id} on {c}: {}",
                            item["effect"].as_str().unwrap_or("?")
                        );
                        if item["effect_capped"] == true {
                            eprintln!("note: the effect was capped by the source's ceiling");
                        }
                    }
                    _ => {
                        return Err(crate::exit::CliError::new(
                            crate::exit::ExitCode::Failure,
                            format!("rejected: {}", item["error"].as_str().unwrap_or("?")),
                        )
                        .into())
                    }
                }
            }
        }
        FlagCmd::Delete {
            external_id,
            signer,
        } => {
            confirm(
                "revoke",
                &[format!("{}/{external_id}", signer.source)],
                Some("the versions it held are served again"),
            )?;
            let v = client
                .flag_revoke(&signer.source, &external_id, &signer.secret)
                .await?;
            if json {
                data(&v)?;
            } else if v["revoked"] == true {
                println!("Revoked {external_id}");
            } else {
                println!("{external_id} was already revoked");
            }
        }
    }
    Ok(())
}

// ── me ───────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum MeCmd {
    /// Who the server takes you for: user, role, groups
    Show,
    /// Your quota usage, per registry
    Quota,
    /// Your recent downloads
    Downloads {
        /// Only the refused ones, with their reason code (pass one to `why`)
        #[arg(long)]
        denied: bool,
        #[arg(long, default_value_t = 50)]
        limit: u64,
        /// Only the last N days
        #[arg(long)]
        days: Option<u64>,
    },
    /// Advisories on versions you published or pulled
    Advisories,
    /// The team namespaces you belong to
    Namespaces,
}

pub async fn run_me(cmd: MeCmd, client: &BatleHubClient) -> Result<()> {
    match cmd {
        MeCmd::Show => {
            let v = client.me_show().await?;
            show(&v, &Value::Null, &[])
        }
        MeCmd::Quota => {
            let v = client.me_quota().await?;
            show(
                &v,
                &v,
                &[
                    "registry",
                    "state",
                    "bytes_used",
                    "bytes_limit",
                    "packages_used",
                    "packages_limit",
                ],
            )
        }
        MeCmd::Downloads {
            denied,
            limit,
            days,
        } => {
            let q = DownloadsQuery {
                limit,
                days,
                outcome: denied.then_some("denied"),
            };
            let v = client.me_downloads(&q).await?;
            let mut cols = vec!["downloaded_at", "registry", "name", "version"];
            if denied {
                cols.push("reason_code");
            } else {
                cols.push("blocked");
            }
            show(&v, &v, &cols)
        }
        MeCmd::Advisories => {
            let v = client.me_advisories().await?;
            if v["scanning_available"] == false && !ctx().structured() {
                eprintln!("note: no scanner runs on this instance; the list is flags only");
            }
            show(
                &v,
                &v["advisories"],
                &[
                    "registry",
                    "name",
                    "version",
                    "highest_severity",
                    "relation",
                ],
            )
        }
        MeCmd::Namespaces => {
            let v = client.me_namespaces().await?;
            show(&v, &v, &["registry", "prefix", "group_id", "package_count"])
        }
    }
}

// ── upstream ─────────────────────────────────────────────────────────────────

const UPSTREAM_COLUMNS: &[&str] = &[
    "registry",
    "package_name",
    "version",
    "state",
    "consecutive_misses",
    "first_missed_at",
    "last_checked_at",
];

#[derive(Subcommand)]
pub enum UpstreamCmd {
    /// What the upstream audit knows about a package: `registry/name` [admin]
    Status { coordinate: String },
    /// Probe the upstream for a package, or one version, now [admin]
    Recheck { coordinate: String },
    /// Packages and versions the upstream no longer lists; `-r` narrows to one registry [admin]
    Disappeared {
        /// missing, disappeared or restored
        #[arg(long)]
        state: Option<String>,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long, default_value_t = 50)]
        per_page: u64,
    },
}

pub async fn run_upstream(cmd: UpstreamCmd, client: &BatleHubClient) -> Result<()> {
    match cmd {
        UpstreamCmd::Status { coordinate } => {
            let c = coord(client, &coordinate).await?;
            let v = client.upstream_status(&c.registry, &c.name).await?;
            show(&v, &v["rows"], UPSTREAM_COLUMNS)
        }
        UpstreamCmd::Recheck { coordinate } => {
            let c = coord(client, &coordinate).await?;
            let v = client
                .upstream_recheck(&c.registry, &c.name, c.version.as_deref())
                .await?;
            if ctx().structured() {
                return data(&v);
            }
            println!(
                "{c}: probed {}, missing {}, inconclusive {}",
                v["probed"], v["missing"], v["inconclusive"]
            );
            for t in v["transitions"].as_array().into_iter().flatten() {
                println!(
                    "  {}",
                    t.as_str().map_or_else(|| t.to_string(), str::to_owned)
                );
            }
            Ok(())
        }
        UpstreamCmd::Disappeared {
            state,
            page,
            per_page,
        } => {
            let q = DisappearedQuery {
                registry: ctx().registry.clone(),
                state,
                page,
                per_page,
            };
            let v = client.upstream_disappeared(&q).await?;
            show(&v, &v["items"], UPSTREAM_COLUMNS)
        }
    }
}

// ── namespace signing-key ────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum SigningKeyCmd {
    /// The keys a namespace's releases are signed with, in the `-r` registry [admin]
    List { namespace: String },
    /// Add or replace a namespace's key from an ASCII-armored public key file [admin]
    Set {
        namespace: String,
        /// The key id, e.g. `51852D87348FFC4C`
        #[arg(long)]
        key_id: String,
        /// The ASCII-armored public key; `-` for stdin
        #[arg(long)]
        armor: String,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        source_url: Option<String>,
        #[arg(long)]
        trust_signature: Option<String>,
    },
    /// Delete a namespace's key (destructive: asks first) [admin]
    Delete { namespace: String, key_id: String },
}

pub async fn run_signing_key(cmd: SigningKeyCmd, client: &BatleHubClient) -> Result<()> {
    let registry = ctx().require_registry(None)?;
    match cmd {
        SigningKeyCmd::List { namespace } => {
            let v = client.signing_keys(&registry, &namespace).await?;
            show(&v, &v["keys"], &["key_id", "source", "source_url"])
        }
        SigningKeyCmd::Set {
            namespace,
            key_id,
            armor,
            source,
            source_url,
            trust_signature,
        } => {
            let ascii_armor = if armor == "-" {
                std::io::read_to_string(std::io::stdin())?
            } else {
                std::fs::read_to_string(&armor).with_context(|| format!("reading {armor}"))?
            };
            let key = json!({
                "key_id": key_id,
                "ascii_armor": ascii_armor,
                "source": source,
                "source_url": source_url,
                "trust_signature": trust_signature,
            });
            client.signing_key_set(&registry, &namespace, &key).await?;
            done(&format!("Set key {key_id} on {registry}/{namespace}"));
            Ok(())
        }
        SigningKeyCmd::Delete { namespace, key_id } => {
            confirm(
                "delete",
                &[format!("key {key_id} of {registry}/{namespace}")],
                None,
            )?;
            client
                .signing_key_delete(&registry, &namespace, &key_id)
                .await?;
            done(&format!("Deleted key {key_id} of {registry}/{namespace}"));
            Ok(())
        }
    }
}
