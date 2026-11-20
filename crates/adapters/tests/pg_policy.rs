//! `PolicyRepository`, against both implementations.
//!
//! RFC 0015 §6.3's `policy` table, and the in-memory store that stands in for it
//! elsewhere. Same construction as `pg_grants.rs` and for the same reason:
//! agreement between an adapter and its double is not evidence on its own —
//! survey finding 2 shipped precisely because an empty list meant "everything"
//! in four repository implementations "that all agreed with each other". The
//! properties are stated once and each store is made to satisfy them separately.
//!
//! The Postgres half is skipped when `DATABASE_URL` is unset, and it reports the
//! skip rather than passing silently.

use std::sync::Arc;

use batlehub_adapters::db::PgPolicyRepository;
use batlehub_adapters::in_memory::InMemoryPolicyRepository;
use batlehub_core::entities::{Immutable, QuotaRules, RuleOverride, VersioningRules, Visibility};
use batlehub_core::ports::{version_node_key, NodeKind, PolicyRepository, StoredPolicy};

/// Both stores, or just the in-memory one when there is no database.
///
/// `registry` is the caller's own and is the only thing cleaned — the lesson
/// `pg_grants.rs` records: `cargo test` runs these concurrently, the in-memory
/// store is fresh per test and Postgres is not, and a blanket cleanup deleted
/// another test's rows mid-run. The two halves then disagreed, which is exactly
/// the signal this file exists to produce, about the harness rather than the
/// adapter.
async fn stores(registry: &str) -> Vec<(&'static str, Arc<dyn PolicyRepository>)> {
    let mut out: Vec<(&'static str, Arc<dyn PolicyRepository>)> =
        vec![("in-memory", InMemoryPolicyRepository::new())];

    match std::env::var("DATABASE_URL") {
        Ok(url) => {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .expect("connect to DATABASE_URL");
            batlehub_adapters::migrations::embedded_migrator()
                .run(&pool)
                .await
                .expect("migrations");
            sqlx::query("DELETE FROM policy WHERE registry = $1")
                .bind(registry)
                .execute(&pool)
                .await
                .expect("clean");
            out.push(("postgres", Arc::new(PgPolicyRepository::new(pool))));
        }
        Err(_) => {
            eprintln!(
                "note: DATABASE_URL is unset, so only the in-memory store was exercised. \
                 Set DATABASE_URL to check the Postgres one."
            );
        }
    }
    out
}

fn versioning() -> VersioningRules {
    VersioningRules {
        enforce_semver: true,
        allow_prerelease: false,
        version_pattern: Some(r"^\d+\.\d+\.\d+$".to_owned()),
        immutable: Immutable::Released,
        monotonic: true,
        dry_run: false,
    }
}

fn package_policy(registry: &str, package: &str) -> StoredPolicy {
    let mut p = StoredPolicy::new(registry, NodeKind::Package, package);
    p.visibility = Some(Visibility::Team);
    p.versioning = Some(versioning());
    p.quota = Some(QuotaRules {
        max_bytes_per_user: Some(1024),
        max_packages_per_user: Some(7),
        warn_threshold_pct: Some(80),
        block: true,
    });
    p.rules = vec![RuleOverride {
        gate: "release_age_gate".to_owned(),
        settings: serde_json::json!({ "kind": "release_age_gate", "min_age_secs": 0 }),
    }];
    p.set_by = Some("admin".to_owned());
    p
}

/// Everything a node can carry survives a write and a read, in both stores.
///
/// The whole-row round trip rather than a field at a time: the Postgres adapter
/// stores three of these as JSONB, and a field that serialises and does not
/// deserialise is the failure this catches.
#[tokio::test]
async fn a_full_policy_round_trips() {
    let reg = "policy-test-roundtrip";
    for (name, store) in stores(reg).await {
        let written = package_policy(reg, "@acme/cards");
        store.put_policy(written.clone()).await.expect(name);

        let read = store
            .policy_on_node(reg, NodeKind::Package, "@acme/cards")
            .await
            .expect(name)
            .unwrap_or_else(|| panic!("{name}: the row must be there"));
        assert_eq!(read, written, "{name}");
    }
}

/// `None` means **inherit**, and must not read back as a default.
///
/// The distinction §4.3 makes for grants, in its policy form: a row that stored
/// `public` where the operator wrote nothing would be an override with a default
/// value, and would stop the namespace above from applying.
#[tokio::test]
async fn an_absent_field_reads_back_absent_not_defaulted() {
    let reg = "policy-test-absent";
    for (name, store) in stores(reg).await {
        let mut p = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        p.visibility = Some(Visibility::Team);
        store.put_policy(p).await.expect(name);

        let read = store
            .policy_on_node(reg, NodeKind::Package, "pkg")
            .await
            .expect(name)
            .expect("present");
        assert_eq!(read.visibility, Some(Visibility::Team), "{name}");
        assert_eq!(read.prerelease_visibility, None, "{name}: must inherit");
        assert_eq!(read.versioning, None, "{name}: must inherit");
        assert_eq!(read.quota, None, "{name}: must inherit");
        assert!(read.rules.is_empty(), "{name}: must inherit");
    }
}

/// Both tiers in one call, **deepest last**.
///
/// The ordering is load-bearing rather than cosmetic: `PolicyPath::resolve`
/// takes the last declaration, so a package row arriving after its version row
/// would let the shallower tier win. That is the difference from `grants_for`,
/// where resolution unions and the order only affects `explain`'s provenance.
#[tokio::test]
async fn policy_for_returns_both_tiers_deepest_last() {
    let reg = "policy-test-tiers";
    for (name, store) in stores(reg).await {
        let mut pkg = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        pkg.visibility = Some(Visibility::Internal);
        store.put_policy(pkg).await.expect(name);

        let mut ver = StoredPolicy::new(reg, NodeKind::Version, version_node_key("pkg", "1.0.0"));
        ver.visibility = Some(Visibility::Public);
        store.put_policy(ver).await.expect(name);

        let rows = store
            .policy_for(reg, "pkg", Some("1.0.0"))
            .await
            .expect(name);
        assert_eq!(rows.len(), 2, "{name}: {rows:?}");
        assert_eq!(rows[0].node_kind, NodeKind::Package, "{name}");
        assert_eq!(rows[1].node_kind, NodeKind::Version, "{name}");
    }
}

/// A listing names a package and no version, and must not be handed version
/// rows for a coordinate the caller did not name.
#[tokio::test]
async fn policy_for_without_a_version_returns_only_the_package_tier() {
    let reg = "policy-test-noversion";
    for (name, store) in stores(reg).await {
        let mut pkg = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        pkg.visibility = Some(Visibility::Internal);
        store.put_policy(pkg).await.expect(name);
        let mut ver = StoredPolicy::new(reg, NodeKind::Version, version_node_key("pkg", "1.0.0"));
        ver.visibility = Some(Visibility::Public);
        store.put_policy(ver).await.expect(name);

        let rows = store.policy_for(reg, "pkg", None).await.expect(name);
        assert_eq!(rows.len(), 1, "{name}: {rows:?}");
        assert_eq!(rows[0].node_kind, NodeKind::Package, "{name}");
    }
}

/// An empty package name matches no node, in both stores.
///
/// Finding 2's shape as a direct assertion: a scoping predicate that matches
/// everything looks exactly like one that works.
#[tokio::test]
async fn an_empty_package_matches_nothing() {
    let reg = "policy-test-empty-name";
    for (name, store) in stores(reg).await {
        let mut pkg = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        pkg.visibility = Some(Visibility::Team);
        store.put_policy(pkg).await.expect(name);

        assert!(
            store
                .policy_for(reg, "", None)
                .await
                .expect(name)
                .is_empty(),
            "{name}: an empty coordinate must not match every row in the registry"
        );
    }
}

/// A node has exactly one policy: a second write replaces the first rather than
/// adding a second answer to "what applies here".
#[tokio::test]
async fn a_second_write_replaces_the_node() {
    let reg = "policy-test-replace";
    for (name, store) in stores(reg).await {
        let mut first = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        first.visibility = Some(Visibility::Team);
        first.versioning = Some(versioning());
        store.put_policy(first).await.expect(name);

        let mut second = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        second.visibility = Some(Visibility::Public);
        store.put_policy(second).await.expect(name);

        let rows = store.policy_for(reg, "pkg", None).await.expect(name);
        assert_eq!(rows.len(), 1, "{name}: {rows:?}");
        assert_eq!(rows[0].visibility, Some(Visibility::Public), "{name}");
        assert_eq!(
            rows[0].versioning, None,
            "{name}: the node is replaced whole, not merged"
        );
    }
}

/// A row that declares nothing is not a policy, so writing one deletes the node.
#[tokio::test]
async fn writing_an_empty_policy_removes_the_node() {
    let reg = "policy-test-empty";
    for (name, store) in stores(reg).await {
        let mut p = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        p.visibility = Some(Visibility::Team);
        store.put_policy(p).await.expect(name);

        store
            .put_policy(StoredPolicy::new(reg, NodeKind::Package, "pkg"))
            .await
            .expect(name);

        assert!(
            store
                .policy_on_node(reg, NodeKind::Package, "pkg")
                .await
                .expect(name)
                .is_none(),
            "{name}: an override that overrides nothing must not survive as a node"
        );
    }
}

/// §4.1's tier rules are enforced at the port, in both stores — the database
/// cannot constrain a JSONB document's contents, so this is where it happens.
#[tokio::test]
async fn a_version_tier_policy_is_validated() {
    let reg = "policy-test-validate";
    for (name, store) in stores(reg).await {
        let mut bad = StoredPolicy::new(reg, NodeKind::Version, version_node_key("pkg", "1.0.0"));
        bad.versioning = Some(versioning()); // carries the naming fields
        assert!(
            store.put_policy(bad).await.is_err(),
            "{name}: the naming fields have nothing to decide at version tier"
        );

        let mut quota = StoredPolicy::new(reg, NodeKind::Version, version_node_key("pkg", "1.0.0"));
        quota.quota = Some(QuotaRules::default());
        assert!(
            store.put_policy(quota).await.is_err(),
            "{name}: a per-version quota limits a thing published exactly once"
        );

        // …and the one field the tier exists for is accepted.
        let mut pin = StoredPolicy::new(reg, NodeKind::Version, version_node_key("pkg", "1.0.0"));
        pin.versioning = Some(VersioningRules {
            immutable: Immutable::Always,
            allow_prerelease: true,
            ..Default::default()
        });
        store.put_policy(pin).await.expect(name);
    }
}

/// Deleting a package takes its policy with it, at both tiers.
///
/// RFC 0016 §4.4's rule, quoted in §12: package-tier policy dies with the
/// package. A stale `visibility = "public"` outliving a package would silently
/// apply to whoever takes the name next.
#[tokio::test]
async fn deleting_a_package_removes_both_tiers() {
    let reg = "policy-test-delete";
    for (name, store) in stores(reg).await {
        let mut pkg = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        pkg.visibility = Some(Visibility::Team);
        store.put_policy(pkg).await.expect(name);
        let mut ver = StoredPolicy::new(reg, NodeKind::Version, version_node_key("pkg", "1.0.0"));
        ver.visibility = Some(Visibility::Public);
        store.put_policy(ver).await.expect(name);

        store.delete_package_policy(reg, "pkg").await.expect(name);

        assert!(
            store
                .policy_for(reg, "pkg", Some("1.0.0"))
                .await
                .expect(name)
                .is_empty(),
            "{name}"
        );
    }
}

/// …and it stops at the segment boundary.
///
/// RFC 0011-bis §4.2's `digital` versus `digital.pipeline-tools` bug, on the
/// delete path where it destroys rather than discloses. A bare prefix match
/// would take `pkg-internal`'s rows out with `pkg`'s.
#[tokio::test]
async fn deleting_a_package_does_not_take_its_prefix_neighbours() {
    let reg = "policy-test-delete-neighbour";
    for (name, store) in stores(reg).await {
        for key in ["pkg", "pkg-internal"] {
            let mut p = StoredPolicy::new(reg, NodeKind::Package, key);
            p.visibility = Some(Visibility::Team);
            store.put_policy(p).await.expect(name);
            let mut v = StoredPolicy::new(reg, NodeKind::Version, version_node_key(key, "1.0.0"));
            v.visibility = Some(Visibility::Team);
            store.put_policy(v).await.expect(name);
        }

        store.delete_package_policy(reg, "pkg").await.expect(name);

        assert!(
            store
                .policy_for(reg, "pkg", Some("1.0.0"))
                .await
                .expect(name)
                .is_empty(),
            "{name}: the named package goes"
        );
        assert_eq!(
            store
                .policy_for(reg, "pkg-internal", Some("1.0.0"))
                .await
                .expect(name)
                .len(),
            2,
            "{name}: and its prefix neighbour stays, at both tiers"
        );
    }
}

/// One registry's policy is not another's.
#[tokio::test]
async fn policy_is_scoped_to_its_registry() {
    let reg = "policy-test-scope";
    for (name, store) in stores(reg).await {
        let mut p = StoredPolicy::new(reg, NodeKind::Package, "pkg");
        p.visibility = Some(Visibility::Team);
        store.put_policy(p).await.expect(name);

        assert!(
            store
                .policy_for("policy-test-scope-other", "pkg", None)
                .await
                .expect(name)
                .is_empty(),
            "{name}"
        );
    }
}

/// An absent node is `None`, not an error, and deleting one is not an error
/// either.
#[tokio::test]
async fn absent_is_not_an_error() {
    let reg = "policy-test-absent-node";
    for (name, store) in stores(reg).await {
        assert!(store
            .policy_on_node(reg, NodeKind::Package, "nothing")
            .await
            .expect(name)
            .is_none());
        store
            .delete_policy(reg, NodeKind::Package, "nothing")
            .await
            .expect(name);
        store
            .delete_package_policy(reg, "nothing")
            .await
            .expect(name);
    }
}

/// An exemption is a rule whose `settings` say `exempt: true` — the shape the
/// exemption endpoint writes. Both stores find it, and neither takes a mere
/// override of the same gate for one. The Postgres query once looked for
/// `exempt` at the top of the rule and found no exemption at all.
#[tokio::test]
async fn exemptions_in_registry_reads_the_flag_in_settings() {
    let registry = "pg-policy-exemptions";
    for (name, store) in stores(registry).await {
        let rule = |exempt: bool| RuleOverride {
            gate: "security_verdict".to_owned(),
            settings: serde_json::json!({
                "gate": "security_verdict",
                "exempt": exempt,
                "exempt_until": "2099-01-01T00:00:00Z",
                "reason": "test",
            }),
        };
        for (version, exempt) in [("1.0.0", true), ("2.0.0", false)] {
            store
                .put_policy(StoredPolicy {
                    registry: registry.to_owned(),
                    node_kind: NodeKind::Version,
                    node_key: version_node_key("minimist", version),
                    visibility: None,
                    prerelease_visibility: None,
                    versioning: None,
                    quota: None,
                    rules: vec![rule(exempt)],
                    set_by: None,
                })
                .await
                .unwrap();
        }
        let found: Vec<String> = store
            .exemptions_in_registry(registry)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.node_key)
            .collect();
        assert_eq!(found, vec![version_node_key("minimist", "1.0.0")], "{name}");
    }
}
