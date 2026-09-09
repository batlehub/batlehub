-- RFC 0021 §6.5 — the last run of a configured release import.
--
-- Without this the console cannot tell an operator the one thing the scheduler
-- already knew and threw away. `server/src/watcher.rs` says it out loud: "'the
-- import ran and found nothing new' and 'the import has not run' are the two
-- states an operator needs to tell apart", and a `tracing::info!` cannot answer
-- either from a browser.
--
-- One row per run, not one per import: a history is what makes "it has been
-- failing since Tuesday" readable, where a single mutable last-run row would
-- show only the most recent symptom.
CREATE TABLE IF NOT EXISTS release_import_runs (
    id           UUID        PRIMARY KEY,
    -- The target registry, which is what the console groups by and what the
    -- route names. The source repo is recorded too, because one registry can
    -- have several imports configured into it and they fail independently.
    registry     TEXT        NOT NULL,
    repo         TEXT        NOT NULL,
    started_at   TIMESTAMPTZ NOT NULL,
    finished_at  TIMESTAMPTZ NOT NULL,
    imported     BIGINT      NOT NULL DEFAULT 0,
    skipped      BIGINT      NOT NULL DEFAULT 0,
    errors       BIGINT      NOT NULL DEFAULT 0,
    -- Who asked. `NULL` is the scheduler: an interval that fired has no
    -- operator behind it, and recording one would be a lie an audit reads.
    triggered_by TEXT,
    -- Named rather than counted, for the same reason the HTTP report names
    -- them: "3 errors" is not something an operator can act on.
    failures     TEXT
);

-- The console reads the newest run per registry, which is this index exactly.
CREATE INDEX IF NOT EXISTS idx_release_import_runs_registry_started
    ON release_import_runs (registry, started_at DESC);
