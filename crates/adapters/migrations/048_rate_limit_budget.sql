-- RFC 0019 §5.2 — the rate-limit budget forge clients share.
--
-- One row per (registry, token fingerprint), holding what the forge last said
-- in `X-RateLimit-Remaining` / `X-RateLimit-Limit` / `X-RateLimit-Reset`. Both
-- roles — the proxy today, RFC 0018's scan worker when it lands — read it
-- before calling and refuse below their reserve (10 % for the proxy, 25 % for
-- the worker), so the first to hit the ceiling cannot take the other down.
--
-- The fingerprint is a truncated SHA-256 of the token, never the token: the
-- row has to be shareable across registries configured with the same
-- credential without becoming a place the credential is written.
CREATE TABLE IF NOT EXISTS rate_limit_budget (
    registry           TEXT        NOT NULL,
    token_fingerprint  TEXT        NOT NULL,
    remaining          BIGINT      NOT NULL,
    rate_limit         BIGINT      NOT NULL,
    reset_at           TIMESTAMPTZ NOT NULL,
    observed_at        TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (registry, token_fingerprint)
);
