-- RFC 0048-bis: one row per running cold fill, shared by every replica
-- (crates/adapters/src/db/ops/fill_lease.rs).
--
-- `fill_id` is the fencing token: every write a holder makes matches it, and
-- it names the fill's segments in storage, so a holder whose lease was taken
-- over writes nothing anyone reads. Every expiry is compared with NOW() here,
-- never with a replica's clock.
CREATE TABLE IF NOT EXISTS fill_leases (
    storage_key  TEXT PRIMARY KEY,
    fill_id      UUID NOT NULL,
    holder       TEXT NOT NULL,                -- replica name, for logs only
    expires_at   TIMESTAMPTZ NOT NULL,
    phase        TEXT NOT NULL CHECK (phase IN
                   ('filling', 'verified', 'done', 'failed', 'no_store')),
    committed    INTEGER NOT NULL DEFAULT 0,   -- segments readers may read
    written      INTEGER NOT NULL DEFAULT 0,   -- segments that may exist
    total_bytes  BIGINT,
    error        JSONB,                        -- the FillError, 1 KiB at most
    started_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS fill_leases_expires_idx ON fill_leases (expires_at);
