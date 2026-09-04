//! `batlehub why` and `batlehub wait` (RFC 0018 §4.2).
//!
//! `why` is the channel the native error bodies point at: a `403` on the
//! artifact path carries one line, and this prints the verdict behind it —
//! state, reason codes, when it lifts, which scanners answered, and the
//! findings for a caller with `findings:read`.
//!
//! `wait` is the CI contract. It polls the verdict endpoint and exits **0**
//! when the coordinate becomes servable, **1 when waiting cannot help** — a
//! `denied` verdict, or a hold with no clock such as `TIMESTAMP_MISSING` —
//! and **2** on timeout. Exit 1 is returned on the first poll with the
//! reason, never after burning the timeout, so a pipeline fails fast on a
//! decision and waits only on a clock:
//!
//! ```text
//! batlehub wait npm:left-pad@1.3.1 --timeout 2h && npm ci
//! ```

use std::time::Duration;

use anyhow::Result;
use clap::Args;

use crate::api::security::{parse_coordinate, VerdictView};
use crate::api::BatleHubClient;

#[derive(Args)]
pub struct WhyArgs {
    /// `<registry>:<name>@<version>` — the coordinate the refusal named
    pub coordinate: String,
    /// Queue a rescan after printing (needs `gates:exempt` on the registry)
    #[arg(long)]
    pub rescan: bool,
}

#[derive(Args)]
pub struct WaitArgs {
    /// `<registry>:<name>@<version>`
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

pub async fn run_why(args: WhyArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let (registry, name, version) = parse_coordinate(&args.coordinate)?;
    let Some(verdict) = client.get_verdict(&registry, &name, &version).await? else {
        anyhow::bail!(
            "no verdict for {}: either this instance has never been asked for it, the \
             registry has no [registries.security] profile, or your token lacks \
             quarantine:read",
            args.coordinate
        );
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&verdict)?);
    } else {
        print_verdict(&verdict);
    }
    if args.rescan {
        let r = client.rescan(&registry, &name, &version).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&r)?);
        } else if r.queued {
            println!("\nRescan queued (trigger: {}).", r.trigger);
        } else {
            println!("\nA scan job for this version is already open.");
        }
    }
    Ok(())
}

pub fn print_verdict(v: &VerdictView) {
    println!(
        "{}:{}@{}",
        v.package.registry, v.package.name, v.package.version
    );
    // RFC 0019: `batlehub why github:cli/cli@main` asked about a branch and
    // is being answered about a commit. Say so before anything else, or the
    // rest of the block reads as though it were about `main`.
    if let (Some(git_ref), Some(kind), Some(sha)) =
        (&v.requested_ref, &v.ref_kind, &v.resolved_commit)
    {
        println!("  ref          {git_ref} ({kind}) → {sha}");
    }
    println!("  state        {}", v.state);
    println!(
        "  reasons      {}",
        if v.reason_codes.is_empty() {
            "—".to_owned()
        } else {
            v.reason_codes.join(", ")
        }
    );
    if let Some(at) = &v.available_at {
        println!("  available    {at}");
    }
    println!("  policy       {}", v.policy_ref);
    println!("  evaluated    {}", v.evaluated_at);
    println!(
        "  scanned      {}",
        v.last_scanned_at.as_deref().unwrap_or("never")
    );
    println!(
        "  scanners     {}",
        if v.scanners_done.is_empty() {
            "none answered yet".to_owned()
        } else {
            v.scanners_done.join(", ")
        }
    );
    if v.findings_withheld {
        println!("  findings     withheld (needs findings:read)");
    } else if v.findings.is_empty() {
        println!("  findings     none");
    } else {
        println!("  findings");
        for f in &v.findings {
            let reference = f
                .reference
                .as_deref()
                .map(|r| format!(" [{r}]"))
                .unwrap_or_default();
            println!(
                "    {:<12} {:<10} {:<22} {}{}",
                f.scanner, f.severity, f.code, f.summary, reference
            );
        }
    }
    match v.state.as_str() {
        "allowed" | "warned" => println!("\nServed."),
        "quarantined" if v.available_at.is_some() => {
            println!("\nHeld until {}. `batlehub wait` waits for it.", v.available_at.as_deref().unwrap_or(""))
        }
        "quarantined" => println!("\nHeld until a scanner answers (or, for TIMESTAMP_MISSING, until the upstream is dated)."),
        _ => println!("\nRefused until a rescan or an administrator acts. `batlehub why --rescan` queues one."),
    }
}

/// Exit code of `wait`: 0 servable, 1 waiting cannot help, 2 timed out.
pub async fn run_wait(args: WaitArgs, client: &BatleHubClient, json: bool) -> Result<i32> {
    let (registry, name, version) = parse_coordinate(&args.coordinate)?;
    let started = std::time::Instant::now();
    let mut polls = 0u32;
    loop {
        polls += 1;
        let verdict = client.get_verdict(&registry, &name, &version).await?;
        let Some(verdict) = verdict else {
            eprintln!(
                "no verdict for {}: this instance has never been asked for it (request the \
                 artifact once, or check the registry's security profile and your \
                 quarantine:read)",
                args.coordinate
            );
            return Ok(1);
        };
        if verdict.is_served() {
            if json {
                println!("{}", serde_json::to_string_pretty(&verdict)?);
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
            return Ok(0);
        }
        if !verdict.waiting_helps() {
            eprintln!(
                "{}: waiting cannot help — {}. Run `batlehub why {}`.",
                args.coordinate,
                verdict.why_waiting_cannot_help(),
                args.coordinate
            );
            return Ok(1);
        }
        let elapsed = started.elapsed();
        if elapsed >= args.timeout {
            eprintln!(
                "{}: still {} ({}) after {}s; giving up",
                args.coordinate,
                verdict.state,
                verdict.reason_codes.join(", "),
                elapsed.as_secs()
            );
            return Ok(2);
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
