-- RFC 0018 §6.1 — the persisted verdict: what is known about a version
-- before it is served.
--
-- One row per version (not per file: every tarball, classifier and platform
-- file of a release shares the verdict). The state is re-derived on read from
-- the findings and the clock, so a `MIN_AGE_NOT_MET` written yesterday lifts
-- by itself; what the row records is the last evaluation and which scanners
-- have answered, so "pending" is a fact rather than a guess.
CREATE TABLE IF NOT EXISTS artifact_verdicts (
    registry         TEXT        NOT NULL,
    package_name     TEXT        NOT NULL,
    version          TEXT        NOT NULL,
    -- allowed | warned | quarantined | denied
    state            TEXT        NOT NULL,
    reason_codes     TEXT[]      NOT NULL DEFAULT '{}',
    policy_ref       TEXT        NOT NULL,
    available_at     TIMESTAMPTZ,
    evaluated_at     TIMESTAMPTZ NOT NULL,
    last_scanned_at  TIMESTAMPTZ,
    scanners_done    TEXT[]      NOT NULL DEFAULT '{}',
    PRIMARY KEY (registry, package_name, version)
);

CREATE INDEX IF NOT EXISTS artifact_verdicts_state_idx
    ON artifact_verdicts (registry, state);
