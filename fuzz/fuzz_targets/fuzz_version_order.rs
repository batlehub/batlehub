#![no_main]
//! The version order (RFC 0015 §4.5) and the two things built on it.
//!
//! `version_order::newest_first` decides which versions page one *is*, which
//! one the server names as `default_version`, and which ones `capped` throws
//! away; `blocking::best_latest` decides what a filtered listing calls
//! "latest" once the blocked one is gone. Both compare strings that registries
//! accept and semver does not — `1.0-SNAPSHOT`, `1.0.0.10`, `1.0rc1`, `v2.1`.
//!
//! 1. **`newest_first` is a total preorder.** Reflexive, antisymmetric and
//!    transitive over any three strings, and a `sort_by` over a list of them
//!    does not panic — since Rust 1.81 the standard sort *detects* a comparator
//!    that is not a total order and panics, so a comparator bug here is a
//!    request handler crash, not a misordered page.
//! 2. **`is_prerelease` is a function of the version alone**, stable under
//!    the same `v` prefix the order strips.
//! 3. **`best_latest` answers from its input**, prefers a stable strict-semver
//!    version whenever one exists, and never a lower one — checked against an
//!    independent read of the list with the `semver` crate, and cross-checked
//!    against `newest_first`: the answer is a `newest_first` minimum among the
//!    stable strict-semver entries, so the two notions of "newest" this
//!    codebase carries cannot drift apart on the inputs both can read.

use std::cmp::Ordering;

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::blocking::best_latest;
use batlehub_core::services::version_order::{is_prerelease, newest_first};

fn version(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    let n = |u: &mut arbitrary::Unstructured<'_>| u.int_in_range(0..=30u16);
    Ok(match u.int_in_range(0..=6u8)? {
        0 => u.arbitrary::<String>()?,
        1 => format!("{}.{}.{}", n(u)?, n(u)?, n(u)?),
        2 => format!("v{}.{}.{}", n(u)?, n(u)?, n(u)?),
        3 => format!(
            "{}.{}.{}{}",
            n(u)?,
            n(u)?,
            n(u)?,
            [
                "-rc1",
                "-SNAPSHOT",
                "+build.1",
                "rc1",
                "-beta.2",
                "b2",
                ".dev4",
                "-alpha",
                "final",
                "-dev",
                "+incompatible",
                "-0",
                "-rc.1.2",
            ][u.int_in_range(0..=12usize)?]
        ),
        4 => format!("{}.{}", n(u)?, n(u)?),
        5 => format!(
            "{}.{}.{}.{}",
            n(u)?,
            n(u)?,
            n(u)?,
            u.int_in_range(0..=200u16)?
        ),
        _ => format!("{}", n(u)?),
    })
}

fn strict_stable(v: &str) -> Option<semver::Version> {
    semver::Version::parse(v.strip_prefix('v').unwrap_or(v))
        .ok()
        .filter(|s| s.pre.is_empty())
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(count) = u.int_in_range(1..=8usize) else {
        return;
    };
    let mut versions = Vec::with_capacity(count);
    for _ in 0..count {
        let Ok(v) = version(&mut u) else { return };
        versions.push(v);
    }

    // 1. Total preorder, over every triple the list offers.
    for a in &versions {
        assert_eq!(
            newest_first(a, a),
            Ordering::Equal,
            "{a:?} is not equal to itself"
        );
        for b in &versions {
            let ab = newest_first(a, b);
            let ba = newest_first(b, a);
            assert_eq!(
                ab,
                ba.reverse(),
                "newest_first({a:?}, {b:?}) = {ab:?} but reversed is {ba:?}"
            );
            for c in &versions {
                let bc = newest_first(b, c);
                let ac = newest_first(a, c);
                match (ab, bc) {
                    (Ordering::Less, Ordering::Less | Ordering::Equal)
                    | (Ordering::Equal, Ordering::Less) => assert_eq!(
                        ac,
                        Ordering::Less,
                        "{a:?} < {b:?} and {b:?} <= {c:?} but newest_first({a:?}, {c:?}) = {ac:?}"
                    ),
                    (Ordering::Equal, Ordering::Equal) => assert_eq!(
                        ac,
                        Ordering::Equal,
                        "{a:?} == {b:?} == {c:?} but newest_first({a:?}, {c:?}) = {ac:?}"
                    ),
                    _ => {}
                }
            }
        }
        // 2. A `v` prefix is stripped before anything is decided.
        if !a.starts_with('v') && !a.starts_with("dev-") {
            assert_eq!(
                is_prerelease(a),
                is_prerelease(&format!("v{a}")),
                "is_prerelease disagrees about {a:?} with and without a v prefix"
            );
        }
    }
    let mut sorted = versions.clone();
    sorted.sort_by(|a, b| newest_first(a, b));

    // 3. best_latest.
    let Some(best) = best_latest(&versions) else {
        panic!("best_latest returned None for a non-empty list {versions:?}");
    };
    assert!(
        versions.contains(&best),
        "best_latest answered {best:?}, not in {versions:?}"
    );
    let stable: Vec<(&String, semver::Version)> = versions
        .iter()
        .filter_map(|v| strict_stable(v).map(|s| (v, s)))
        .collect();
    if let Some((_, top)) = stable.iter().max_by(|a, b| a.1.cmp(&b.1)) {
        let best_parsed = strict_stable(&best)
            .unwrap_or_else(|| panic!("{best:?} is not stable semver but {stable:?} is available"));
        assert_eq!(
            &best_parsed, top,
            "best_latest picked {best:?} over a newer stable version"
        );
        for (v, _) in &stable {
            assert_ne!(
                newest_first(v, &best),
                Ordering::Less,
                "newest_first says {v:?} is newer than best_latest's {best:?}"
            );
        }
    }
});
