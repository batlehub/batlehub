-- RFC 0018 §6.1 — what each scanner said about a version.
--
-- Replaced as a set with its verdict: a scan re-derives every finding, so the
-- previous rows go and the new ones come in one unit. `raw` is the scanner's
-- own output and is hostile data — read as JSON, never interpolated.
CREATE TABLE IF NOT EXISTS artifact_findings (
    id            UUID        PRIMARY KEY,
    registry      TEXT        NOT NULL,
    package_name  TEXT        NOT NULL,
    version       TEXT        NOT NULL,
    scanner       TEXT        NOT NULL,
    kind          TEXT        NOT NULL,
    code          TEXT        NOT NULL,
    severity      TEXT        NOT NULL,
    reference     TEXT,
    summary       TEXT        NOT NULL,
    confidence    SMALLINT,
    available_at  TIMESTAMPTZ,
    raw           JSONB       NOT NULL DEFAULT 'null'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS artifact_findings_coordinate_idx
    ON artifact_findings (registry, package_name, version);
