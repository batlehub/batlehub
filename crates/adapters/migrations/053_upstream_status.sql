-- RFC 0014 §6.6: what the estate knows about an upstream's opinion of a
-- cached package. A row exists only while upstream has denied something.
CREATE TABLE IF NOT EXISTS upstream_status (
    registry            TEXT        NOT NULL,
    package_name        TEXT        NOT NULL,
    -- '' means "the whole package", not "a version named empty string".
    -- A nullable column cannot carry a primary key in Postgres, and the
    -- alternative — a surrogate id plus a partial unique index per
    -- nullability case — is two indexes and a NULL-safe upsert to express
    -- one fact. The adapter converts at its boundary; nothing above it
    -- knows the sentinel exists.
    version             TEXT        NOT NULL DEFAULT '',
    state               TEXT        NOT NULL,
    first_missed_at     TIMESTAMPTZ NOT NULL,
    last_checked_at     TIMESTAMPTZ NOT NULL,
    confirmed_at        TIMESTAMPTZ,
    consecutive_misses  INTEGER     NOT NULL DEFAULT 1,
    last_error          TEXT,
    PRIMARY KEY (registry, package_name, version)
);
CREATE INDEX IF NOT EXISTS upstream_status_state_idx
    ON upstream_status (registry, state);
