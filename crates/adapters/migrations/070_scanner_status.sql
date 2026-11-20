-- RFC 0050 §6.2 — one row per scanner, written by the worker on a state
-- change (or at most every 30 s), read by the security console's
-- *Scanners & worker* page. An error **class**, never error text: a message
-- can carry a sealed path (threat T5).
CREATE TABLE IF NOT EXISTS scanner_status (
    scanner            TEXT        PRIMARY KEY,
    last_ok_at         TIMESTAMPTZ,
    last_error_at      TIMESTAMPTZ,
    last_error_class   TEXT,
    unavailable_until  TIMESTAMPTZ
);
