-- RFC 0050 §5.2 — the daily security rollup behind /security/trends.
--
-- One row per registry and UTC day. State columns (verdict counts, findings)
-- are written for the current day only, so history never drifts; event
-- columns are recomputed for yesterday and today and backfilled within each
-- source's retention. NULL is a day the source no longer holds, never zero.
CREATE TABLE IF NOT EXISTS security_daily (
    registry           TEXT    NOT NULL,
    day                DATE    NOT NULL,
    allowed            BIGINT,
    warned             BIGINT,
    quarantined        BIGINT,
    denied             BIGINT,
    findings_critical  BIGINT,
    findings_high      BIGINT,
    findings_medium    BIGINT,
    findings_low       BIGINT,
    findings_unknown   BIGINT,
    refusals           BIGINT,
    refusals_by_code   JSONB,
    flags_opened       BIGINT,
    flags_revoked      BIGINT,
    jobs_done          BIGINT,
    jobs_failed        BIGINT,
    PRIMARY KEY (registry, day)
);
