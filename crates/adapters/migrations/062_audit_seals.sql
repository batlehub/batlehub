-- RFC 0036 §6.3–6.4: the audit trail's lifecycle.

-- The class scans of retention and purge: `action` picks the class, and
-- `created_at` the age. Without it every retention batch is a sequential scan
-- of the hottest table in the schema.
CREATE INDEX IF NOT EXISTS idx_access_events_action_created
    ON access_events (action, created_at);

-- The seal chain (§5.3). Append-only: `seal` closes a window, `amend` records
-- the lifecycle rewriting rows in one, `expire` records rows deleted from one.
-- Every record names the window's rows as they are *after* it, and chains to
-- the record before by `prev_digest`; `seq` is the position in the chain, which
-- a SIEM keys on (the same position seen twice with two digests is a rewritten
-- tail).
CREATE TABLE IF NOT EXISTS audit_seals (
    seq          BIGINT      PRIMARY KEY,
    kind         TEXT        NOT NULL CHECK (kind IN ('seal', 'amend', 'expire')),
    window_start TIMESTAMPTZ NOT NULL,
    window_end   TIMESTAMPTZ NOT NULL,
    row_count    BIGINT      NOT NULL,
    affected     BIGINT      NOT NULL DEFAULT 0,
    rows_digest  TEXT        NOT NULL,
    digest       TEXT        NOT NULL,
    prev_digest  TEXT        NOT NULL,
    signature    TEXT        NOT NULL,
    key_id       TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
