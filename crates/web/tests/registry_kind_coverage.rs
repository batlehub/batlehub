//! Every registry kind is driven by a real client, live and air-gapped.
//!
//! A unit test says the adapter parses what the upstream sends; an integration
//! test says the route is wired. Both were green while three kinds were
//! unusable: Open VSX pointed `files.download` at a route no `ovsx` client asks
//! for, the GitLab client refused the document its own typed route requests, and
//! the JetBrains Marketplace could not resolve the only coordinate an IDE ever
//! learns. Each was found by a client, in `tests/heavy/closed_world.sh`, and
//! each would have been found the day the kind was added if the phase had
//! existed then.
//!
//! So this file is the drift gate for that: a kind cannot be added to
//! [`RegistryKind::ALL`] without declaring, here, which phase drives it — and
//! the claim is checked against the suite rather than trusted. See
//! `docs/contributing/adding-a-registry.md` §11 and step 9 of "Adding a new
//! registry adapter" in `CLAUDE.md`.
//!
//! **The credential boundary is deliberately not checked here.**
//! `tests/heavy/authz.sh` already owns that and enforces it better than this
//! file could: `authz_check_kinds_covered` reads `registry_kind.rs` itself and
//! fails when a kind is claimed by neither a client phase nor a route-level row
//! in `authz_read_rows`. Re-implementing it here would give two lists to keep in
//! step and one more thing to get wrong.
//!
//! The air-gap column is declared the same way but **not** enforced, because it
//! is not yet true: four kinds have a case in `crates/web/tests/air_gap.rs` and
//! the rest have none. It is written down as a finite, visible list that is
//! meant to shrink, exactly as `authz_matrix.rs` writes down the read routes no
//! row reaches — an undeclared gap is worse than a declared one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use batlehub_core::entities::RegistryKind;

