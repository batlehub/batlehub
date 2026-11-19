-- The CLI's loopback login hands over a one-time code, not the tokens. A
-- started login carries the CLI's PKCE-style `code_challenge`; the callback
-- writes a second, short-lived row keyed by that code whose `handoff` holds the
-- issued tokens, redeemable once by the process that holds the verifier.
-- Nullable: console logins have neither.
ALTER TABLE oidc_login_states ADD COLUMN IF NOT EXISTS code_challenge TEXT;
ALTER TABLE oidc_login_states ADD COLUMN IF NOT EXISTS handoff TEXT;
-- Who started a loopback login (time, address, user agent, as JSON), shown on
-- the page that asks the signed-in user to authorize the CLI before any code
-- reaches the loopback.
ALTER TABLE oidc_login_states ADD COLUMN IF NOT EXISTS started TEXT;
