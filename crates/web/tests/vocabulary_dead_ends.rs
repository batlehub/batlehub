//! RFC 0015 §11.5 — **the vocabulary has no dead ends in either direction.**
//!
//! > every verb in the enum is requested by at least one route, and every verb a
//! > route requests is in the enum. A verb nothing asks for is a grant an
//! > operator can write that does nothing; a route asking for a verb nobody can
//! > hold is a route nobody can reach. Both have shipped in this tree before,
//! > which is why it is a test rather than a review item.
//!
//! # The two directions are not equally hard
//!
//! **A route cannot request a verb outside the enum.** `Action` is closed and has
//! no `Other(String)` (`entities/permission.rs`), so there is nothing else to
//! pass — the compiler is the assertion and this file adds nothing to it. §13.3
//! records that half as already holding: *"a route cannot request a verb outside
//! the enum, because there is nothing else to pass."*
//!
//! **The other direction is the one that has been false since phase 1.** §13.3
//! deferred it — *"§11.5's dead-end test cannot pass here, because this phase
//! adds the write verbs without yet using them"* — and it stayed deferred through
//! four more phases while the vocabulary grew from 18 verbs to 31. §13.8 had to
//! name eleven unrequested verbs in prose because nothing checked. This is that
//! check.
//!
//! # Why the scope is every route and not the proxy surface
//!
//! §11.5 is explicit, and the reason is a verb this test would otherwise get
//! wrong: *"`catalogue:browse` is requested by the console's explore routes under
//! `/api/v1/`, and a dead-end check scoped to the proxy surface would report the
//! verb unreachable and be wrong."*
//!
//! # How "requested" is decided
//!
//! By reading the source, which wants justifying. A verb is *requested* where it
//! is passed to the decision function, and that is not visible from the router:
//! `authz_matrix.rs` can ask actix which pattern a request matched, but nothing
//! can ask it which `Action` the handler passed three frames down. Some verbs are
//! not requested in a handler at all — `releases:publish` is requested in
//! `local_registry/publish.rs`, below the web crate entirely.
//!
//! So this scans the trees that *ask* and excludes the ones that *grant*. That is
//! the same shape as `ROUTE_INVENTORY`: a stated mapping, checked in both
//! directions so it cannot rot.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use batlehub_core::entities::Action;

/// Source trees where a verb is **requested** — handed to the engine.
const REQUESTING: &[&str] = &[
    "crates/web/src",
    "crates/core/src/services/local_registry",
    "crates/core/src/services/proxy",
    // The engine's own tree: `browsable_registries` resolves `catalogue:browse`
    // on behalf of the explore routes, which is a request even though the call
    // is factored into a shared helper. A scan that stopped at the handler would
    // report the verb unreachable and be wrong — which is the mistake §11.5
    // warns about for this exact verb, arriving through a refactor instead of
    // through a scope.
    "crates/core/src/services/authz",
    "cli/src",
];

/// Files inside those trees that name a verb without **requesting** one, and so
/// must not count.
///
/// Three kinds. `translate.rs` and `server/src/grants.rs` *grant*, naming most
/// of the vocabulary while building §10 rule 5's maps — counting them would make
/// every verb look requested and turn this file green for exactly the wrong
/// reason. `entities/` *defines*. And `differential.rs` is a **harness**: it
/// names `releases:read` and `source:read` because those are the verbs it
/// compares two evaluators on, which is a statement about the test corpus rather
/// than about any route.
///
/// # The granting file is matched by path, not by basename
///
/// This read `ends_with("grants.rs")` until RFC 0017 added
/// `handlers/back_office/governance/grants.rs` — a file that *requests*
/// `grants:read` and `grants:write` and was silently excluded by a rule written
/// for a different file with the same name. The gate went green over two verbs
/// no route was seen to ask for, which is precisely the silence it exists to
/// break. A basename is not an identity; the granting file is named by where it
/// is.
fn is_not_a_requester(path: &Path) -> bool {
    let p = path.to_string_lossy();
    p.contains("/entities/")
        || p.ends_with("translate.rs")
        || p.ends_with("server/src/grants.rs")
        || p.ends_with("differential.rs")
}

