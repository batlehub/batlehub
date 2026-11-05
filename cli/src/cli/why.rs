//! `batlehub-cli why` (RFC 0040 §4.8): why a version is not pullable, or not
//! pullable *yet* — every cause the download checks, in the order it checks
//! them, from `GET /api/v1/packages/access`.

use anyhow::Result;
use batlehub_core::entities::reason_code_docs_url;
use clap::Args;
use serde_json::Value;

use crate::api::BatleHubClient;
use crate::coordinate::Coordinate;
use crate::exit::{CliError, ExitCode};
use crate::render::{ctx, data, warn};

#[derive(Args)]
pub struct WhyArgs {
    /// `registry/name@version`, or `registry/name` to explain which version a
    /// client asking for the newest receives
    #[arg(required_unless_present = "last")]
    pub coordinate: Option<String>,
    /// Your own recent refusals, newest first: coordinates to pass back to `why`
    #[arg(long, conflicts_with = "coordinate")]
    pub last: bool,
    /// How many refusals `--last` lists
    #[arg(long, default_value_t = 20)]
    pub limit: u64,
    /// Queue a rescan after printing (needs `gates:exempt` on the registry)
    #[arg(long)]
    pub rescan: bool,
}

pub async fn run(args: WhyArgs, client: &BatleHubClient) -> Result<()> {
    if args.last {
        return last(client, args.limit).await;
    }
    let raw = args.coordinate.unwrap_or_default();
    let c = Coordinate::resolve(client, &raw, ctx().registry.as_deref()).await?;
    let answer = client
        .pullability(&c.registry, &c.name, c.version.as_deref())
        .await?;
    if ctx().structured() {
        data(&answer)?;
    } else if let Some(version) = c.version.as_deref() {
        print!("{}", render(&c.registry, &c.name, &answer));
        // The scanners' own detail (RFC 0018): which advisory, which rule,
        // which file — shown to a token with `findings:read`, as before.
        if verdict_holds(&answer) {
            if let Ok(Some(v)) = client.get_verdict(&c.registry, &c.name, version).await {
                print!("{}", findings(&v));
            }
        }
    } else {
        print!("{}", render_resolution(&c.registry, &c.name, &answer));
    }
    if args.rescan {
        if let Some(version) = &c.version {
            let r = client.rescan(&c.registry, &c.name, version).await?;
            if !ctx().structured() {
                if r.queued {
                    println!("\nRescan queued (trigger: {}).", r.trigger);
                } else {
                    println!("\nA scan job for this version is already open.");
                }
            }
        }
    }
    // A question, so ⏳ is a successful answer; ⛔ is exit 4 (§4.6).
    if headline(&answer) == "refused" {
        return Err(CliError::reported(ExitCode::Refused).into());
    }
    Ok(())
}

/// `pullable`, `waiting` or `refused`; an older server, with no headline,
/// answers from `can_access`.
fn headline(answer: &Value) -> &str {
    match answer["headline"].as_str() {
        Some(h) => h,
        None if answer["can_access"].as_bool() == Some(true) => "pullable",
        None => "refused",
    }
}

fn icon(state: &str) -> &'static str {
    match state {
        "pass" | "pullable" => "✅",
        "warn" => "⚠️",
        "wait" | "waiting" => "⏳",
        _ => "⛔",
    }
}

/// The block §4.8 shows: headline, one line per cause, `next:`. Every icon
/// has its word beside it, so nothing is said by colour or glyph alone.
pub fn render(registry: &str, name: &str, answer: &Value) -> String {
    let version = answer["package"]["version"].as_str().unwrap_or("");
    let coord = format!("{registry}/{name}@{version}");
    let mut out = String::new();
    if answer["headline"].is_null() {
        warn("server predates RFC 0040: only the registry grant and an admin block are checked");
        let ok = answer["can_access"].as_bool() == Some(true);
        out.push_str(&format!(
            "{} {coord} — {}\n",
            if ok { "✅" } else { "⛔" },
            if ok {
                "pullable now".to_owned()
            } else {
                format!(
                    "not pullable: {}",
                    answer["reason"].as_str().unwrap_or("refused")
                )
            }
        ));
        return out;
    }
    let h = headline(answer);
    let tail = match h {
        "pullable" => "pullable now".to_owned(),
        "waiting" => match answer["until"].as_str() {
            Some(t) => format!("pullable from {}", pretty_time(t)),
            None => "pullable when a scanner answers".to_owned(),
        },
        _ => "not pullable".to_owned(),
    };
    out.push_str(&format!("{} {coord} — {tail}\n\n", icon(h)));
    let mut next = None;
    for cause in answer["causes"].as_array().into_iter().flatten() {
        let state = cause["state"].as_str().unwrap_or("");
        let mut line = cause["detail"].as_str().unwrap_or("").to_owned();
        if let Some(code) = cause["reason_code"].as_str() {
            line = format!("{code}: {line}");
        }
        if let Some(until) = cause["until"].as_str() {
            line.push_str(&format!(" (until {})", pretty_time(until)));
        }
        out.push_str(&format!(
            "  {} {:<5} {:<11} {line}\n",
            icon(state),
            state,
            cause["kind"].as_str().unwrap_or("")
        ));
        if let Some(code) = cause["reason_code"].as_str() {
            out.push_str(&format!("{:22}{}\n", "", reason_code_docs_url(code)));
        }
        if next.is_none() && state != "pass" && state != "warn" {
            next = cause["next"].as_str().map(str::to_owned);
        }
    }
    let next = match (h, next) {
        ("waiting", _) => Some(format!(
            "`batlehub-cli wait {coord}` returns when it becomes pullable"
        )),
        (_, n) => n,
    };
    if let Some(n) = next {
        out.push_str(&format!("\nnext: {n}\n"));
    }
    out
}

