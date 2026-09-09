-- RFC 0008 §6.3 — what an air-gapped instance was asked for and did not hold.
--
-- One row per (registry, storage_key): mise retries, and a log that grew with
-- the retries would bury the fact under its own repetitions. The counter and
-- `last_seen` are what an operator sorts by to find the gap that actually
-- hurts.
--
-- The key is attacker-writable — anyone who can ask this instance for a
-- package writes a row — so the table is capped per registry by the recorder
-- and swept by `miss_retention_days`.
CREATE TABLE IF NOT EXISTS missing_content (
    registry     TEXT        NOT NULL,
    storage_key  TEXT        NOT NULL,
    -- artifact | document | checksum | ref | unmirrored_host
    kind         TEXT        NOT NULL,
    -- The coordinate as the client spelled it: a label for a human, never
    -- parsed.
    coordinate   TEXT,
    first_seen   TIMESTAMPTZ NOT NULL,
    last_seen    TIMESTAMPTZ NOT NULL,
    count        BIGINT      NOT NULL DEFAULT 1,
    PRIMARY KEY (registry, storage_key)
);

-- The two orders the admin page and the retention sweep read in.
CREATE INDEX IF NOT EXISTS idx_missing_content_last_seen
    ON missing_content (last_seen DESC);

CREATE INDEX IF NOT EXISTS idx_missing_content_registry_count
    ON missing_content (registry, count DESC);