/// Verbs deliberately requested by nothing, each with the reason.
///
/// **This list is the point of the test, not an escape from it.** Every entry is
/// a decision someone has to defend, the test fails when a verb leaves the enum's
/// requested set without being added here, and `no_stale_exceptions` fails when
/// an entry stops being true — so it cannot quietly become a list of everything.
const DELIBERATELY_UNREQUESTED: &[(&str, &str)] = &[
    // ── one verb, and it is a decision rather than a backlog item ────────────
    //
    // Its three siblings were on this list and are not any more —
    // `terraform:signing-keys:write` and `jetbrains:channel:assign` in §13.15,
    // `openvsx:namespace:claim` in §13.16 once the team-namespace separator
    // stopped being hardcoded to `/`. This one is not waiting on anything.
    //
    // **`npm:dist-tags:write` is a decision, not a backlog item.** §4.2 carries
    // the argument in full: dist-tags here are *derived* from the published
    // version set so RFC 0006's block-repair can move `latest` the instant a
    // version is blocked, and storing them forces a choice between a stored value
    // that lies, a blocked version served, or a broken `npm install`. It should
    // stay unrequested, and this entry is where somebody about to re-take that
    // decision will meet it.
    (
        "npm:dist-tags:write",
        "deliberate: dist-tags are derived so RFC 0006's block-repair can move \
         `latest`; storing them has no good answer when the tagged version is \
         withdrawn — see §4.2",
    ),
];

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/web is two levels below the workspace root")
        .to_path_buf()
}

