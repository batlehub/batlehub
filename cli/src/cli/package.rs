use anyhow::Result;
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
    /// Show all versions of a package
    Versions {
        /// Registry name
        registry: String,
        /// Package name
        name: String,
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
}

pub async fn run(
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
            let c = crate::coordinate::Coordinate::resolve(client, &coordinate, default_registry)
                .await?;
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
        PackageCommand::Versions { registry, name } => {
            let resp = client
                .list_packages(PackageQuery {
                    registry: Some(registry.clone()),
                    name: Some(name.clone()),
                    page: 0,
                    per_page: 200,
                })
                .await?;

            let items: Vec<_> = resp
                .items
                .into_iter()
                .filter(|p| p.name == name && p.registry == registry)
                .collect();

            if json {
                crate::render::data(&items)?;
            } else {
                print_versions_table(&items);
            }
        }
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

fn print_versions_table(items: &[crate::api::package::PackageSummary]) {
    let mut table = Table::new();
    table.set_header(["Version", "Status", "Accesses"]);
    for p in items {
        table.add_row(vec![
            Cell::new(&p.version),
            render_status_cell(&p.status),
            Cell::new(p.access_count),
        ]);
    }
    println!("{table}");
    println!("{} version(s)", items.len());
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
