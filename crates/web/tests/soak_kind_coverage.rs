//! Which registry kinds the soak actually drives — and, for the rest, why not.
//!
//! The leak suites (`perf/scripts/soak.sh`, `tests/heavy/soak.sh`) put the
//! server under constant load and fail when it does not give back what it took.
//! What they can find is bounded by what they *touch*: a leak lives in a code
//! path, and every kind brings its own client, its own document parser and its
//! own rewriter. A soak that drives npm alone exercises one of each, and would
//! be as green against a registry kind that leaked a megabyte per listing.
//!
//! So this is the same instrument as [`registry_kind_coverage`]'s air-gap
//! column, for the same reason: the covered set is read from
//! `perf/config.soak.toml` — one source of truth, so the claim cannot drift
//! from the configuration that makes it true — and every kind outside it owes
//! a written reason here. An undeclared gap is worse than a declared one,
//! because it reads as coverage.
//!
//! Adding a kind to the soak means deleting its line from `NOT_SOAKED`. Adding
//! a kind to `RegistryKind::ALL` means adding one. The test below refuses to
//! let either stop being true.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Why a kind is not under load yet. Every one of these is a fixture problem,
/// not a decision: the soak needs an upstream that speaks the protocol, and the
/// mock (`perf/mock-upstream`) speaks five.
const NOT_SOAKED: &[(&str, &str)] = &[
    (
        "github",
        "forge API: needs a mock that answers releases and refs, or a live token",
    ),
    ("forgejo", "forge API, as github"),
    ("gitlab", "forge API, as github"),
    (
        "cargo",
        "the mock serves the download but no sparse index per crate",
    ),
    (
        "openvsx",
        "needs a gallery document and a signed VSIX fixture",
    ),
    (
        "pypi",
        "needs a Simple index and a wheel the integrity check accepts",
    ),
    (
        "conda",
        "needs repodata or a shard index; the closed-world phase owns those",
    ),
    ("composer", "needs packages.json and a zip fixture"),
    ("vscode-marketplace", "needs the gallery's query protocol"),
    (
        "terraform",
        "needs the registry protocol's discovery and provider documents",
    ),
    ("nuget", "needs a flat index and a .nupkg fixture"),
    ("deb", "needs a Release/Packages index and a .deb"),
    ("rpm", "needs repomd.xml and an .rpm"),
    ("pacman", "needs a package database and a .pkg.tar.zst"),
    ("jetbrains", "needs the JetBrains download protocol"),
    (
        "jetbrains-marketplace",
        "needs the plugin repository protocol",
    ),
    ("nodedist", "file-shaped: cheap to add, not added yet"),
    ("sdkman", "needs the broker's candidate protocol"),
    ("rustup", "file-shaped: cheap to add, not added yet"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/web has a grandparent")
        .to_path_buf()
}

/// The kinds `perf/config.soak.toml` declares, read from the file itself.
///
/// A line-scan rather than a TOML parse: this needs one field of one table
/// array, the file is ours, and the alternative is a dependency for six lines.
/// `registry_kind_coverage.rs` reads a bash array the same way.
fn soaked_kinds() -> BTreeSet<String> {
    let config = std::fs::read_to_string(repo_root().join("perf/config.soak.toml"))
        .expect("perf/config.soak.toml is readable");
    let mut in_registry = false;
    let mut kinds = BTreeSet::new();
    for line in config.lines() {
        let line = line.trim();
        if line == "[[registries]]" {
            in_registry = true;
        } else if line.starts_with('[') && line != "[[registries]]" {
            // Any other table ends the `[[registries]]` header we are reading;
            // `[registries.rbac]` and friends come after the `type` line.
            in_registry = in_registry && line.starts_with("[registries.");
        } else if in_registry {
            if let Some(rest) = line.strip_prefix("type = ") {
                kinds.insert(rest.trim_matches(['"', ' ']).to_owned());
                in_registry = false;
            }
        }
    }
    kinds
}

/// Every kind in `RegistryKind::ALL`, read from its own source.
fn all_kinds() -> BTreeSet<String> {
    batlehub_core::entities::RegistryKind::ALL
        .iter()
        .map(|k| k.as_str().to_owned())
        .collect()
}

/// A kind is either under load or has a reason. Never neither, never both.
#[test]
fn every_registry_kind_is_soaked_or_declares_why_not() {
    let soaked = soaked_kinds();
    let excused: BTreeSet<String> = NOT_SOAKED.iter().map(|(k, _)| (*k).to_owned()).collect();
    let all = all_kinds();

    for (kind, reason) in NOT_SOAKED {
        assert!(
            !reason.trim().is_empty(),
            "'{kind}' is declared unsoaked with no reason"
        );
        assert!(
            all.contains(*kind),
            "NOT_SOAKED names '{kind}', which is not a registry kind"
        );
    }

    let both: Vec<_> = soaked.intersection(&excused).collect();
    assert!(
        both.is_empty(),
        "these kinds are both under load and excused from it — delete their \
         NOT_SOAKED line: {both:?}"
    );

    let neither: Vec<_> = all
        .iter()
        .filter(|k| !soaked.contains(*k) && !excused.contains(*k))
        .collect();
    assert!(
        neither.is_empty(),
        "these kinds are neither driven by the soak nor declared in NOT_SOAKED: \
         {neither:?} — add a registry to perf/config.soak.toml and an arm to \
         perf/k6/scenarios/10_soak.js, or write down why not"
    );

    println!(
        "soak coverage: {} of {} kinds under load ({}) — {} declared gap(s)",
        soaked.len(),
        all.len(),
        soaked.iter().cloned().collect::<Vec<_>>().join(", "),
        excused.len()
    );
}

/// The config and the load generator have to agree: a registry nothing asks for
/// is a row of zeroes in the report, which reads as a kind that costs nothing.
#[test]
fn every_soaked_registry_has_an_arm_in_the_scenario() {
    let scenario = std::fs::read_to_string(repo_root().join("perf/k6/scenarios/10_soak.js"))
        .expect("scenario");
    let config =
        std::fs::read_to_string(repo_root().join("perf/config.soak.toml")).expect("config");

    let names: Vec<String> = config
        .lines()
        .filter_map(|l| l.trim().strip_prefix("name = "))
        .map(|n| n.trim_matches(['"', ' ']).to_owned())
        .collect();
    assert!(!names.is_empty(), "the soak config declares no registry");

    for name in &names {
        // Either the scenario names the registry directly, or it reaches it
        // through a constant in `perf/k6/config.js`.
        let shared =
            std::fs::read_to_string(repo_root().join("perf/k6/config.js")).expect("k6 config");
        assert!(
            scenario.contains(name.as_str()) || shared.contains(name.as_str()),
            "'{name}' is configured for the soak but no arm in \
             perf/k6/scenarios/10_soak.js asks for it — it would report as a \
             registry that costs nothing"
        );
    }
}
