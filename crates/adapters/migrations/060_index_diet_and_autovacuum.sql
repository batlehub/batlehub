-- Index diet and autovacuum tuning for the tables that grow or churn fastest.
--
-- Every DROP below names an index whose columns are a leading prefix of
-- another index (or the primary key) on the same table, so the planner keeps
-- the same access path and every insert stops paying for a second copy.
-- `access_events` takes one insert per proxied read; it carried eight indexes.

-- access_events: (registry) and (registry, package_name) are prefixes of
-- idx_access_events_pkg (registry, package_name, package_version, created_at).
DROP INDEX IF EXISTS idx_access_events_registry;
DROP INDEX IF EXISTS idx_access_events_registry_name;
-- (outcome) has three values; no query filters on it alone, and the ones that
-- combine it with a registry or a time range use those indexes instead.
DROP INDEX IF EXISTS idx_access_events_outcome;

-- package_statuses: both are prefixes of uq_package_status
-- (registry, package_name, package_version, COALESCE(package_artifact, '')).
DROP INDEX IF EXISTS idx_package_statuses_registry;
DROP INDEX IF EXISTS idx_package_statuses_registry_name;

-- Prefixes of the primary key (key|ip, window_start).
DROP INDEX IF EXISTS idx_rate_limit_key;
DROP INDEX IF EXISTS idx_ip_violation_ip;

-- The only read is "newest N across every webhook", which the
-- (webhook_name, received_at) index cannot serve; the housekeeping sweep
-- (server/src/stores.rs) reads by received_at too.
DROP INDEX IF EXISTS idx_inbound_webhook_events_name_time;
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_received_at
    ON inbound_webhook_events (received_at DESC);

-- Autovacuum. The defaults wait for 20 % of a table to change, which on a
-- hundred-million-row audit table is twenty million rows of stale visibility
-- map: the index-only scans migration 021 was written for fall back to heap
-- fetches long before a vacuum comes.
ALTER TABLE access_events SET (
    autovacuum_vacuum_insert_scale_factor = 0.01,
    autovacuum_vacuum_scale_factor        = 0.02,
    autovacuum_analyze_scale_factor       = 0.01
);

-- Counter tables: one row updated in place per request, rows deleted by the
-- minute. A free 30 % per page lets the `count` update stay a HOT update (no
-- indexed column changes), and the low scale factor reclaims the deletes.
ALTER TABLE rate_limit_counters SET (
    fillfactor                     = 70,
    autovacuum_vacuum_scale_factor = 0.02
);
ALTER TABLE ip_violation_counters SET (
    fillfactor                     = 70,
    autovacuum_vacuum_scale_factor = 0.02
);

-- Rewritten in place (large JSONB documents, lease updates, last-access
-- stamps) and purged by the housekeeping sweep: dead tuples, and TOAST for
-- metadata_cache, pile up well before 20 %.
ALTER TABLE metadata_cache       SET (autovacuum_vacuum_scale_factor = 0.05);
ALTER TABLE scan_jobs            SET (autovacuum_vacuum_scale_factor = 0.05);
ALTER TABLE artifact_cache_meta  SET (autovacuum_vacuum_scale_factor = 0.05);
ALTER TABLE oidc_login_states    SET (autovacuum_vacuum_scale_factor = 0.05);
