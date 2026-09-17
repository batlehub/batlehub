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
//! **The air-gap column is enforced too, and it had to be.** It was declared
//! and unchecked for a while, and it drifted the way an unchecked declaration
//! does: on 2026-09-16 eight kinds — composer, github, gitlab, goproxy, maven,
//! nuget, rubygems, terraform — had a case in `crates/web/tests/air_gap.rs` and
//! still declared `Gap`. Nothing was missing but the row, and nothing could
//! fail, so the published number said 8 of 25 when the truth was 16. A count
//! that under-reports is worse than no count: it invites work that is already
//! done.
//!
//! So [`every_air_gap_claim_matches_the_cases_that_exist`] reads `air_gap.rs`
//! itself and compares the kinds its labs drive against this table, both ways.
//! An undeclared gap is worse than a declared one, and a *stale* one is worse
//! than both.
//!
//! What is left is 21 of 25, and the four `Gap` rows are all the same kind of
//! thing: the extension and plugin galleries, which answer by **query** rather
//! than with a document. There is no packument, simple page or index to project
//! the held set onto, which RFC 0008-bis §4 makes a non-goal rather than a gap
//! to close — so each row says that, instead of claiming nobody got to it.

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

/// One row per kind, and the test below refuses to let that stop being true.
const COVERAGE: &[(&str, Live, AirGap)] = &[
    // The phase name is the *client*, which is why it is rarely the kind's own
    // name: `mise` drives three forge kinds, `apt` drives deb, `java` is Maven.
    ("github", Live::ClosedWorld("mise"), AirGap::Case),
    ("forgejo", Live::ClosedWorld("forgejo"), AirGap::Case),
    ("gitlab", Live::ClosedWorld("gitlab"), AirGap::Case),
    ("cargo", Live::Suite("tests/heavy/rustup.sh"), AirGap::Case),
    ("npm", Live::ClosedWorld("node"), AirGap::Case),
    (
        "openvsx",
        Live::ClosedWorld("ovsx"),
        AirGap::Gap("no listing to compose: a gallery answers by *query*, not with a document — there is no packument, simple page or index to project the held set onto, which RFC 0008-bis §4 makes a non-goal rather than a gap to close"),
    ),
    ("goproxy", Live::ClosedWorld("go"), AirGap::Case),
    ("pypi", Live::ClosedWorld("python"), AirGap::Case),
    ("conda", Live::ClosedWorld("conda"), AirGap::Case),
    ("composer", Live::ClosedWorld("php"), AirGap::Case),
    (
        "vscode-marketplace",
        Live::ClosedWorld("vscode"),
        AirGap::Gap("no listing to compose: a gallery answers by *query*, not with a document — there is no packument, simple page or index to project the held set onto, which RFC 0008-bis §4 makes a non-goal rather than a gap to close"),
    ),
    ("maven", Live::ClosedWorld("java"), AirGap::Case),
    ("terraform", Live::ClosedWorld("terraform"), AirGap::Case),
    ("rubygems", Live::ClosedWorld("ruby"), AirGap::Case),
    ("nuget", Live::ClosedWorld("dotnet"), AirGap::Case),
    // deb / rpm / pacman share one air-gap case, `air_gap.rs::path_family_air_gap`,
    // because they share one handler and one answer: the index is **not**
    // composed, and the case pins that the refusal is honest — a `503` naming
    // the path, recorded once, nothing invented — and that every held file is
    // still served by path. Not composing is current behaviour rather than an
    // impossibility: this server already generates and signs all three indexes
    // in `local` mode, and what stops the air gap is that each is a *set* of
    // documents carrying checksums of each other, where `apk`'s is one file
    // (RFC 0008-bis §4).
    ("deb", Live::ClosedWorld("apt"), AirGap::Case),
    ("rpm", Live::ClosedWorld("dnf"), AirGap::Case),
    ("pacman", Live::ClosedWorld("pacman"), AirGap::Case),
    // A suite of its own rather than a `closed_world.sh` phase, for `cargo`'s
    // reason one format over: the client *is* the package manager of the
    // distribution this kind serves, and the suite has to fetch `apk.static`
    // out of the very tree it then proxies — twice, once per apk generation
    // (2.14.10 from v3.22, 3.0.8 from v3.23/v3.24). Neither is skipped: both
    // ship as static binaries, so neither needs a rootfs or a user namespace
    // (RFC 0026 §6.8).
    ("apk", Live::Suite("tests/heavy/apk.sh"), AirGap::Case),
    (
        "jetbrains",
        Live::ClosedWorld("jbr"),
        AirGap::Gap("no listing to compose: a gallery answers by *query*, not with a document — there is no packument, simple page or index to project the held set onto, which RFC 0008-bis §4 makes a non-goal rather than a gap to close"),
    ),
    (
        "jetbrains-marketplace",
        Live::ClosedWorld("jbplugin"),
        AirGap::Gap("no listing to compose: a gallery answers by *query*, not with a document — there is no packument, simple page or index to project the held set onto, which RFC 0008-bis §4 makes a non-goal rather than a gap to close"),
    ),
    ("generic", Live::ClosedWorld("helm"), AirGap::Case),
    ("nodedist", Live::ClosedWorld("nvm"), AirGap::Case),
    ("sdkman", Live::ClosedWorld("sdkman"), AirGap::Case),
    ("rustup", Live::Suite("tests/heavy/rustup.sh"), AirGap::Case),
    // Two suites drive this kind and the closed world is the one that counts
    // here: `ansible` installs `community.general` through the instance with
    // egress denied and then *runs* a plugin out of it. `tests/heavy/galaxy.sh`
    // is the wider one — blocking, the pinned refusal, the publish and its
    // import task, and the two role modes — and it needs the real upstream's
    // version history, which a closed world cannot provide.
    ("galaxy", Live::ClosedWorld("ansible"), AirGap::Case),
    // A suite of its own rather than a closed-world phase, and not for want of
    // trying: a `nix` binary is *dynamically linked into `/nix/store`* (its ELF
    // interpreter is `/nix/store/…-glibc/lib/ld-linux-x86-64.so.2`), there is
    // no static build published anywhere, and the release tarball is a store
    // closure rather than a binary. So the client cannot be unpacked into a run
    // directory the way `apk.static` can — it needs a real `/nix`, which only
    // root can create. The suite therefore works in a *chroot store*
    // (`--store 'local?root=…'`), which keeps the logical store dir
    // `/nix/store` while the bytes stay under its own temp directory; a
    // closed-world phase would have to duplicate that whole apparatus for no
    // extra evidence (RFC 0028 §6.10 and the suite's own header).
    ("nix", Live::Suite("tests/heavy/nix.sh"), AirGap::Case),
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

// ── The air-gap column, checked against the file that would make it true ─────

/// The air-gap cases themselves, read at compile time. A gate that asked a
/// human to keep two lists in step is the gate that let eight rows go stale.
const AIR_GAP_SRC: &str = include_str!("air_gap.rs");

/// Every helper in `air_gap.rs` that stands a disconnected app up for one kind.
/// Each takes the registry type as a **string literal**, which is what makes the
/// kind a case drives readable from the source instead of from a run.
///
/// A new helper belongs here the day it is written. Forgetting costs a false
/// `Gap` rather than a false `Case`, which is the direction to fail in.
const LAB_CALLS: &[&str] = &[
    "holding_lab(",
    "holding_lab_extra(",
    "signed_lab_of(",
    "local_registry_app_parts(",
    "local_registry_app_parts_with_artifact_meta(",
];

/// A table-driven case names its kinds in a field rather than at a call site —
/// `path_family_air_gap` covers deb, rpm and pacman from one `FAMILY` table.
const LAB_FIELDS: &[&str] = &["kind: "];

/// A kind whose case exists but drives it through something this scanner
/// cannot read — a helper taking the kind as a variable with no literal in
/// sight. Each entry says why, and an empty list is the healthy state.
const CASE_WITHOUT_A_READABLE_DRIVER: &[(&str, &str)] = &[];

/// The kinds `air_gap.rs` actually drives, read off its source.
///
/// Deliberately a scanner and not a list: the failure this exists to prevent is
/// a case being written and the row not being updated, and any mechanism that
/// needs a second edit to notice reproduces it exactly.
fn kinds_driven_by_an_air_gap_case() -> BTreeSet<String> {
    let known: BTreeSet<&str> = RegistryKind::ALL.iter().map(|k| k.as_str()).collect();
    let mut found = BTreeSet::new();

    for marker in LAB_CALLS.iter().chain(LAB_FIELDS.iter()) {
        let mut rest = AIR_GAP_SRC;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            // The first string literal after the marker is the kind: every
            // other argument these helpers take is an identifier, a bool or a
            // slice. A window, so a call with no literal cannot reach forward
            // into the next one and claim its kind.
            let window = &rest[..rest.len().min(200)];
            if let Some(open) = window.find('"') {
                let after = &window[open + 1..];
                if let Some(close) = after.find('"') {
                    let literal = &after[..close];
                    if known.contains(literal) {
                        found.insert(literal.to_owned());
                    }
                }
            }
        }
    }
    found
}

