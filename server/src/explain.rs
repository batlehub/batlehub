//! `batlehub explain-config` — print what a config file actually grants.
//!
//! # Why this exists
//!
//! RFC 0015 §4.2 moves wildcard expansion from evaluation time to config load:
//! `releases:*` and `*` become an explicit set of verbs once, when the file is
//! read, rather than being re-derived on every request. That is a real property
//! — it is what makes "what does this config grant?" a question with an answer —
//! and the RFC is explicit that half of it is being able to *see* the result:
//!
//! > Making it visible takes a new task — `task config:explain`, which dumps the
//! > expanded grants for a config file and does not exist today; it lands with
//! > the vocabulary in phase 1, because an expansion nobody can print is only
//! > half of the property this paragraph claims.
//!
//! # What it is not
//!
//! Not `GET /api/v1/admin/authz/explain` (RFC 0015 §4.8). That answers "may this
//! subject do this thing to this resource, and which tier granted it" against a
//! running server with grants, and lands in phase 3. This one reads a file, has
//! no hierarchy to walk and no request to judge, and answers the narrower
//! question the config file alone can answer: after expansion, which verbs does
//! each role and each group hold on each registry?
//!
//! The two will not disagree, because they will not overlap: this prints the
//! registry tier, which is the only tier a config file can express.

use std::collections::BTreeSet;

use anyhow::{Context, Result};

use batlehub_config::load;
use batlehub_core::entities::{expand_patterns, RegistryKind, WildcardScope};

/// Read `path` and print the expanded permission set per registry.
pub(crate) fn explain_config(path: &str) -> Result<()> {
    let cfg = load(path).with_context(|| format!("loading {path}"))?;

    println!("# {path}");
    println!(
        "#\n# Permissions after expansion. A `\"*\"` in [registries.rbac] expands to the\n\
         # four read verbs it has always meant (RFC 0015 §10 rule 3), not to every verb\n\
         # in the vocabulary — an administrator's write access comes from the role check,\n\
         # not from that string.\n"
    );

    // RFC 0015 §4.1's instance tier, above every registry. Printed first because
    // that is where it sits on every resolution path, and because §4.2's
    // printability property applies to it as much as to a registry's block: *"an
    // expansion nobody can print is only half of the property this paragraph
    // claims."* A reader who cannot see this node cannot tell a verb granted
    // server-wide from one granted nowhere.
    println!("instance (every registry)");
    match crate::grants::build_instance_grants(&cfg)? {
        Some(_) => {
            // The resolved node, which is the translation *plus* the block —
            // rule 5's control verbs are in no config file, so printing only what
            // was written would understate what an administrator holds.
            print_node(&batlehub_core::services::authz::translate::instance_node(
                crate::grants::build_instance_grants(&cfg)?.as_ref(),
            ));
        }
        None => {
            println!("  (no [grants] block — the control verbs below come from §10 rule 5)");
            print_node(&batlehub_core::services::authz::translate::instance_node(
                None,
            ));
        }
    }
    println!();

    for reg in &cfg.registries {
        let kind: Option<RegistryKind> = reg.registry_type.parse().ok();
        println!("registry {} ({})", reg.name, reg.registry_type);

        let rows: Vec<(String, &Vec<String>)> = vec![
            ("anonymous".to_owned(), &reg.rbac.anonymous),
            ("user".to_owned(), &reg.rbac.user),
            ("admin".to_owned(), &reg.rbac.admin),
        ];

        for (subject, patterns) in rows {
            print_row(&subject, patterns, kind)?;
        }
        // BTreeSet so the output is stable across runs and can be diffed; a
        // HashMap's iteration order would make every dump a different file.
        for group in reg.rbac.groups.keys().collect::<BTreeSet<_>>() {
            let patterns = &reg.rbac.groups[group];
            print_row(&format!("group:{group}"), patterns, kind)?;
        }

        // ── the resolved registry node, and any namespaces ───────────────────
        //
        // The rows above are what the operator wrote; this is what the server
        // will actually resolve, after the §10 translation, rule 2's
        // conjunction, and any `[registries.grants]` block. They are printed
        // together because the gap between them is the interesting part — rule 5
        // writes out write verbs that appear in no config file, and rule 2
        // withholds a `catalogue:browse` whose flag is set.
        match crate::grants::build_registry_grants(reg) {
            Ok(built) => {
                println!("  ── resolved ──");
                print_node(&built.registry);
                for (prefix, node) in &built.namespaces {
                    println!("  namespace \"{prefix}\"");
                    print_node(node);
                }
            }
            Err(e) => println!("  !! {e}"),
        }
        println!();
    }

    Ok(())
}

/// One subject's line: what was written, and what it resolves to.
///
/// Both halves, because either alone is misleading. The patterns alone are what
/// the operator already has in front of them; the expansion alone hides which
/// line to edit.
fn print_row(subject: &str, patterns: &[String], kind: Option<RegistryKind>) -> Result<()> {
    if patterns.is_empty() {
        println!("  {subject:<24} (nothing)");
        return Ok(());
    }

    let expanded = expand_patterns(patterns, WildcardScope::Legacy)
        .with_context(|| format!("{subject}: {}", patterns.join(", ")))?;

    let written = patterns.join(", ");
    let resolved: Vec<&str> = expanded.iter().map(|a| a.as_str()).collect();

    // Only worth showing both when they differ — otherwise the line says the
    // same thing twice and the ones that *did* expand stop standing out.
    if resolved.len() == patterns.len() && written == resolved.join(", ") {
        println!("  {subject:<24} {written}");
    } else {
        println!("  {subject:<24} {written}");
        println!("  {:<24}   → {}", "", resolved.join(", "));
    }

    // An ecosystem verb on the wrong registry type is a startup failure
    // (`build_policy`), but `explain-config` is what someone runs *to find out
    // why*, so it names the problem rather than reporting the grant as if it
    // worked.
    if let Some(kind) = kind {
        let inert: Vec<&str> = expanded
            .iter()
            .filter(|a| !a.applies_to(kind))
            .map(|a| a.as_str())
            .collect();
        if !inert.is_empty() {
            println!(
                "  {:<24}   !! {} is not defined for this registry type — the server \
                 will refuse to start",
                "",
                inert.join(", ")
            );
        }
    }
    Ok(())
}

/// One node's grants, subject by subject.
fn print_node(node: &batlehub_core::entities::Node) {
    match &node.grants {
        None => println!("    (inherits)"),
        Some(g) if g.is_sealed() => {
            println!("    (sealed — stops inheritance; only the administrative floor survives)")
        }
        Some(g) => {
            for (subject, actions) in g.entries() {
                if actions.is_empty() {
                    continue;
                }
                println!(
                    "    {:<24} {}",
                    subject.to_string(),
                    actions
                        .iter()
                        .map(|a| a.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}