/// How a kind is driven by a real client against a real upstream, with the
/// client unable to reach anything else.
#[derive(Debug, Clone, Copy)]
enum Live {
    /// A phase of `tests/heavy/closed_world.sh`, by its name in `PHASES`.
    ClosedWorld(&'static str),
    /// A suite of its own, because the kind *is* the toolchain the suite
    /// installs: `rustup` hands out the compiler, so its closed world has to
    /// bootstrap one.
    Suite(&'static str),
}

/// Whether the kind has been driven against a disconnected instance.
#[derive(Debug, Clone, Copy)]
enum AirGap {
    /// A case in `crates/web/tests/air_gap.rs`.
    Case,
    /// Nothing yet, and why it has not been done.
    Gap(&'static str),
}

const NO_CLIENT_YET: &str =
    "no air-gap case yet: the kind's client has not been driven against a disconnected instance";

/// One row per kind, and the test below refuses to let that stop being true.
const COVERAGE: &[(&str, Live, AirGap)] = &[
    // The phase name is the *client*, which is why it is rarely the kind's own
    // name: `mise` drives three forge kinds, `apt` drives deb, `java` is Maven.
    (
        "github",
        Live::ClosedWorld("mise"),
        AirGap::Gap("airgap.sh drives the github kind through mise; no in-process case"),
    ),
    (
        "forgejo",
        Live::ClosedWorld("forgejo"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "gitlab",
        Live::ClosedWorld("gitlab"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    ("cargo", Live::Suite("tests/heavy/rustup.sh"), AirGap::Case),
    ("npm", Live::ClosedWorld("node"), AirGap::Case),
    (
        "openvsx",
        Live::ClosedWorld("ovsx"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "goproxy",
        Live::ClosedWorld("go"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    ("pypi", Live::ClosedWorld("python"), AirGap::Case),
    ("conda", Live::ClosedWorld("conda"), AirGap::Case),
    (
        "composer",
        Live::ClosedWorld("php"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "vscode-marketplace",
        Live::ClosedWorld("vscode"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "maven",
        Live::ClosedWorld("java"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "terraform",
        Live::ClosedWorld("terraform"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "rubygems",
        Live::ClosedWorld("ruby"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "nuget",
        Live::ClosedWorld("dotnet"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    ("deb", Live::ClosedWorld("apt"), AirGap::Gap(NO_CLIENT_YET)),
    ("rpm", Live::ClosedWorld("dnf"), AirGap::Gap(NO_CLIENT_YET)),
    (
        "pacman",
        Live::ClosedWorld("pacman"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "jetbrains",
        Live::ClosedWorld("jbr"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "jetbrains-marketplace",
        Live::ClosedWorld("jbplugin"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "generic",
        Live::ClosedWorld("helm"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "nodedist",
        Live::ClosedWorld("nvm"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "sdkman",
        Live::ClosedWorld("sdkman"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
    (
        "rustup",
        Live::Suite("tests/heavy/rustup.sh"),
        AirGap::Gap(NO_CLIENT_YET),
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The words of a `NAME=(a b c)` bash array, which may wrap over lines.
fn bash_array(source: &str, name: &str) -> BTreeSet<String> {
    let start = source
        .find(&format!("\n{name}=("))
        .unwrap_or_else(|| panic!("{name}=( … ) is not in the file any more"));
    let open = source[start..].find('(').unwrap() + start + 1;
    let close = source[open..]
        .find(')')
        .unwrap_or_else(|| panic!("{name}=( … ) is never closed"))
        + open;
    source[open..close]
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_registry_kind_declares_how_a_real_client_proves_it() {
    let declared: BTreeSet<&str> = COVERAGE.iter().map(|(kind, ..)| *kind).collect();
    let known: BTreeSet<&str> = RegistryKind::ALL.iter().map(|k| k.as_str()).collect();

    let undeclared: Vec<&&str> = known.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "\n{} registry kind(s) are in RegistryKind::ALL and not in this file: {undeclared:?}\n\
         A kind is not finished when its adapter parses and its route answers — both were\n\
         true of three kinds that no client could use. Add a row saying which closed-world\n\
         phase drives it and which authz target proves its credential boundary, and write\n\
         the phase if it does not exist yet (docs/contributing/adding-a-registry.md §11).\n",
        undeclared.len()
    );

    let stale: Vec<&&str> = declared.difference(&known).collect();
    assert!(
        stale.is_empty(),
        "\n{stale:?} is declared here and is not a RegistryKind any more — drop the row.\n"
    );
}

#[test]
fn every_live_claim_names_a_phase_that_exists() {
    let closed_world = read("tests/heavy/closed_world.sh");
    let phases = bash_array(&closed_world, "PHASES");

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for (kind, live, _) in COVERAGE {
        match live {
            Live::ClosedWorld(phase) => {
                assert!(
                    phases.contains(*phase),
                    "\n'{kind}' claims the closed-world phase '{phase}', which is not in PHASES.\n\
                     Either the phase was renamed and this row was not, or the claim was never true.\n"
                );
                claimed.insert((*phase).to_owned());
            }
            Live::Suite(path) => assert!(
                repo_root().join(path).exists(),
                "\n'{kind}' claims the suite '{path}', which does not exist.\n"
            ),
        }
    }

    // And the other direction: a phase nobody claims is a phase whose kind was
    // removed, or a row that was never written.
    let orphans: Vec<&String> = phases.difference(&claimed).collect();
    assert!(
        orphans.is_empty(),
        "\nclosed-world phase(s) {orphans:?} are not claimed by any kind in this file.\n"
    );
}

/// Not an assertion — the air-gap column is a declared gap, and this prints how
/// big it is so it stays visible in the test output the way
/// `authz_matrix.rs`'s coverage reports do.
#[test]
fn report_air_gap_coverage() {
    let (covered, gaps): (Vec<_>, Vec<_>) = COVERAGE
        .iter()
        .partition(|(_, _, air)| matches!(air, AirGap::Case));

    for (kind, _, air) in &gaps {
        if let AirGap::Gap(reason) = air {
            assert!(
                !reason.trim().is_empty(),
                "'{kind}' declares an air-gap gap with no reason"
            );
        }
    }

    println!(
        "air-gap coverage: {} of {} kinds have a case ({}) — {} declared gap(s)",
        covered.len(),
        COVERAGE.len(),
        covered
            .iter()
            .map(|(k, ..)| *k)
            .collect::<Vec<_>>()
            .join(", "),
        gaps.len()
    );
}