/// The air-gap column says what `air_gap.rs` does, in both directions.
///
/// Both halves matter and they fail for opposite reasons. A kind driven by a
/// case while declaring `Gap` under-reports the work already done — the failure
/// that actually happened, eight times. A kind declaring `Case` that no case
/// drives over-reports it, which is the failure that would hide a gap.
#[test]
fn every_air_gap_claim_matches_the_cases_that_exist() {
    let driven = kinds_driven_by_an_air_gap_case();
    let excused: BTreeSet<&str> = CASE_WITHOUT_A_READABLE_DRIVER
        .iter()
        .map(|(k, _)| *k)
        .collect();

    let mut understated = Vec::new();
    let mut overstated = Vec::new();
    for (kind, _, air_gap) in COVERAGE {
        match air_gap {
            AirGap::Gap(_) if driven.contains(*kind) => understated.push(*kind),
            AirGap::Case if !driven.contains(*kind) && !excused.contains(kind) => {
                overstated.push(*kind)
            }
            _ => {}
        }
    }

    assert!(
        understated.is_empty(),
        "{} kind(s) have a case in air_gap.rs and still declare AirGap::Gap: {}.\n\
         The work is done; the row is not. Flip it to AirGap::Case — a count that\n\
         under-reports invites someone to write a case that already exists.",
        understated.len(),
        understated.join(", "),
    );
    assert!(
        overstated.is_empty(),
        "{} kind(s) declare AirGap::Case and no case in air_gap.rs drives them: {}.\n\
         Either the case is missing, or it drives the kind through a helper this\n\
         scanner cannot read — add the helper to LAB_CALLS, or the kind to\n\
         CASE_WITHOUT_A_READABLE_DRIVER with the reason.",
        overstated.len(),
        overstated.join(", "),
    );
}
