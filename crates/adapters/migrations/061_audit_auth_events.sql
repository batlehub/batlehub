-- RFC 0036 §6.1: authentication events in the audit trail.
--
-- `throttled_count` is how many further events of the same kind a throttled
-- writer suppressed before writing this row. Only `credential_rejected` sets
-- it (one row per source IP per minute); NULL everywhere else.
ALTER TABLE access_events
    ADD COLUMN IF NOT EXISTS throttled_count INTEGER;

-- What an event that is not about a package is about: a token's id and name,
-- a sign-in's provider. Never a secret.
ALTER TABLE access_events
    ADD COLUMN IF NOT EXISTS detail TEXT;

-- The source IP a personal access token was last presented from, written with
-- `last_used_at`. A token accepted from a different address emits
-- `token_new_source`. NULL until the first use after this migration, which is
-- "unknown", not "first use": no event is emitted against a NULL.
ALTER TABLE user_tokens
    ADD COLUMN IF NOT EXISTS last_used_ip TEXT;
