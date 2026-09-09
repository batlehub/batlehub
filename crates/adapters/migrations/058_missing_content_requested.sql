-- RFC 0008-bis §4.4 — the miss log, one column wider (two, in fact).
--
-- `requested_version`: the version the client asked for, when its request
-- named one — an artifact's, a forge release by tag. A listing names none.
-- `held_versions`: what this instance held of the package at the time, which
-- is what a synthesised listing named. Together they are the next plan's
-- diff: not "left-pad is missing" but "1.2.0 was asked for; 1.3.0 is held".
ALTER TABLE missing_content
    ADD COLUMN IF NOT EXISTS requested_version TEXT;

ALTER TABLE missing_content
    ADD COLUMN IF NOT EXISTS held_versions TEXT[] NOT NULL DEFAULT '{}';
