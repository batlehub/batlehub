-- RFC 0002 §4.6 — when the SBOM re-scan last covered each registry, so the
-- exposure report can say how fresh its CVE knowledge is. One row per
-- registry, overwritten by each pass.
CREATE TABLE IF NOT EXISTS registry_scan_state (
    registry           TEXT        PRIMARY KEY,
    last_scan_at       TIMESTAMPTZ NOT NULL,
    artifacts_scanned  BIGINT      NOT NULL DEFAULT 0,
    findings           BIGINT      NOT NULL DEFAULT 0,
    errors             BIGINT      NOT NULL DEFAULT 0
);