/// Whether a verdict cause holds or warns: then its findings are worth asking for.
fn verdict_holds(answer: &Value) -> bool {
    answer["causes"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["kind"] == "verdict" && c["state"] != "pass")
}

/// The findings block under the answer.
fn findings(v: &crate::api::security::VerdictView) -> String {
    if v.findings_withheld {
        return "\nfindings: withheld (needs findings:read)\n".to_owned();
    }
    if v.findings.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nfindings:\n");
    for f in &v.findings {
        let reference = f
            .reference
            .as_deref()
            .map(|r| format!(" [{r}]"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:<12} {:<10} {:<22} {}{}\n",
            f.scanner, f.severity, f.code, f.summary, reference
        ));
    }
    out
}

/// `why registry/name`: what a client asking for the newest gets, and why
/// the newer ones are not it.
fn render_resolution(registry: &str, name: &str, answer: &Value) -> String {
    let mut out = String::new();
    let versions = answer["versions"].as_array().cloned().unwrap_or_default();
    let newest = versions.first();
    match answer["resolved_version"].as_str() {
        Some(v) => {
            out.push_str(&format!("{registry}/{name} resolves to {v}"));
            if let Some(n) = newest.filter(|n| n["version"].as_str() != Some(v)) {
                let (_, code, until) = summary(n);
                out.push_str(&format!(
                    " — {} is {} ({code}{})",
                    n["version"].as_str().unwrap_or(""),
                    n["headline"].as_str().unwrap_or(""),
                    until.map(|u| format!(", until {u}")).unwrap_or_default()
                ));
            }
            out.push('\n');
        }
        None => out.push_str(&format!(
            "⛔ {registry}/{name}: none of its newest versions is pullable now\n"
        )),
    }
    let mut table = comfy_table::Table::new();
    table.set_header(["VERSION", "STATE", "UNTIL", "CODE"]);
    for v in &versions {
        let (state, code, until) = summary(v);
        table.add_row([
            v["version"].as_str().unwrap_or("").to_owned(),
            state,
            until.unwrap_or_else(|| "—".into()),
            code,
        ]);
    }
    out.push_str(&format!("\n{table}\n"));
    out
}

/// A version's state word, its first code and its clock.
fn summary(p: &Value) -> (String, String, Option<String>) {
    let worst = p["causes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| matches!(c["state"].as_str(), Some("refuse" | "wait" | "warn")));
    let state = match worst {
        Some(c) if c["kind"] == "lifecycle" => "yanked".to_owned(),
        Some(c) if c["kind"] == "verdict" => c["detail"]
            .as_str()
            .and_then(|d| d.split(' ').next())
            .unwrap_or("held")
            .to_owned(),
        Some(c) if c["kind"] == "block" => "blocked".to_owned(),
        Some(c) => c["state"]
            .as_str()
            .unwrap_or("")
            .replace("refuse", "denied"),
        None => "allowed".to_owned(),
    };
    let code = worst
        .and_then(|c| c["reason_code"].as_str())
        .unwrap_or("—")
        .to_owned();
    let until = p["until"].as_str().map(pretty_time);
    (state, code, until)
}

/// `2026-11-04T14:00:00Z` → `2026-11-04 14:00 UTC`.
fn pretty_time(t: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(t)
        .map(|d| {
            d.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| t.to_owned())
}

/// `why --last`: the caller's own refusals, each a coordinate to pass back.
async fn last(client: &BatleHubClient, limit: u64) -> Result<()> {
    let rows = client.my_refusals(limit).await?;
    if ctx().structured() {
        return data(&rows);
    }
    let rows = rows.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        eprintln!("no refusals in the last 30 days");
        return Ok(());
    }
    let mut table = comfy_table::Table::new();
    table.set_header(["COORDINATE", "WHEN", "CODE"]);
    for r in &rows {
        table.add_row([
            format!(
                "{}/{}@{}",
                r["registry"].as_str().unwrap_or(""),
                r["name"].as_str().unwrap_or(""),
                r["version"].as_str().unwrap_or("")
            ),
            r["downloaded_at"]
                .as_str()
                .map(pretty_time)
                .unwrap_or_default(),
            r["reason_code"].as_str().unwrap_or("—").to_owned(),
        ]);
    }
    println!("{table}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_held_version_reads_as_the_rfc_shows_it() {
        let answer = json!({
            "package": {"version": "1.4.0"},
            "headline": "waiting",
            "until": "2026-11-04T14:00:00Z",
            "causes": [
                {"kind": "access", "state": "pass", "detail": "you may read npm-local"},
                {"kind": "verdict", "state": "wait", "reason_code": "MIN_AGE_NOT_MET",
                 "detail": "quarantined — MIN_AGE_NOT_MET", "until": "2026-11-04T14:00:00Z",
                 "next": "wait"},
            ],
        });
        let text = render("npm-local", "left-pad", &answer);
        assert!(
            text.starts_with("⏳ npm-local/left-pad@1.4.0 — pullable from 2026-11-04 14:00 UTC"),
            "{text}"
        );
        assert!(text.contains("✅ pass  access"), "{text}");
        assert!(
            text.contains("⏳ wait  verdict     MIN_AGE_NOT_MET:"),
            "{text}"
        );
        assert!(text.contains("refused-downloads#min-age-not-met"), "{text}");
        assert!(
            text.contains("next: `batlehub-cli wait npm-local/left-pad@1.4.0`"),
            "{text}"
        );
    }

    #[test]
    fn the_headline_falls_back_on_an_older_server() {
        assert_eq!(headline(&json!({"can_access": true})), "pullable");
        assert_eq!(headline(&json!({"can_access": false})), "refused");
        assert_eq!(headline(&json!({"headline": "waiting"})), "waiting");
    }
}
