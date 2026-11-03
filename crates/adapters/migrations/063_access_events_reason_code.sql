-- RFC 0040 §6.1, decision 39: the stable code of a refusal, beside the
-- free-text `deny_reason`. `GET /api/v1/me/downloads?outcome=denied` returns it
-- to the person refused (their own rows only), without the operator's prose.
-- Nullable: rows written before this column, and every non-refusal, have none.
ALTER TABLE access_events ADD COLUMN IF NOT EXISTS reason_code TEXT;
