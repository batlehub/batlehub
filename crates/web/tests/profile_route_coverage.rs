//! `perf/profile_routes.txt` names every API operation exactly once, as either
//! `profiled` or `skip: <reason>`.
//!
//! `perf/scripts/profile.sh` profiles the CPU of each soak arm alone and
//! records the routes the router matched while it ran. Its report checks the
//! `profiled` half against those measured routes, but that only runs when the
//! `profile` label is on a pull request. This test runs on every one, and it
//! is what stops a new route from slipping past the profile unnoticed: a route
//! the server registers and the file does not name fails here, with the line
//! to add.
//!
//! See `perf/README.md` § "CPU profile per path".

mod common;
use common::*;

use std::collections::{BTreeMap, BTreeSet};

const INVENTORY: &str = include_str!("../../../perf/profile_routes.txt");

fn spec_operations() -> BTreeSet<String> {
    let spec = batlehub_web::openapi_spec();
    let mut ops = BTreeSet::new();
    for (path, item) in &spec.paths.paths {
        let methods = [
            ("GET", item.get.is_some()),
            ("PUT", item.put.is_some()),
            ("POST", item.post.is_some()),
            ("DELETE", item.delete.is_some()),
            ("PATCH", item.patch.is_some()),
            ("HEAD", item.head.is_some()),
        ];
        for (method, present) in methods {
            if present {
                ops.insert(format!("{method} {}", canonical(path)));
            }
        }
    }
    ops
}

#[test]
fn the_profile_route_inventory_matches_the_router() {
    let mut listed: BTreeMap<String, &str> = BTreeMap::new();
    let mut problems = Vec::new();
    for line in INVENTORY.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((route, status)) = line.split_once('\t') else {
            problems.push(format!("no tab between route and status: {line:?}"));
            continue;
        };
        let (method, path) = route.trim().split_once(' ').unwrap_or(("", route));
        let key = format!("{method} {}", canonical(path));
        let status = status.trim();
        let valid = status == "profiled"
            || status
                .strip_prefix("skip:")
                .is_some_and(|reason| !reason.trim().is_empty());
        if !valid {
            problems.push(format!(
                "{key}: status must be `profiled` or `skip: <reason>`, got {status:?}"
            ));
        }
        if listed.insert(key.clone(), status).is_some() {
            problems.push(format!("{key} is listed twice"));
        }
    }

    let spec = spec_operations();
    let unlisted: Vec<&String> = spec.iter().filter(|op| !listed.contains_key(*op)).collect();
    if !unlisted.is_empty() {
        problems.push(format!(
            "{} operation(s) the server registers are absent from perf/profile_routes.txt. \
             Add an arm to perf/k6/soak_arms.js that reaches each and mark it `profiled`, \
             or record why it is not profiled:\n{}",
            unlisted.len(),
            unlisted
                .iter()
                .map(|op| format!("{op}\tskip: no arm yet"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    let stale: Vec<&String> = listed.keys().filter(|op| !spec.contains(*op)).collect();
    if !stale.is_empty() {
        problems.push(format!(
            "perf/profile_routes.txt names operation(s) the server no longer registers — \
             delete them:\n{}",
            stale
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    assert!(problems.is_empty(), "\n{}\n", problems.join("\n\n"));
}
