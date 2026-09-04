// Embedded SQL migrator — avoids the sqlx `macros` feature which transitively
// pulls in sqlx-mysql → rsa (RUSTSEC-2023-0071, no upstream fix available).
// Migration::new() computes the SHA-384 checksum automatically, matching
// what `sqlx::migrate!()` would have embedded.

use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::SqlStr;
use std::borrow::Cow;

macro_rules! mig {
    ($ver:expr, $desc:literal, $path:literal) => {
        Migration::new(
            $ver,
            Cow::Borrowed($desc),
            MigrationType::Simple,
            SqlStr::from_static(include_str!($path)),
            false,
        )
    };
}

/// Build the embedded SQL migrator without connecting to a database.
pub fn embedded_migrator() -> Migrator {
    Migrator::with_migrations(vec![
        mig!(1, "init", "../migrations/001_init.sql"),
        mig!(
            2,
            "artifact storage",
            "../migrations/002_artifact_storage.sql"
        ),
        mig!(3, "user tokens", "../migrations/003_user_tokens.sql"),
        mig!(
            4,
            "artifact size bytes",
            "../migrations/004_artifact_size_bytes.sql"
        ),
        mig!(5, "metadata cache", "../migrations/005_metadata_cache.sql"),
        mig!(6, "local packages", "../migrations/006_local_packages.sql"),
        mig!(
            7,
            "artifact cache meta",
            "../migrations/007_artifact_cache_meta.sql"
        ),
        mig!(8, "quota", "../migrations/008_quota.sql"),
        mig!(
            9,
            "local packages status",
            "../migrations/009_local_packages_status.sql"
        ),
        mig!(10, "rate limit", "../migrations/010_rate_limit.sql"),
        mig!(
            11,
            "package ownership",
            "../migrations/011_package_ownership.sql"
        ),
        mig!(12, "signing", "../migrations/012_signing.sql"),
        mig!(13, "beta channel", "../migrations/013_beta_channel.sql"),
        mig!(14, "ip blocks", "../migrations/014_ip_blocks.sql"),
        mig!(
            15,
            "team namespaces",
            "../migrations/015_team_namespaces.sql"
        ),
        mig!(
            16,
            "package visibility",
            "../migrations/016_package_visibility.sql"
        ),
        mig!(
            17,
            "access events indexes",
            "../migrations/017_access_events_idx.sql"
        ),
        mig!(18, "config changes", "../migrations/018_config_changes.sql"),
        mig!(19, "system kv", "../migrations/019_system_kv.sql"),
        mig!(20, "artifact sboms", "../migrations/020_artifact_sboms.sql"),
        mig!(
            21,
            "access events covering idx",
            "../migrations/021_access_events_covering_idx.sql"
        ),
        mig!(
            22,
            "notification subscriptions",
            "../migrations/022_notification_subscriptions.sql"
        ),
        mig!(
            23,
            "inbound webhook events",
            "../migrations/023_inbound_webhook_events.sql"
        ),
        mig!(
            24,
            "notification subscriptions gin index",
            "../migrations/024_notification_subs_gin_index.sql"
        ),
        mig!(
            25,
            "artifact vulnerabilities",
            "../migrations/025_artifact_vulnerabilities.sql"
        ),
        mig!(
            26,
            "artifact cache checksum",
            "../migrations/026_artifact_cache_checksum.sql"
        ),
        mig!(
            27,
            "deprecation unlisting",
            "../migrations/027_deprecation_unlisting.sql"
        ),
        mig!(28, "user blocks", "../migrations/028_user_blocks.sql"),
        mig!(29, "audit ip ua", "../migrations/029_audit_ip_ua.sql"),
        mig!(
            30,
            "access events nullable target",
            "../migrations/030_access_events_nullable_target.sql"
        ),
        mig!(
            31,
            "stats history rollup",
            "../migrations/031_stats_history.sql"
        ),
        mig!(
            32,
            "artifact sbom license",
            "../migrations/032_artifact_sbom_license.sql"
        ),
        mig!(
            33,
            "stats history listing reads",
            "../migrations/033_stats_history_listing_reads.sql"
        ),
        mig!(
            34,
            "package readmes",
            "../migrations/034_package_readmes.sql"
        ),
        mig!(
            35,
            "package readme full-text search",
            "../migrations/035_package_readmes_fts.sql"
        ),
        mig!(
            36,
            "oidc login states",
            "../migrations/036_oidc_login_states.sql"
        ),
        mig!(
            37,
            "user tokens qualified by provider",
            "../migrations/037_user_tokens_provider.sql"
        ),
        mig!(
            38,
            "user token last used",
            "../migrations/038_user_tokens_last_used.sql"
        ),
        mig!(
            39,
            "local package tombstones",
            "../migrations/039_local_package_tombstones.sql"
        ),
        mig!(
            40,
            "retention keep pin",
            "../migrations/040_retention_keep.sql"
        ),
        mig!(41, "grants", "../migrations/041_grants.sql"),
        mig!(
            42,
            "ownership to grants",
            "../migrations/042_ownership_to_grants.sql"
        ),
        mig!(43, "policy", "../migrations/043_policy.sql"),
        mig!(
            44,
            "provider_signing_keys",
            "../migrations/044_provider_signing_keys.sql"
        ),
        mig!(
            45,
            "team_namespace_separator",
            "../migrations/045_team_namespace_separator.sql"
        ),
        mig!(
            46,
            "user_tokens_groups",
            "../migrations/046_user_tokens_groups.sql"
        ),
        mig!(
            47,
            "ref_resolutions",
            "../migrations/047_ref_resolutions.sql"
        ),
        mig!(
            48,
            "rate_limit_budget",
            "../migrations/048_rate_limit_budget.sql"
        ),
        mig!(
            49,
            "artifact_verdicts",
            "../migrations/049_artifact_verdicts.sql"
        ),
        mig!(
            50,
            "artifact_findings",
            "../migrations/050_artifact_findings.sql"
        ),
        mig!(51, "scan_jobs", "../migrations/051_scan_jobs.sql"),
        mig!(
            52,
            "worker_heartbeats",
            "../migrations/052_worker_heartbeats.sql"
        ),
        mig!(
            53,
            "upstream_status",
            "../migrations/053_upstream_status.sql"
        ),
        mig!(54, "package_flags", "../migrations/054_package_flags.sql"),
        mig!(
            55,
            "registry_scan_state",
            "../migrations/055_registry_scan_state.sql"
        ),
        mig!(
            56,
            "missing_content",
            "../migrations/056_missing_content.sql"
        ),
        mig!(57, "bundle_imports", "../migrations/057_bundle_imports.sql"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded list must have a `mig!` entry for every `.sql` file in
    /// `migrations/`, numbered contiguously from 1. This replaces a hand-bumped
    /// count: adding a migration needs only the new `mig!` entry + `.sql` file —
    /// no test edit — while a forgotten entry (or a numbering gap) fails here.
    #[test]
    fn embedded_migrator_is_contiguous_and_complete() {
        let m = embedded_migrator();

        // Versions are strictly increasing and contiguous starting at 1.
        for (i, mig) in m.iter().enumerate() {
            assert_eq!(
                mig.version,
                (i + 1) as i64,
                "migration #{i} should have version {} (versions must be contiguous from 1)",
                i + 1
            );
        }

        // Every `NNN_*.sql` on disk is embedded (catches a new file with no `mig!` entry).
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
        let sql_count = std::fs::read_dir(dir)
            .expect("read migrations dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"))
            })
            .count();
        assert_eq!(
            m.iter().count(),
            sql_count,
            "every .sql file in {dir} must have a mig!() entry (and vice versa)"
        );
    }
}
