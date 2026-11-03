//! `batlehub-cli why` and `batlehub-cli wait` (RFC 0018 §4.2).
//!
//! `why` is the channel the native error bodies point at: a `403` on the
//! artifact path carries one line, and this prints the verdict behind it —
//! state, reason codes, when it lifts, which scanners answered, and the
//! findings for a caller with `findings:read`.
//!
//! `wait` is the CI contract. It polls the verdict endpoint and exits **0**
//! when the coordinate becomes servable, **4 when waiting cannot help** — a
//! `denied` verdict, or a hold with no clock such as `TIMESTAMP_MISSING` —
//! and **8** on timeout (RFC 0040 §4.6). Exit 4 is returned on the first poll
//! with the reason, never after burning the timeout, so a pipeline fails fast
//! on a decision and waits only on a clock:
//!
//! ```text
//! batlehub-cli wait npm/left-pad@1.3.1 --timeout 2h && npm ci
//! ```

use std::time::Duration;

use anyhow::Result;
use clap::Args;

use crate::api::BatleHubClient;
use crate::coordinate::Coordinate;
use crate::exit::{CliError, ExitCode};
use crate::render::{ctx, deprecated, Format};

/// `registry/name@version` (or a bare `name@version` with `-r`), resolved
/// against the registries the caller can see.
async fn versioned(client: &BatleHubClient, raw: &str) -> Result<(String, String, String)> {
    Ok(Coordinate::resolve(client, raw, ctx().registry.as_deref())
        .await?
        .require_version()?)
}

