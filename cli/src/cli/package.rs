use anyhow::Result;

use super::lifecycle::Verb;
use crate::coordinate::Coordinate;
use crate::exit::{CliError, ExitCode};
use clap::Subcommand;
use comfy_table::{Cell, Color, Table};

use crate::api::{
    package::{PackageQuery, PackageStatus, ReadmeQuery},
    BatleHubClient,
};

fn render_status_cell(status: &PackageStatus) -> Cell {
    match status {
        PackageStatus::Available => Cell::new("available").fg(Color::Green),
        PackageStatus::Blocked { reason } => Cell::new(format!("blocked: {reason}")).fg(Color::Red),
    }
}

#[derive(Subcommand)]
pub enum PackageCommand {
    /// List packages (across all or a specific registry)
    List {
        /// Filter by name substring
        #[arg(long, short = 's')]
        search: Option<String>,
        /// Only blocked versions (the server filters, so paging stays exact)
        #[arg(long)]
        blocked: bool,
        /// Page number (0-based)
        #[arg(long, default_value = "0")]
        page: u64,
        /// Results per page
        #[arg(long, default_value = "50")]
        per_page: u64,
    },
    /// The versions of a package: `registry/name`, or a bare name with `-r`
    Versions {
        /// `registry/name`
        coordinate: String,
        /// Rows per page (the server caps it at `[limits].versions_per_page`)
        #[arg(long)]
        limit: Option<u64>,
        /// The page to show, 0-based
        #[arg(long, default_value_t = 0)]
        page: u64,
        /// Follow the pages to the end, up to 10 000 rows
        #[arg(long)]
        all: bool,
        /// Only what this instance holds; do not ask the upstream
        #[arg(long)]
        no_upstream: bool,
    },
    /// One version: its state, where it comes from, and whether you can pull it
    Show {
        /// `registry/name[@version]`, or a bare name with `-r`. Without a version,
        /// the one the console would show: the newest stable version held here.
        coordinate: String,
    },
    /// Print a version's README
    ///
    /// The source, not a rendering: markdown in a terminal is readable, and
    /// turning it into ANSI is a separate concern (RFC 0007 §4.2).
    Readme {
        /// `registry/name[@version]`, or a bare `name[@version]` with `-r`. Without a version, the
        /// newest one that has a README answers.
        coordinate: String,
        /// Do not ask upstream about a version this instance holds nothing of.
        ///
        /// The console's package page asks by default; a script that wants only
        /// what is held here — or that runs where there is no route off site —
        /// asks for the local answer.
        #[arg(long)]
        no_upstream: bool,
        /// Print the console page that renders it — images and all — and fetch
        /// nothing.
        #[arg(long, conflicts_with = "no_upstream")]
        url: bool,
    },
    /// Yank versions: kept, but no longer resolved (destructive: asks first)
    Yank {
        /// `registry/name@version`, one or more
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Undo a yank
    Unyank {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Delete versions; each version number is then spent forever (destructive: asks first)
    Delete {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Pin versions so retention never reclaims them
    Pin {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Release a retention pin
    Unpin {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Mark versions deprecated
    Deprecate {
        #[arg(required = true)]
        coordinates: Vec<String>,
        /// Shown to the clients that read deprecation messages
        #[arg(long)]
        message: Option<String>,
    },
    /// Remove a deprecation
    Undeprecate {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Hide versions from listings and search, without deleting (destructive: asks first)
    Unlist {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Undo an unlist
    Relist {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
    /// Refuse versions to every client (destructive: asks first) [admin]
    Block {
        #[arg(required = true)]
        coordinates: Vec<String>,
        /// Recorded with the block; an admin reads it, a client sees the code
        #[arg(long)]
        reason: String,
    },
    /// Lift a block [admin]
    Unblock {
        #[arg(required = true)]
        coordinates: Vec<String>,
    },
}

pub async fn run(
    cmd: PackageCommand,
    client: &BatleHubClient,
    default_registry: Option<&str>,
    json: bool,
) -> Result<()> {
    let (verb, coordinates, message) = match cmd {
        PackageCommand::Yank { coordinates } => (Verb::Yank, coordinates, None),
        PackageCommand::Unyank { coordinates } => (Verb::Unyank, coordinates, None),
        PackageCommand::Delete { coordinates } => (Verb::Delete, coordinates, None),
        PackageCommand::Pin { coordinates } => (Verb::Pin, coordinates, None),
        PackageCommand::Unpin { coordinates } => (Verb::Unpin, coordinates, None),
        PackageCommand::Deprecate {
            coordinates,
            message,
        } => (Verb::Deprecate, coordinates, message),
        PackageCommand::Undeprecate { coordinates } => (Verb::Undeprecate, coordinates, None),
        PackageCommand::Unlist { coordinates } => (Verb::Unlist, coordinates, None),
        PackageCommand::Relist { coordinates } => (Verb::Relist, coordinates, None),
        PackageCommand::Block {
            coordinates,
            reason,
        } => (Verb::Block, coordinates, Some(reason)),
        PackageCommand::Unblock { coordinates } => (Verb::Unblock, coordinates, None),
        other => return consult(other, client, default_registry, json).await,
    };
    super::lifecycle::run(verb, &coordinates, message.as_deref(), client).await
}

async fn consult(
    cmd: PackageCommand,
    client: &BatleHubClient,
    default_registry: Option<&str>,
    json: bool,
) -> Result<()> {
    match cmd {
        PackageCommand::List {
            search,
            blocked,
            page,
            per_page,
        } => {
            let reg = default_registry.map(str::to_string);
            let resp = client
                .list_packages(PackageQuery {
                    registry: reg,
                    name: search,
                    blocked,
                    page,
                    per_page,
                })
                .await?;
            let items = resp.items;

            if json {
                crate::render::data(&items)?;
            } else {
                print_packages_table(&items, resp.total);
            }
        }
        PackageCommand::Readme {
            coordinate,
            no_upstream,
            url,
        } => {
            let c = Coordinate::resolve(client, &coordinate, default_registry).await?;
            if url {
                let url = console_url(&client.base_url, &c);
                if json {
                    crate::render::data(&serde_json::json!({ "url": url }))?;
                } else {
                    println!("{url}");
                }
                return Ok(());
            }
            let (registry, name, version) = (c.registry, c.name, c.version);
            let resp = client
                .package_readme(
                    &registry,
                    &name,
                    ReadmeQuery {
                        version,
                        format: "source",
                        upstream: no_upstream.then_some("skip"),
                    },
                )
                .await?;

            if json {
                crate::render::data(&resp)?;
            } else {
                print_readme(&resp);
            }
        }
        PackageCommand::Versions {
            coordinate,
            limit,
            page,
            all,
            no_upstream,
        } => {
            let c = Coordinate::resolve(client, &coordinate, default_registry).await?;
            let (registry, name) = (c.registry, c.name);
            let mut rows: Vec<serde_json::Value> = Vec::new();
            let mut page = page;
            let mut next = None;
            loop {
                let resp = client
                    .package_versions_page(&registry, &name, page, limit, !no_upstream)
                    .await?;
                rows.extend(resp["versions"].as_array().cloned().unwrap_or_default());
                let p = &resp["versions_page"];
                let (per_page, total) = (
                    p["per_page"].as_u64().unwrap_or(0),
                    p["total"].as_u64().unwrap_or(0),
                );
                let more = per_page > 0 && (page + 1) * per_page < total;
                if !more {
                    break;
                }
                if !all {
                    next = Some(page + 1);
                    break;
                }
                if rows.len() >= MAX_ROWS {
                    crate::render::warn(&format!(
                        "--all stopped at {MAX_ROWS} rows; continue with --page {}",
                        page + 1
                    ));
                    break;
                }
                page += 1;
            }
            if json {
                crate::render::data(&rows)?;
            } else {
                print_version_rows(&rows);
            }
            // On stderr, so `jq '.[].version'` keeps working (§4.3).
            if let Some(n) = next {
                eprintln!("next: --page {n}");
            }
        }
        PackageCommand::Show { coordinate } => {
            let c = Coordinate::resolve(client, &coordinate, default_registry).await?;
            return show(client, &c.registry, &c.name, c.version.as_deref(), json).await;
        }
        // The lifecycle verbs are dispatched by `run` before this.
        _ => anyhow::bail!("not a consult command"),
    }
    Ok(())
}

/// `package show`: the explore row of one version, then `why`'s answer for it,
/// so the state column and the reason behind it come from the server (§4.8).
async fn show(
    client: &BatleHubClient,
    registry: &str,
    name: &str,
    version: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut detail = client.package_detail(registry, name, version).await?;
    // No version asked: the server names its default, then answers for it.
    if version.is_none() {
        if let Some(v) = detail["default_version"].as_str().map(str::to_owned) {
            detail = client.package_detail(registry, name, Some(&v)).await?;
        }
    }
    let row = detail["selected"].clone();
    // `selected` falls back to the default for an unknown `version=`; that is
    // not the version asked for.
    let found = row["version"]
        .as_str()
        .filter(|v| version.is_none_or(|asked| asked == *v));
    let Some(v) = found.map(str::to_owned) else {
        let what = version.map_or_else(
            || format!("{registry}/{name} has no versions"),
            |v| format!("{registry}/{name}@{v} not found"),
        );
        return Err(CliError::new(ExitCode::NotFound, what).into());
    };
    let answer = client.pullability(registry, name, Some(&v)).await?;
    if json {
        return crate::render::data(&serde_json::json!({
            "registry": registry,
            "name": name,
            "version": row,
            "links": detail["links"],
            "pullability": answer,
        }));
    }
    print!("{}", show_text(&row, &detail["links"]));
    println!();
    print!("{}", super::why::render(registry, name, &answer));
    Ok(())
}

/// The facts block of `package show`; `—` for what the server does not know.
fn show_text(row: &serde_json::Value, links: &serde_json::Value) -> String {
    let cell = |v: &serde_json::Value| v.as_str().unwrap_or("—").to_owned();
    let mut out = String::new();
    for (label, value) in [
        ("version", cell(&row["version"])),
        ("state", cell(&row["state"])),
        ("source", cell(&row["source"])),
        ("published", cell(&row["published_at"])),
        ("license", cell(&row["license"])),
        ("repository", cell(&links["repository"])),
        ("homepage", cell(&links["homepage"])),
    ] {
        out.push_str(&format!("{label:<11} {value}\n"));
    }
    if row["deprecated"].as_bool() == Some(true) {
        out.push_str(&format!(
            "{:<11} {}\n",
            "deprecated",
            row["deprecation_message"].as_str().unwrap_or("yes")
        ));
    }
    if row["unlisted"].as_bool() == Some(true) {
        out.push_str("unlisted    yes — installable by exact version only\n");
    }
    out
}

fn print_packages_table(items: &[crate::api::package::PackageSummary], total: usize) {
    let mut table = Table::new();
    table.set_header(["Registry", "Name", "Version", "Status", "Accesses"]);
    for p in items {
        table.add_row(vec![
            Cell::new(&p.registry),
            Cell::new(&p.name),
            Cell::new(&p.version),
            render_status_cell(&p.status),
            Cell::new(p.access_count),
        ]);
    }
    println!("{table}");
    println!("{} / {} package(s)", items.len(), total);
}

/// `--all`'s cap (RFC 0040 §4.3).
const MAX_ROWS: usize = 10_000;

/// VERSION · STATE · SOURCE · PUBLISHED: the state is the server's own
/// (`allowed`, `upstream-only`, `yanked`, …), never derived here.
fn print_version_rows(rows: &[serde_json::Value]) {
    if rows.is_empty() {
        eprintln!("no versions");
        return;
    }
    let mut table = Table::new();
    table.set_header(["VERSION", "STATE", "SOURCE", "PUBLISHED"]);
    let cell = |v: &serde_json::Value| v.as_str().unwrap_or("—").to_owned();
    for r in rows {
        table.add_row(vec![
            cell(&r["version"]),
            cell(&r["state"]),
            cell(&r["source"]),
            cell(&r["published_at"]),
        ]);
    }
    println!("{table}");
}

/// The console's package page, which renders the README this command prints
/// as source. The console is served on the API's own origin (`spa.rs`).
fn console_url(base_url: &str, c: &Coordinate) -> String {
    use crate::api::auth::percent_encode;
    let mut url = format!(
        "{base_url}/packages/{}/{}",
        percent_encode(&c.registry),
        percent_encode(&c.name)
    );
    if let Some(v) = &c.version {
        url.push_str(&format!("?version={}", percent_encode(v)));
    }
    url
}

/// The README, with everything that qualifies it said *before* the text.
///
/// On stderr rather than stdout, so `batlehub package readme x/y > README.md`
/// writes the document and not a header — while a reader at a terminal still
/// sees that they are looking at another version's prose, or at a document this
/// instance does not hold.
fn print_readme(resp: &crate::api::package::ReadmeResponse) {
    if resp.is_fallback {
        if let Some(requested) = &resp.requested_version {
            eprintln!(
                "note: showing {}'s README; version {requested} ships none",
                resp.version
            );
        }
    }
    if resp.package_level {
        eprintln!("note: this is the package's README, not this version's");
    }
    if !resp.stored {
        eprintln!(
            "note: read from the upstream's own answer ({}); nothing of this version is held here",
            resp.freshness.as_deref().unwrap_or("unknown freshness")
        );
    }
    if resp.truncated {
        eprintln!("note: truncated — this registry stores only the beginning of it");
    }
    if resp.format != "markdown" {
        eprintln!("note: this README is {}, not markdown", resp.format);
    }
    // `source_text` is absent only for `format=html`, which this never asks for.
    println!("{}", resp.source_text.as_deref().unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn console_url_encodes_a_scoped_name_and_the_version() {
        let c = |name: &str, version: Option<&str>| Coordinate {
            registry: "npm-local".into(),
            name: name.into(),
            version: version.map(str::to_string),
        };
        assert_eq!(
            console_url("https://hub.example", &c("@scope/pkg", Some("1.0.0+b"))),
            "https://hub.example/packages/npm-local/%40scope%2Fpkg?version=1.0.0%2Bb"
        );
        assert_eq!(
            console_url("https://hub.example", &c("lodash", None)),
            "https://hub.example/packages/npm-local/lodash"
        );
    }

    #[test]
    fn show_prints_the_servers_state_and_dashes_for_the_unknown() {
        let text = show_text(
            &json!({"version": "1.4.0", "state": "pending", "source": "proxied",
                    "unlisted": true}),
            &json!(null),
        );
        assert!(text.contains("state       pending\n"), "{text}");
        assert!(text.contains("license     —\n"), "{text}");
        assert!(text.contains("unlisted    yes"), "{text}");
        assert!(!text.contains("deprecated"), "{text}");
    }
}