/// Source with every `#[cfg(test)]` module removed.
///
/// A test module names verbs it is *about* rather than verbs a route requests, so
/// counting one would make this file green by describing itself.
///
/// **By brace matching, not by truncating at the first marker.** The first
/// version did truncate, on the assumption that test modules come last — and
/// `ops/quota.rs` puts its at line 15, above every handler, so the scan read six
/// lines of a file that requests `quota:read` four times and reported the verb as
/// a dead end. The convention was not a rule, and a check that depends on one is
/// a check that is wrong wherever the convention is not followed.
fn without_tests(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(i) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..i]);
        // Skip to the module body, then past its matching close brace.
        let after = &rest[i..];
        let Some(open) = after.find('{') else {
            break;
        };
        let mut depth = 0usize;
        let mut end = None;
        for (offset, ch) in after[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + offset + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(e) => rest = &after[e..],
            // Unbalanced: drop the remainder rather than count it.
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Whether this file is test code rather than code a route runs.
///
/// A sibling `tests.rs` carries no `#[cfg(test)]` marker of its own — the
/// attribute is on the `mod` declaration in its parent — so stripping inline test
/// modules does not reach it. `local_registry/tests.rs` names `releases:list`,
/// and without this the scan reported the verb as requested by a route when what
/// requests it is an assertion about a fixture.
fn is_test_file(path: &Path) -> bool {
    let p = path.to_string_lossy();
    p.ends_with("/tests.rs") || p.contains("/tests/")
}

/// Every verb some route asks the engine for.
fn requested_verbs() -> BTreeSet<String> {
    let root = workspace_root();
    let mut files = Vec::new();
    for tree in REQUESTING {
        rust_files(&root.join(tree), &mut files);
    }

    let mut found = BTreeSet::new();
    for path in files {
        if is_not_a_requester(&path) || is_test_file(&path) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let source = without_tests(&source);
        let source = source.as_str();
        for action in Action::ALL {
            // The Rust spelling, which is how a call site names it.
            let variant = format!("{action:?}");
            if source.contains(&format!("Action::{variant}")) {
                found.insert(action.as_str().to_owned());
            }
        }
    }
    found
}

/// The scan finds something, or every assertion below is vacuous.
///
/// The failure this guards against is not subtle: a wrong `REQUESTING` path, a
/// renamed directory, or a `workspace_root` that walks up one level too few makes
/// `requested_verbs` empty, and an empty set would make
/// `every_verb_is_requested_by_some_route` fail loudly — but would make
/// `no_stale_exceptions` pass silently.
#[test]
fn the_scan_actually_reads_the_tree() {
    let requested = requested_verbs();
    assert!(
        requested.len() > 3,
        "the source scan found {} verbs, which means it is not reading the tree it \
         thinks it is — check REQUESTING against the crate layout",
        requested.len()
    );
    assert!(
        requested.contains("releases:read"),
        "`releases:read` is requested by 76 sites; a scan that misses it is broken"
    );
}

/// §11.5's first direction: **no verb is a dead end.**
///
/// A verb nothing requests is a grant an operator can write, `explain` will
/// report, `config:explain` will print — and that changes nothing about what the
/// server does. §4.2 calls that failure out by name for a *typo'd* verb; a real
/// verb nobody consults is the same silence with a spelling that looks right.
#[test]
fn every_verb_is_requested_by_some_route() {
    let requested = requested_verbs();
    let excepted: BTreeSet<&str> = DELIBERATELY_UNREQUESTED.iter().map(|(v, _)| *v).collect();

    let dead_ends: Vec<&str> = Action::ALL
        .iter()
        .map(|a| a.as_str())
        .filter(|v| !requested.contains(*v) && !excepted.contains(v))
        .collect();

    assert!(
        dead_ends.is_empty(),
        "these verbs are in the vocabulary and requested by no route: {dead_ends:?}\n\
         \n\
         A verb nothing asks for is a grant that does nothing — an operator writes \
         it, `explain` reports it, and the server behaves identically. Either wire \
         it to the route it is for, or add it to DELIBERATELY_UNREQUESTED with the \
         reason it has none."
    );
}

/// …and the exception list cannot rot.
///
/// The other half of §11.1's two-directional gate, applied to this list: an entry
/// that has stopped being true is an entry nobody will revisit, and a list of
/// stale exceptions is how a check becomes decoration. When somebody wires
/// `releases:list`, this fails and makes them delete the excuse.
#[test]
fn no_stale_exceptions() {
    let requested = requested_verbs();
    let stale: Vec<&str> = DELIBERATELY_UNREQUESTED
        .iter()
        .map(|(v, _)| *v)
        .filter(|v| requested.contains(*v))
        .collect();

    assert!(
        stale.is_empty(),
        "these verbs are listed as requested by nothing, and are requested: {stale:?}\n\
         Delete the entry — the exception outlived the reason for it."
    );
}

/// Every exception names a verb that exists.
///
/// A typo here would silently except nothing while looking like it excepted
/// something, which is the same class of failure as a typo'd verb in a config —
/// the one §4.2 built a closed enum to remove.
#[test]
fn every_exception_names_a_real_verb() {
    let vocabulary: BTreeSet<&str> = Action::ALL.iter().map(|a| a.as_str()).collect();
    for (verb, reason) in DELIBERATELY_UNREQUESTED {
        assert!(
            vocabulary.contains(verb),
            "'{verb}' is excepted from the dead-end check and is not in the vocabulary"
        );
        assert!(
            !reason.trim().is_empty(),
            "'{verb}' is excepted without a reason, which is an exception nobody can review"
        );
    }
}

/// The second direction, recorded rather than asserted.
///
/// *"Every verb a route requests is in the enum"* holds because `Action` is a
/// closed enum with no free-text variant — there is nothing else a call site
/// could pass, and the compiler rejects the attempt. This test exists so a reader
/// looking for §11.5's second half finds it answered rather than missing.
#[test]
fn a_route_cannot_request_a_verb_outside_the_vocabulary() {
    // Every requested verb parses back to the variant it names: the scan reads
    // Rust spellings and the vocabulary is wire spellings, so a variant that
    // round-trips through neither would mean the two have drifted.
    for verb in requested_verbs() {
        assert!(
            verb.parse::<Action>().is_ok(),
            "'{verb}' was scanned as requested and is not a member of the enum"
        );
    }
}

// ── The published vocabulary ─────────────────────────────────────────────────
//
// The dead-end test above asks whether every verb is *requested*. This one asks
// whether every verb is *documented*, and it exists because the answer was no in
// the direction that hurts: `/guide/access-control` listed 14 of the 31 and
// named three — `cargo:owners:write`, `nuget:symbols:push`,
// `maven:metadata:write` — that have never existed in the enum. The same page
// says "a verb not on this list is a startup error", so an operator copying one
// out of the published documentation got a server that would not boot.
//
// That is §13.3's finding a second time (*"three permissions this repository had
// been granting to nobody, one of them in the published docs"*), and it recurred
// for the reason §11.5 gives about lists in prose: a closed set with a
// hand-maintained copy has two definitions, and only one of them compiles.

/// Every `` `verb` `` in a table row of the access-control guide.
fn documented_verbs() -> BTreeSet<String> {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide/access-control.md"),
    )
    .expect("the operator guide is part of the repository");

    // Only the `### The verbs` section. The page has other tables — the subject
    // forms, the policies — whose first cell is also a backticked token, and a
    // scan that swept the whole file would report `user:<id>` as an invented
    // verb. Bounded by the next `###`, so a table added to the section is
    // covered and one added elsewhere is not.
    let section = doc
        .split_once("### The verbs")
        .expect("the guide has a verbs section")
        .1;
    let section = section
        .split_once("\n### ")
        .map_or(section, |(before, _)| before);

    // **Every** backticked `a:b` token in the section, not only table cells.
    // The three invented verbs were in a prose sentence — "Four more are
    // ecosystem-scoped: `npm:dist-tags:write`, `cargo:owners:write`, …" — so a
    // scan of table rows would have read straight past the bug it exists to
    // catch, and passed.
    let mut found = BTreeSet::new();
    for span in section.split('`').skip(1).step_by(2) {
        // Verb-shaped only: `a:b`, lowercase, no spaces, not trailing. That
        // excludes the expansions (`releases:*`), the subject forms
        // (`group:<provider>:<name>`), the prefix written as `releases:` in
        // prose, and commands like `task config:explain`.
        let verb_shaped = span.contains(':')
            && !span.starts_with(':')
            && !span.ends_with(':')
            && span
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == ':' || c == '-');
        if verb_shaped {
            found.insert(span.to_owned());
        }
    }
    found
}

/// The guide's tables and `Action::ALL` are the same set.
///
/// Both directions matter and they fail differently. A verb in the enum and not
/// in the guide is a capability an operator cannot discover; a verb in the guide
/// and not in the enum is a config file that will not load — and that one was
/// live, in the published site, for three verbs.
#[test]
fn the_guide_documents_exactly_the_vocabulary() {
    let documented = documented_verbs();
    let real: BTreeSet<String> = Action::ALL.iter().map(|a| a.as_str().to_owned()).collect();

    assert!(
        !documented.is_empty(),
        "no verbs parsed out of the guide — the table format changed and this \
         test silently stopped checking anything"
    );

    let undocumented: Vec<&String> = real.difference(&documented).collect();
    let invented: Vec<&String> = documented.difference(&real).collect();

    assert!(
        undocumented.is_empty() && invented.is_empty(),
        "the guide and the vocabulary disagree.\n\
         \n\
         In the enum, absent from /guide/access-control:\n  {undocumented:?}\n\
         \n\
         In the guide, absent from the enum — a config file copied from the docs \
         fails at startup:\n  {invented:?}\n"
    );
}
