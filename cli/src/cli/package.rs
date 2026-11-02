use anyhow::Result;

use super::lifecycle::Verb;
use crate::coordinate::Coordinate;
use crate::render::deprecated;
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
        /// Show only blocked packages
        #[arg(long)]
        blocked_only: bool,
        /// Page number (0-based)
        #[arg(long, default_value = "0")]
        page: u64,
        /// Results per page
        #[arg(long, default_value = "50")]
        per_page: u64,
    },
    /// The versions of a package: `registry/name`, or a bare name with `-r`
    Versions {
        /// `registry/name` (the older `<registry> <name>` still works, with a warning)
        #[arg(num_args = 1..=2, required = true)]
        coordinate: Vec<String>,
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
            blocked_only,
            page,
            per_page,
        } => {
            let reg = default_registry.map(str::to_string);
            let resp = client
                .list_packages(PackageQuery {
                    registry: reg,
                    name: search,
                    page,
                    per_page,
                })
                .await?;

            let items: Vec<_> = if blocked_only {
                resp.items
                    .into_iter()
                    .filter(|p| matches!(p.status, PackageStatus::Blocked { .. }))
                    .collect()
            } else {
                resp.items
            };

            if json {
                crate::render::data(&items)?;
            } else {
                print_packages_table(&items, resp.total);
            }
        }
        PackageCommand::Readme {
            coordinate,
            no_upstream,
        } => {
            let c = Coordinate::resolve(client, &coordinate, default_registry).await?;
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
            let raw = match coordinate.as_slice() {
                [registry, name] => {
                    deprecated(
                        "package versions <registry> <name>",
                        "package versions <registry>/<name>",
                    );
                    format!("{registry}/{name}")
                }
                [one] => one.clone(),
                _ => anyhow::bail!("expected registry/name"),
            };
            let c = Coordinate::resolve(client, &raw, default_registry).await?;
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
        // The lifecycle verbs are dispatched by `run` before this.
        _ => anyhow::bail!("not a consult command"),
    }
    Ok(())
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
