-- RFC 0018 §5.4 — the scan queue. Jobs are leased, not consumed.
--
-- A row carries `leased_until` and `attempts`; a worker heartbeats while
-- scanning; a lease that expires (OOM on a hostile archive) returns the job to
-- the queue, and after `max_attempts` the verdict gets `SCANNER_ERROR`. The
-- queue is Postgres because it is the one store every deployment has, and
-- `FOR UPDATE SKIP LOCKED` is enough for "one job per new version" across any
-- number of workers.
--
-- Idempotent on the coordinate: the partial unique index lets one *open* job
-- exist per version, so a burst of first requests enqueues once.
CREATE TABLE IF NOT EXISTS scan_jobs (
    id             UUID        PRIMARY KEY,
    registry       TEXT        NOT NULL,
    package_name   TEXT        NOT NULL,
    version        TEXT        NOT NULL,
    published_at   TIMESTAMPTZ,
    artifact_sha256 TEXT,
    -- first_seen | webhook | rescan | backfill
    trigger        TEXT        NOT NULL,
    -- 0 = first_seen … 3 = backfill; lower is dequeued first.
    priority       SMALLINT    NOT NULL,
    attempts       INTEGER     NOT NULL DEFAULT 0,
    leased_until   TIMESTAMPTZ,
    leased_by      TEXT,
    last_error     TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at   TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS scan_jobs_open_coordinate_idx
    ON scan_jobs (registry, package_name, version)
    WHERE completed_at IS NULL;

CREATE INDEX IF NOT EXISTS scan_jobs_lease_idx
    ON scan_jobs (priority, created_at)
    WHERE completed_at IS NULL;
