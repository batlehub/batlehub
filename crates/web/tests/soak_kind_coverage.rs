//! Which registry kinds the soak actually drives — and, for any it does not, why.
//!
//! The leak suites (`perf/scripts/soak.sh`, `tests/heavy/soak.sh`) put the
//! server under constant load and fail when it does not give back what it took.
//! What they can find is bounded by what they *touch*: a leak lives in a code
//! path, and every kind brings its own client, its own document parser and its
//! own rewriter. A soak that drives npm alone exercises one of each, and would
//! be as green against a registry kind that leaked a megabyte per listing.
//!
//! Three files have to agree for "the soak drives every kind" to be true, and
//! this is where they are made to:
//!
//!   - `perf/k6/soak_arms.js` — the arms, each naming the `kind` it drives;
//!   - `perf/config.soak.toml` — the registries those arms address;
//!   - [`NOT_SOAKED`] — the kinds that are deliberately left out, with a reason.
//!
//! The reason this is a test and not a comment: every one of those files reads
//! plausibly on its own while the set disagrees. An arm can name a kind and
//! point at a registry of another type; a registry can be declared and asked
//! for by nothing, which shows in the report as a kind that costs nothing.
//!
//! What it does **not** check is that the arms work — that is
//! `perf/k6/scenarios/11_soak_arms.js`, which asks for each one and requires
//! the status the arm declares. Run by `soak.sh` before the load, because the
//! load's own check is "not 5xx" and a 404 passes it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Kinds the soak does not drive, and why.
///
/// **Empty, as of the pass that added the remaining nineteen.** It is kept
/// because the first test below needs somewhere to put a kind that cannot be
/// driven, and an undeclared gap is worse than a declared one — it reads as
/// coverage.
///
/// A reason belongs here only when it is about the *kind*. "The mock does not
/// speak it yet" is not one of those: `perf/mock-upstream/src/protocols/` is
/// one module per protocol and a kind costs a handful of routes, which is what
/// this list being empty is evidence of.
const NOT_SOAKED: &[(&str, &str)] = &[];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/web has a grandparent")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
}

/// One arm of the mix: the kind it claims and the registry it addresses.
///
/// A line scan rather than a JavaScript parse, for the reason the config below
/// gets one: this needs two fields of an object literal, the file is ours, and
/// the alternative is a JS engine in a Rust test.
fn arms() -> Vec<(String, String)> {
    let source = read("perf/k6/soak_arms.js");
    let mut out = Vec::new();
    let mut kind: Option<String> = None;
    for line in source.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("kind: ") {
            kind = Some(rest.trim_matches([',', '"', ' ']).to_owned());
        } else if let Some(rest) = line.strip_prefix("registry: ") {
            // `registry: NPM_REGISTRY,` — the constant's name, resolved below
            // against `config.js`, so an arm cannot point at a registry that
            // does not exist by spelling one inline.
            if let Some(k) = kind.take() {
                out.push((k, rest.trim_matches([',', ' ']).to_owned()));
            }
        }
    }
    out
}

/// `{constant name: registry name}` from `perf/k6/config.js`.
fn registry_constants() -> BTreeMap<String, String> {
    read("perf/k6/config.js")
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("export const ")?;
            let (name, value) = rest.split_once(" = ")?;
            let value = value.trim().trim_end_matches(';').trim();
            // Only the plain string constants; `TERRAFORM_UPSTREAM_HOST` and
            // friends are expressions and are not registry names.
            let value = value.strip_prefix('"')?.strip_suffix('"')?;
            Some((name.trim().to_owned(), value.to_owned()))
        })
        .collect()
}

/// `{registry name: type}` from `perf/config.soak.toml`.
///
/// A line scan rather than a TOML parse: this needs two fields of one table
/// array, the file is ours, and the alternative is a dependency for ten lines.
/// `registry_kind_coverage.rs` reads a bash array the same way.
fn configured_registries() -> BTreeMap<String, String> {
    let config = read("perf/config.soak.toml");
    let mut out = BTreeMap::new();
    let (mut kind, mut name) = (None, None);
    for line in config.lines() {
        let line = line.trim();
        if line == "[[registries]]" {
            (kind, name) = (None, None);
        } else if let Some(rest) = line.strip_prefix("type = ") {
            kind = Some(rest.trim_matches(['"', ' ']).to_owned());
        } else if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim_matches(['"', ' ']).to_owned());
        }
        if let (Some(k), Some(n)) = (kind.as_ref(), name.as_ref()) {
            out.insert(n.clone(), k.clone());
            (kind, name) = (None, None);
        }
    }
    out
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
    let soaked: BTreeSet<String> = arms().into_iter().map(|(kind, _)| kind).collect();
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

    let unknown: Vec<_> = soaked.difference(&all).collect();
    assert!(
        unknown.is_empty(),
        "these arms name a kind that is not a registry kind: {unknown:?}"
    );

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
         {neither:?} — add a registry to perf/config.soak.toml, a protocol module \
         to perf/mock-upstream/src/protocols/ and an arm to perf/k6/soak_arms.js, \
         or write down why not"
    );

    println!(
        "soak coverage: {} of {} kinds under load, {} declared gap(s)",
        soaked.len(),
        all.len(),
        excused.len()
    );
}

/// An arm's `kind` has to be the type of the registry it addresses.
///
/// The check that makes the count above mean something. Without it an arm can
/// claim `conda` while pointing at the npm registry, and every file still reads
/// correctly on its own: the kind is a real kind, the registry is a real
/// registry, and the coverage report says conda is covered.
#[test]
fn every_arm_addresses_a_registry_of_the_kind_it_claims() {
    let constants = registry_constants();
    let configured = configured_registries();
    let mut problems = Vec::new();

    for (kind, constant) in arms() {
        let Some(registry) = constants.get(&constant) else {
            problems.push(format!(
                "arm for '{kind}' uses {constant}, which perf/k6/config.js does not export"
            ));
            continue;
        };
        match configured.get(registry) {
            None => problems.push(format!(
                "arm for '{kind}' addresses '{registry}', which perf/config.soak.toml \
                 does not declare"
            )),
            Some(actual) if *actual != kind => problems.push(format!(
                "arm claims kind '{kind}' but '{registry}' is a '{actual}' registry"
            )),
            Some(_) => {}
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n  "));
}

/// The config and the load generator have to agree the other way too: a
/// registry nothing asks for is a row of zeroes in the report, which reads as a
/// kind that costs nothing.
#[test]
fn every_soaked_registry_has_an_arm_in_the_mix() {
    let constants = registry_constants();
    let addressed: BTreeSet<String> = arms()
        .into_iter()
        .filter_map(|(_, constant)| constants.get(&constant).cloned())
        .collect();

    let configured = configured_registries();
    assert!(
        !configured.is_empty(),
        "the soak config declares no registry"
    );

    let unasked: Vec<_> = configured
        .keys()
        .filter(|name| !addressed.contains(*name))
        .collect();
    assert!(
        unasked.is_empty(),
        "these registries are configured for the soak but no arm in \
         perf/k6/soak_arms.js asks for them — they would report as registries \
         that cost nothing: {unasked:?}"
    );
}
