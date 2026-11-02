//! `batlehub authz` — why the resolver decided that.
//!
//! RFC 0015 §4.8. Every mechanism the RFC removes had its own way of being
//! invisible, and *"the only way to answer 'why was this refused?' was to read
//! Rust."* §4.8 puts this command beside the console page for a specific reason:
//! *"The same data is available to `batlehub authz explain` … so this is not a
//! reason to open a browser."*
//!
//! # Why the output leads with provenance
//!
//! A resolved set without provenance tells an operator *what* a subject holds.
//! Naming the tier **and the subject form** that produced each verb tells them
//! **which line to edit** — which §4.8 calls the difference between a debugging
//! tool and a diagnostic. So `granted_by` is a column rather than a detail
//! behind `--json`.

use anyhow::Result;
use clap::Subcommand;
use comfy_table::Table;

use crate::api::{
    authz::{ExplainQuery, ExplainResponse, ShadowResponse},
    BatleHubClient,
};

#[derive(Subcommand)]
pub enum AuthzCommand {
    /// Simulate whether an identity may access a version: `registry/name@version` [admin]
    Check(super::groups::AccessCheckArgs),
    /// Resolve what a subject may do, and say which tier granted each verb
    Explain {
        /// Registry name
        registry: String,
        /// Subject, in grant spelling: `*`, `role:user`, `group:oidc1:eng`,
        /// `group:*:eng`, `user:alice`
        #[arg(long)]
        subject: String,
        /// The verb being asked about, e.g. `releases:read`
        #[arg(long)]
        action: String,
        /// Package name. Namespace tiers only match when this is given.
        #[arg(long)]
        package: Option<String>,
        /// Version, for the version tier
        #[arg(long)]
        version: Option<String>,
    },
    /// What shadow mode has served that enforcement would have refused
    Shadow {
        /// How many individual entries to show
        #[arg(long)]
        limit: Option<usize>,
        /// Show every entry rather than the per-node summary
        #[arg(long)]
        detail: bool,
    },
}

pub async fn run(cmd: AuthzCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        AuthzCommand::Explain {
            registry,
            subject,
            action,
            package,
            version,
        } => {
            let answer = client
                .authz_explain(ExplainQuery {
                    registry: &registry,
                    subject: &subject,
                    action: &action,
                    package: package.as_deref(),
                    version: version.as_deref(),
                })
                .await?;
            if json {
                crate::render::data(&answer)?;
            } else {
                print_explain(&answer, &subject, &action);
            }
        }
        AuthzCommand::Check(args) => {
            return super::groups::run(super::groups::Group::AccessCheck(args), client, json).await
        }
        AuthzCommand::Shadow { limit, detail } => {
            let report = client.authz_shadow(limit).await?;
            if json {
                crate::render::data(&report)?;
            } else {
                print_shadow(&report, detail);
            }
        }
    }
    Ok(())
}

fn print_explain(answer: &ExplainResponse, subject: &str, action: &str) {
    let verdict = if answer.decision == "allow" {
        "ALLOW"
    } else {
        "DENY"
    };
    println!("{verdict}  {subject}  {action}");
    if let Some(reason) = &answer.reason {
        println!("       {reason}");
    }

    // §4.7, and the thing an operator must not miss: under a shadow the grants
    // refuse and the server serves anyway. Printed immediately under the verdict
    // because a `DENY` line read without it is the opposite of what happens.
    if let Some(shadow) = &answer.shadowed_by {
        println!();
        println!(
            "  ⚠ SHADOWED by {} until {} — this request is SERVED despite the denial above,",
            shadow.node, shadow.until
        );
        println!("    and the refusal is only recorded. `batlehub authz shadow` lists them.");
    }

    println!();
    if answer.resolved.is_empty() {
        println!("  no verbs resolved for this subject");
    } else {
        let mut table = Table::new();
        // `Granted by` before `Subject`: the tier is the line to edit, and the
        // subject form is which key on that line.
        table.set_header(["Verb", "Granted by", "Matched subject"]);
        for verb in &answer.resolved {
            table.add_row([&verb.action, &verb.granted_by, &verb.subject]);
        }
        println!("{table}");
    }

    println!();
    let a = &answer.attributes;
    let mut attrs = Table::new();
    attrs.set_header(["Attribute", "Value"]);
    attrs.add_row(["visibility", &a.visibility]);
    attrs.add_row(["prerelease_visibility", &a.prerelease_visibility]);
    attrs.add_row(["immutable", &a.immutable]);
    attrs.add_row(["monotonic", &a.monotonic.to_string()]);
    if a.versioning_dry_run {
        attrs.add_row(["versioning", "DRY RUN — evaluated, not enforced"]);
    }
    if !a.exempt_gates.is_empty() {
        attrs.add_row(["exempt gates", &a.exempt_gates.join(", ")]);
    }
    println!("{attrs}");

    println!();
    println!("Tiers walked: {}", answer.tiers_walked.join(" → "));

    // The same discipline the `access-check` simulator carries: a bare verdict
    // is ambiguous between "nothing denies this" and "nothing I looked at denies
    // this". Printed last and always, including on an `ALLOW`, because that is
    // the answer it most changes the meaning of.
    println!();
    println!("Not covered by this answer:");
    for layer in &answer.not_covered {
        println!("  · {layer}");
    }
}

fn print_shadow(report: &ShadowResponse, detail: bool) {
    if report.no_shadow_configured {
        // "Quiet" and "absent" look identical in an empty list and mean opposite
        // things: the first says enforcing is safe, the second says nothing was
        // measured. An operator about to flip a migration reads this first.
        println!("No node is in shadow. Nothing is being served that grants would refuse.");
        println!("(An empty list below would otherwise read as 'the shadow found nothing'.)");
        return;
    }

    if report.by_node.is_empty() {
        println!("Shadow is configured and has served nothing yet.");
        return;
    }

    let mut table = Table::new();
    table.set_header(["Node", "Until", "Served", "Missing verbs", "Subjects"]);
    for s in &report.by_node {
        table.add_row([
            s.node.clone(),
            s.shadow_until.clone(),
            s.count.to_string(),
            s.actions.join(", "),
            s.subjects.join(", "),
        ]);
    }
    println!("{table}");
    println!("\nEach row is a request that WAS SERVED and would be refused if you enforced today.");

    if detail {
        println!();
        let mut rows = Table::new();
        rows.set_header(["When", "Coordinate", "Verb", "Subject", "Node"]);
        for d in &report.recent {
            rows.add_row([
                d.at.clone(),
                format!("{}/{}@{}", d.registry, d.package, d.version),
                d.action.clone(),
                d.subject.clone(),
                d.node.clone(),
            ]);
        }
        println!("{rows}");
        if report.recent.len() >= report.kept {
            // A bounded buffer that looked exhaustive would understate what a
            // shadow is serving, which is the wrong direction for this report.
            println!(
                "\n⚠ Showing {} entries, which is the buffer's limit — older would-have-beens \
                 have been dropped. The per-node counts above are also bounded by it.",
                report.kept
            );
        }
    }
}
