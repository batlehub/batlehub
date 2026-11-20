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
use crate::render::{ctx, Format};

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
    /// Only verdicts carrying this reason code, e.g. `SOC_VERDICT`
    #[arg(long)]
    pub reason: Option<String>,
    /// Every protected registry you may read, not the configured one
    #[arg(long)]
    pub all_registries: bool,
    /// The page to print, 1-based (default 1)
    #[arg(long)]
    pub page: Option<u64>,
    /// Verdicts per page (default 100, at most 1000)
    #[arg(long, alias = "per-page")]
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
}

fn csv_wanted() -> bool {
    ctx().format == Format::Csv
}

/// `verdict list`: the registry's stored verdicts, newest scan first.
async fn list_verdicts(args: &ListArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let registry = if args.all_registries {
        None
    } else {
        Some(ctx().require_registry(None)?)
    };
    let report = client
        .list_verdicts(&crate::api::security::VerdictListParams {
            registry: registry.as_deref(),
            state: args.state.as_deref(),
            reason: args.reason.as_deref(),
            page: args.page,
            per_page: args.limit,
        })
        .await?;
    if json {
        crate::render::data(&report)?;
        return Ok(());
    }
    let items = report["items"].as_array().cloned().unwrap_or_default();
    let total = report["total"].as_u64().unwrap_or(items.len() as u64);
    let page = report["page"].as_u64().unwrap_or(1);
    let per_page = report["per_page"]
        .as_u64()
        .unwrap_or(items.len() as u64)
        .max(1);
    let scope = registry.as_deref().unwrap_or("every registry");
    let pages = total.div_ceil(per_page).max(1);
    if pages > 1 {
        println!("{total} verdict(s) in {scope} — page {page} of {pages}");
    } else {
        println!("{total} verdict(s) in {scope}");
    }
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
        .pullers(&registry, &name, &version, &args.since, csv_wanted())
        .await?;
    if json {
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
            csv_wanted(),
        )
        .await?;
    // The server rendered it; printing it verbatim is what keeps the CSV an
    // auditor keeps identical to the one the API serves.
    if json {
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

// ── `security status` (RFC 0050 §4.5) ────────────────────────────────────────

#[derive(clap::Subcommand, Debug)]
pub enum SecurityCommand {
    /// One line per security domain — holds, flags, scanners, coverage; exit 1
    /// on any ATTENTION, 7 when a line is UNKNOWN or not readable by this token
    Status,
}

pub async fn run_security(cmd: SecurityCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        SecurityCommand::Status => security_status(client, json).await,
    }
}

/// The four lines in their order: label, body key, console page.
const STATUS_LINES: [(&str, &str, &str); 4] = [
    ("HOLDS", "holds", "/security/holds"),
    ("FLAGS", "flags", "/security/flags"),
    ("SCANNERS", "scanners", "/security/scanners"),
    ("COVERAGE", "coverage", "/security/coverage"),
];

fn n(block: &serde_json::Value, key: &str) -> u64 {
    block[key].as_u64().unwrap_or(0)
}

/// The sentence of one line, from its block.
fn status_sentence(key: &str, b: &serde_json::Value) -> String {
    if b["state"] == "unknown" {
        return "could not be read".to_owned();
    }
    match key {
        "holds" if n(b, "held") == 0 => "nothing held".to_owned(),
        "holds" => format!(
            "{} versions held, {} for malware",
            n(b, "held"),
            n(b, "malware")
        ),
        "flags" if n(b, "blocking") == 0 => "no flag blocks a download".to_owned(),
        "flags" => {
            let mut s = format!("{} flags block downloads", n(b, "blocking"));
            if n(b, "pulled_after_flag") > 0 {
                s.push_str(&format!(
                    "; {} downloaded after they were flagged",
                    n(b, "pulled_after_flag")
                ));
            }
            s
        }
        // The cause first, in the UI's order (`SecurityStatusStrip.vue`).
        "scanners" => {
            let plural = |n: u64, one: &str, many: &str| if n == 1 { one } else { many }.to_owned();
            let uncovered: Vec<&str> = b["uncovered"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if !uncovered.is_empty() {
                return format!(
                    "no live worker covers {}: new versions stay held",
                    uncovered.join(", ")
                );
            }
            if b["breaker_open"] == true {
                return "OSV is not answering: scans wait for it".to_owned();
            }
            let never = n(b, "never_ran");
            if never > 0 {
                return format!("{never} {} never ran", plural(never, "scanner", "scanners"));
            }
            let failing = n(b, "scanners_attention");
            if failing > 0 {
                return format!(
                    "{failing} {} since {} last success",
                    plural(failing, "scanner failing", "scanners failing"),
                    plural(failing, "its", "their")
                );
            }
            let deferred = n(b, "deferred_over_day");
            if deferred > 0 {
                return format!(
                    "{deferred} {} for more than a day",
                    plural(deferred, "job deferred", "jobs deferred")
                );
            }
            let live = n(b, "live_workers");
            format!(
                "{live} {} running, {} queued, {} failed",
                plural(live, "worker", "workers"),
                n(b, "queued"),
                n(b, "failed")
            )
        }
        _ => {
            let unprotected = b["unprotected"].as_array().map_or(0, Vec::len) as u64;
            if n(b, "protected") == 0 && unprotected == n(b, "registries") {
                "no registry is protected: nothing is scanned".to_owned()
            } else if unprotected == 0 {
                // `upstream`: the registries that serve one, the only ones a
                // profile can protect.
                format!("all {} registries are protected", n(b, "upstream"))
            } else {
                format!(
                    "{unprotected} of {} registries not protected",
                    n(b, "registries")
                )
            }
        }
    }
}

/// The exit code of an overview (decision 30): ATTENTION wins (`1`), then
/// an UNKNOWN or a block not shown (`7`), else `0`.
pub fn status_exit(body: &serde_json::Value) -> Option<ExitCode> {
    let states: Vec<Option<&str>> = STATUS_LINES
        .iter()
        .map(|(_, key, _)| body.get(*key).and_then(|b| b["state"].as_str()))
        .collect();
    if states.contains(&Some("attention")) {
        Some(ExitCode::Failure)
    } else if states.iter().any(|s| s.is_none() || *s == Some("unknown")) {
        Some(ExitCode::Partial)
    } else {
        None
    }
}

async fn security_status(client: &BatleHubClient, json: bool) -> Result<()> {
    let body = client.security_overview().await?;
    if json {
        crate::render::data(&body)?;
    } else {
        let mut hidden = Vec::new();
        for (label, key, page) in STATUS_LINES {
            let Some(block) = body.get(key) else {
                hidden.push(label);
                continue;
            };
            let state = block["state"].as_str().unwrap_or("unknown").to_uppercase();
            println!(
                "{label:<10} {state:<10} {:<40} {page}",
                status_sentence(key, block)
            );
        }
        if !hidden.is_empty() {
            println!(
                "not shown: {} (this token cannot read them)",
                hidden.join(", ")
            );
        }
    }
    match status_exit(&body) {
        Some(code) if json => Err(CliError::reported(code).into()),
        Some(code) => Err(CliError::quiet(code).into()),
        None => Ok(()),
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

    /// Decision 30 (T11): ATTENTION wins; an UNKNOWN or a block not shown is
    /// 7, never 0.
    #[test]
    fn status_exit_codes() {
        let line = |s: &str| serde_json::json!({ "state": s });
        let all = |s: &str| serde_json::json!({ "holds": line(s), "flags": line(s), "scanners": line(s), "coverage": line(s) });
        assert_eq!(status_exit(&all("ok")), None);
        assert_eq!(status_exit(&all("unknown")), Some(ExitCode::Partial));
        let mut body = all("ok");
        body["holds"] = line("attention");
        body["flags"] = line("unknown");
        assert_eq!(status_exit(&body), Some(ExitCode::Failure));
        let flags_only = serde_json::json!({ "flags": line("ok") });
        assert_eq!(status_exit(&flags_only), Some(ExitCode::Partial));
    }

    /// The scanners line names its cause, in the UI's order, and agrees in
    /// number; coverage counts the registries a profile can protect.
    #[test]
    fn status_sentences_name_the_cause() {
        let scanners = |v: serde_json::Value| status_sentence("scanners", &v);
        assert_eq!(
            scanners(serde_json::json!({ "live_workers": 3, "queued": 2, "failed": 0 })),
            "3 workers running, 2 queued, 0 failed"
        );
        assert_eq!(
            scanners(serde_json::json!({ "live_workers": 1 })),
            "1 worker running, 0 queued, 0 failed"
        );
        assert_eq!(
            scanners(serde_json::json!({ "never_ran": 1, "scanners_attention": 2 })),
            "1 scanner never ran"
        );
        assert_eq!(
            scanners(serde_json::json!({ "breaker_open": true, "never_ran": 1 })),
            "OSV is not answering: scans wait for it"
        );
        assert_eq!(
            scanners(serde_json::json!({ "scanners_attention": 2, "deferred_over_day": 1 })),
            "2 scanners failing since their last success"
        );
        assert_eq!(
            scanners(serde_json::json!({ "uncovered": ["npm"], "breaker_open": true })),
            "no live worker covers npm: new versions stay held"
        );
        assert_eq!(
            status_sentence(
                "coverage",
                &serde_json::json!({ "registries": 5, "upstream": 3, "protected": 3, "unprotected": [] })
            ),
            "all 3 registries are protected"
        );
    }
}
