-- RFC 0040 §6.5, decision 40: the CLI's loopback login. A started login may
-- name `http://127.0.0.1:<port>/callback`, validated before the row is written;
-- the callback redirects there instead of to the console. Nullable: the
-- console's logins, and every row written before this column, have none.
ALTER TABLE oidc_login_states ADD COLUMN IF NOT EXISTS return_to TEXT;
