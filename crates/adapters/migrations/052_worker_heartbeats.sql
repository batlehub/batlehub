-- RFC 0018 §4.3 — which workers are alive.
--
-- The "is any worker running" check behind the startup warning and the
-- `batlehub_workers_live` gauge: a `[security]` registry with no live worker
-- keeps refusing `SCAN_PENDING` below `mature_age_secs`, and the operator
-- should hear that from the server rather than from a user's failed install.
CREATE TABLE IF NOT EXISTS worker_heartbeats (
    worker_id   TEXT        PRIMARY KEY,
    last_seen   TIMESTAMPTZ NOT NULL,
    registries  TEXT[]      NOT NULL DEFAULT '{}'
);
