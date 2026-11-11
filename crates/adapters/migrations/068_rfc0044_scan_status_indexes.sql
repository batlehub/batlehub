-- RFC 0044 decision 33 — the two reads the scan status and the findings list
-- add, each on an index rather than a walk of a table that only grows.
--
-- `scan_jobs` keeps every closed row. A list page reads the latest closed job
-- per coordinate (§4.2 rule 5), and the Scans page's `failed` filter reads the
-- same thing per registry: this index serves both, newest first.
CREATE INDEX IF NOT EXISTS scan_jobs_closed_coordinate_idx
    ON scan_jobs (registry, package_name, version, completed_at DESC)
    WHERE completed_at IS NOT NULL;

-- The findings list filters by registry and an advisory-id prefix (§6.2).
CREATE INDEX IF NOT EXISTS artifact_findings_reference_idx
    ON artifact_findings (registry, reference);