/// `batlehub verdicts …` (RFC 0018 §4.2): the admin's side of the verdict.
#[derive(clap::Subcommand)]
pub enum VerdictsCommand {
    /// Who pulled a version inside a window — the incident question,
    /// from the same access-log query the flip alert carried
    Pullers(PullersArgs),
    /// The verdicts of a registry, by state (admin)
    List(ListArgs),
    /// Queue a low-priority scan of every cached version of a registry (admin)
    Backfill(BulkArgs),
    /// Queue a rescan of every verdict of a registry, or of one state (admin)
    Rescan(BulkArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// `allowed`, `warned`, `quarantined` or `denied`; default every state
    #[arg(long)]
    pub state: Option<String>,
    /// Per state (default 100, at most 1000)
    #[arg(long)]
    pub limit: Option<u64>,
}

#[derive(Args)]
pub struct BulkArgs {
    /// `rescan` only: the state to rescan; default every verdict
    #[arg(long)]
    pub state: Option<String>,
}

#[derive(Args)]
pub struct PullersArgs {
    /// `registry/name@version`
    pub coordinate: String,
    /// The window back from now (`30d`, `12h`, `90m`) or an RFC 3339 instant;
    /// default: the registry's `pullers_window_days`
    #[arg(long, default_value = "")]
    pub since: String,
    /// Deprecated: `--format csv`
    #[arg(long, hide = true)]
    pub csv: bool,
}

/// `--csv` is the old spelling of `--format csv`.
fn csv_wanted(flag: bool) -> bool {
    if flag {
        deprecated("--csv", "--format csv");
    }
    flag || ctx().format == Format::Csv
}

/// `verdict list`: the registry's stored verdicts, newest scan first.
async fn list_verdicts(args: &ListArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let registry = ctx().require_registry(None)?;
    let report = client
        .list_verdicts(&registry, args.state.as_deref(), args.limit)
        .await?;
    if json {
        crate::render::data(&report)?;
        return Ok(());
    }
    let items = report["items"].as_array().cloned().unwrap_or_default();
    println!("{} verdict(s) in {registry}", items.len());
    if items.is_empty() {
        return Ok(());
    }
    println!(
        "{:<12} {:<40} {:<12} codes",
        "state", "coordinate", "scanned"
    );
    for v in items {
        print_verdict_row(&v);
    }
    Ok(())
}

/// One verdict row.
fn print_verdict_row(v: &serde_json::Value) {
    let p = &v["package"];
    let coordinate = format!(
        "{}@{}",
        p["name"].as_str().unwrap_or(""),
        p["version"].as_str().unwrap_or("")
    );
    let scanned = v["last_scanned_at"]
        .as_str()
        .map(|s| s.chars().take(10).collect::<String>())
        .unwrap_or_else(|| "never".into());
    let codes = v["reason_codes"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    println!(
        "{:<12} {:<40} {:<12} {}",
        v["state"].as_str().unwrap_or(""),
        coordinate,
        scanned,
        codes
    );
}

/// `verdict pullers`: who pulled a held version before it was held.
async fn list_pullers(args: &PullersArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let (registry, name, version) = versioned(client, &args.coordinate).await?;
    let body = client
        .pullers(
            &registry,
            &name,
            &version,
            &args.since,
            csv_wanted(args.csv),
        )
        .await?;
    if args.csv || json {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let report: serde_json::Value = serde_json::from_str(&body)?;
    let rows = report["pullers"].as_array().cloned().unwrap_or_default();
    println!(
        "{} pulled by {} identit{} since {}",
        args.coordinate,
        rows.len(),
        if rows.len() == 1 { "y" } else { "ies" },
        report["since"].as_str().unwrap_or("?")
    );
    if rows.is_empty() {
        return Ok(());
    }
    println!(
        "{:<40} {:<10} {:>6}  {:<25} {:<25}",
        "identity", "role", "pulls", "first", "last"
    );
    for r in rows {
        println!(
            "{:<40} {:<10} {:>6}  {:<25} {:<25}",
            r["identity"].as_str().unwrap_or(""),
            r["role"].as_str().unwrap_or(""),
            r["count"].as_u64().unwrap_or(0),
            r["first_pull"].as_str().unwrap_or(""),
            r["last_pull"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// `batlehub audit …` (RFC 0018 §4.2): the identity-scoped half.
///
/// A group of one, deliberately. §4.2 names `audit pulls`, and the audit log's
/// other readers are admin-shaped (`admin audit-log`, `admin export-audit-log`)
/// while this one is a report an auditor runs. Folding it under `admin` would
/// have hidden it among twenty-seven subcommands.
#[derive(clap::Subcommand)]
pub enum AuditCommand {
    /// What one identity pulled inside a window — the transpose of
    /// `verdict pullers`
    Pulls(PullsArgs),
}

#[derive(Args)]
pub struct PullsArgs {
    /// Whose pulls: a user id, or `ip:<addr>` for an anonymous caller
    #[arg(long)]
    pub identity: String,
    /// Narrow to one package name
    #[arg(long)]
    pub package: Option<String>,
    /// The window back from now (`30d`, `12h`, `90m`) or an RFC 3339 instant;
    /// default: 30 days
    #[arg(long, default_value = "")]
    pub since: String,
    /// Deprecated: `--format csv`
    #[arg(long, hide = true)]
    pub csv: bool,
}

pub async fn run_audit(cmd: AuditCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        AuditCommand::Pulls(args) => list_pulls(&args, client, json).await,
    }
}

/// `audit pulls`: what one identity received.
async fn list_pulls(args: &PullsArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let body = client
        .audit_pulls(
            &args.identity,
            ctx().registry.as_deref(),
            args.package.as_deref(),
            &args.since,
            csv_wanted(args.csv),
        )
        .await?;
    // The server rendered it; printing it verbatim is what keeps the CSV an
    // auditor keeps identical to the one the API serves.
    if args.csv || json {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let report: serde_json::Value = serde_json::from_str(&body)?;
    let rows = report["pulls"].as_array().cloned().unwrap_or_default();
    println!(
        "{} pulled {} coordinate{} since {}",
        args.identity,
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        report["since"].as_str().unwrap_or("?")
    );
    if rows.is_empty() {
        return Ok(());
    }
    println!(
        "{:<14} {:<28} {:<14} {:>6}  {:<25}",
        "registry", "package", "version", "pulls", "last"
    );
    for r in rows {
        println!(
            "{:<14} {:<28} {:<14} {:>6}  {:<25}",
            r["registry"].as_str().unwrap_or(""),
            r["package_name"].as_str().unwrap_or(""),
            r["version"].as_str().unwrap_or("-"),
            r["count"].as_u64().unwrap_or(0),
            r["last_pull"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

pub async fn run_verdicts(cmd: VerdictsCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        VerdictsCommand::List(args) => list_verdicts(&args, client, json).await,
        VerdictsCommand::Backfill(args) => bulk(client, "backfill", &args, json).await,
        VerdictsCommand::Rescan(args) => bulk(client, "rescan", &args, json).await,
        VerdictsCommand::Pullers(args) => list_pullers(&args, client, json).await,
    }
}

async fn bulk(client: &BatleHubClient, op: &str, args: &BulkArgs, json: bool) -> Result<()> {
    let registry = ctx().require_registry(None)?;
    let out = client
        .bulk_scan(op, &registry, args.state.as_deref())
        .await?;
    if json {
        crate::render::data(&out)?;
    } else {
        println!(
            "{}: {} of {} coordinate(s) queued at {} priority in {}",
            op,
            out["queued"].as_u64().unwrap_or(0),
            out["considered"].as_u64().unwrap_or(0),
            out["trigger"].as_str().unwrap_or("?"),
            registry
        );
    }
    Ok(())
}

#[derive(Args)]
pub struct WaitArgs {
    /// `registry/name@version`
    pub coordinate: String,
    /// Give up after this long (e.g. `30m`, `2h`; default 1h)
    #[arg(long, default_value = "1h", value_parser = parse_duration)]
    pub timeout: Duration,
    /// Poll interval when the hold names no clock (default 30s)
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    pub interval: Duration,
}

/// `90`, `90s`, `15m`, `2h`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("'{s}' is not a duration (90, 90s, 15m, 2h)"))?;
    let mult = match unit.trim() {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" => 3600,
        "d" => 86_400,
        other => return Err(format!("unknown duration unit '{other}' (s, m, h, d)")),
    };
    Ok(Duration::from_secs(n * mult))
}

/// `wait`: `Ok` when servable; exit 4 when waiting cannot help, 8 on timeout.
pub async fn run_wait(args: WaitArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let (registry, name, version) = versioned(client, &args.coordinate).await?;
    let started = std::time::Instant::now();
    let mut polls = 0u32;
    loop {
        polls += 1;
        let verdict = client.get_verdict(&registry, &name, &version).await?;
        let Some(verdict) = verdict else {
            return Err(CliError::new(
                ExitCode::Refused,
                format!(
                    "no verdict for {registry}/{name}@{version}: this instance has never been \
                     asked for it"
                ),
            )
            .hint(
                "request the artifact once, or check the registry's security profile and your \
                 quarantine:read",
            )
            .into());
        };
        if verdict.is_served() {
            if json {
                crate::render::data(&verdict)?;
            } else {
                println!(
                    "{} is {} ({})",
                    args.coordinate,
                    verdict.state,
                    if verdict.reason_codes.is_empty() {
                        "no findings".to_owned()
                    } else {
                        verdict.reason_codes.join(", ")
                    }
                );
            }
            return Ok(());
        }
        if !verdict.waiting_helps() {
            return Err(CliError::new(
                ExitCode::Refused,
                format!(
                    "{registry}/{name}@{version}: waiting cannot help — {}",
                    verdict.why_waiting_cannot_help()
                ),
            )
            .hint(format!("batlehub-cli why {registry}/{name}@{version}"))
            .into());
        }
        let elapsed = started.elapsed();
        if elapsed >= args.timeout {
            return Err(CliError::new(
                ExitCode::Timeout,
                format!(
                    "{registry}/{name}@{version}: still {} ({}) after {}s; giving up",
                    verdict.state,
                    verdict.reason_codes.join(", "),
                    elapsed.as_secs()
                ),
            )
            .into());
        }
        // Sleep to the clock when there is one, else the poll interval —
        // and never past the timeout.
        let until_clock = verdict
            .available_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|at| {
                (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
                    .to_std()
                    .unwrap_or_default()
            });
        let mut nap = until_clock
            .unwrap_or(args.interval)
            .max(Duration::from_secs(1));
        let remaining = args.timeout - elapsed;
        if nap > remaining {
            nap = remaining;
        }
        if polls == 1 {
            eprintln!(
                "{}: {} ({}); waiting up to {}s",
                args.coordinate,
                verdict.state,
                verdict.reason_codes.join(", "),
                args.timeout.as_secs()
            );
        }
        tokio::time::sleep(nap).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_with_or_without_a_unit() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert!(parse_duration("2w").is_err());
        assert!(parse_duration("h").is_err());
    }
}
