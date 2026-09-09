-- RFC 0019 §5.2 — what a forge ref resolved to, and what it resolved to before.
--
-- A branch is mutable by design and a tag by accident; a package registry has
-- neither. This table is what turns "the tag moved" from a fact nobody can see
-- into something detectable: the proxy trusts the first observation, and any
-- later resolution that disagrees is recorded as `previous_sha` — the field
-- phase 2's `TAG_MOVED` reads.
--
-- Keyed by the ref *as the client spelled it*, per registry and repository. A
-- full commit SHA is never stored: it resolves without a call and cannot move.
CREATE TABLE IF NOT EXISTS ref_resolutions (
    registry      TEXT        NOT NULL,
    owner_repo    TEXT        NOT NULL,
    git_ref       TEXT        NOT NULL,
    -- `tag` | `branch` — see `RefKind::as_str`.
    ref_kind      TEXT        NOT NULL,
    sha           TEXT        NOT NULL,
    resolved_at   TIMESTAMPTZ NOT NULL,
    previous_sha  TEXT,
    PRIMARY KEY (registry, owner_repo, git_ref)
);
