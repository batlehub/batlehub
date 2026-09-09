-- RFC 0008 §6.4 — what came across the gap, and when.
--
-- The disconnected instance's only content path is an import, so the history
-- of imports is the history of the instance: an operator asking "where did
-- this artifact come from" has no upstream to point at and this table
-- instead. One row per accepted bundle.
CREATE TABLE IF NOT EXISTS bundle_imports (
    bundle_id    TEXT        PRIMARY KEY,
    -- The hex ed25519 key whose signature verified. Not a secret: it is the
    -- public half, and naming it is how an operator tells two signers apart.
    signer_key   TEXT        NOT NULL,
    imported_at  TIMESTAMPTZ NOT NULL,
    imported_by  TEXT,
    entries      BIGINT      NOT NULL DEFAULT 0,
    blobs        BIGINT      NOT NULL DEFAULT 0,
    -- Blobs whose bytes did not hash to their own name, and entries whose
    -- key or blob was refused. Kept as a count and a sample, because an
    -- import that rejected something is the event worth reading.
    rejected     BIGINT      NOT NULL DEFAULT 0,
    rejected_sample TEXT,
    created_from TEXT
);

CREATE INDEX IF NOT EXISTS idx_bundle_imports_imported_at
    ON bundle_imports (imported_at DESC);
