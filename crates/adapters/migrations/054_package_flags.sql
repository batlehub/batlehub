-- RFC 0002 §6.2 (recast by §13) — flags pushed by a trusted source.
--
-- One row per (source, external_id): a re-push updates in place, a revoke
-- leaves a tombstone (`revoked_at`) so the exposure report can still say
-- who pulled the version while the flag stood. `version` is an exact
-- version or '*' — ranges wait for the version-scheme RFC, and the column
-- that would hold them is deliberately absent so nothing can store one.
CREATE TABLE IF NOT EXISTS package_flags (
    id            UUID        PRIMARY KEY,
    source        TEXT        NOT NULL,
    external_id   TEXT        NOT NULL,
    registry      TEXT        NOT NULL,
    package_name  TEXT        NOT NULL,
    version       TEXT        NOT NULL,
    -- cve | malware | license | policy | anything the source says
    kind          TEXT        NOT NULL,
    -- inform | warn | gate | hard_block
    effect        TEXT        NOT NULL,
    severity      TEXT,
    summary       TEXT        NOT NULL,
    url           TEXT,
    first_seen    TIMESTAMPTZ NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL,
    expires_at    TIMESTAMPTZ,
    revoked_at    TIMESTAMPTZ,
    CONSTRAINT uq_package_flags_source_external UNIQUE (source, external_id),
    CONSTRAINT ck_package_flags_effect
        CHECK (effect IN ('inform', 'warn', 'gate', 'hard_block'))
);

-- The read every request on a registry without [security] makes, and the
-- join side of the exposure report.
CREATE INDEX IF NOT EXISTS idx_package_flags_package
    ON package_flags (registry, package_name)
    WHERE revoked_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_package_flags_source
    ON package_flags (source, updated_at DESC);
