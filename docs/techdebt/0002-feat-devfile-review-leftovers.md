# TD-0002 — What the `feat/devfile` review left for later

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-10-11, code review of `feat/devfile` against `main` (at `75bb6c88`) |
| **Area** | audit trail, config validation, devfile registry, leader election |

The review raised ten findings. The branch fixed the ones that could lose data
or blind the audit stream before it merged: the housekeeping sweep deleting the
`metadata_cache` rows stale-on-error serves, the JSON audit stream being
filtered out by `RUST_LOG`, and the unthrottled `sign_in_failed` writes. The six
below are real but not urgent, and nobody has reproduced them yet beyond reading
the code. Each one says where to look and what would retire it.

## 1. The manual audit purge always rewrites, even with nothing sealed

`crates/core/src/services/audit_trail.rs`, the purge path. It now always goes
through `rewrite()`: it reads full rows 5 000 at a time and deletes them by id
list. Before, one `DELETE … WHERE created_at < $1 AND action IN (…)` did it.
`batlehub admin audit-log --purge-before` on a table with tens of millions of
rows becomes thousands of round trips inside one HTTP request. It can outlive
the client or ingress timeout and leave the purge half done.

**Retired when** an unsealed trail purges through `repo.purge_events_before`,
and only a sealed one pays for the rewrite.

## 2. A devfile blob or manifest lookup resolves every version

`crates/web/src/handlers/proxy/devfile.rs`, `artifact_for_digest`. A digest is
found by calling `resolve_metadata_uncaptured_for` once per version of the
stack, one after another. Cold, that is one upstream manifest GET per version
for each blob a `registry-library pull` asks for.

**Retired when** a digest → (version, artifact) map is built once from the
filtered index and the lookup reads it.

## 3. `devfile.not-anonymous` ignores pattern grants

`crates/config/src/schema/mod.rs`, `devfile_warnings`. It accepts only the
literal `releases:read` or `*` in `rbac.anonymous`. `releases:*` grants the read
through `expand_pattern`, works, and still warns on every start and reload that
every tile and pull will be refused, which pushes the operator to "fix" a
correct config.

**Retired when** the check expands the patterns the way authz does.

## 4. A config rejected from the editor is attributed to `config-editor`

`crates/web/src/services/reload/applier.rs`. A rejected config is recorded in
`config_changes` and on the audit stream as `user.id = "config-editor"`. An
applied one records the real admin. The `config_rejected.yml` Sigma rule
therefore cannot say who keeps submitting invalid configs.

**Retired when** `ReloadSource::AdminRequest` carries the caller's identity
through to the rejection row.

## 5. Retention accepts security rows expiring before the access rows

`crates/config/src/schema/audit.rs`. The rule that stops evidence expiring
before the traffic it explains is skipped when `access_retention_days = 0`
(keep forever). So `access_retention_days = 0` with
`security_retention_days = 30` is valid, and a test pins it: downloads are kept
forever while sign-ins, purges, grants and `gdpr_erase` rows go after 30 days.

**Retired when** `0` is treated as infinity in that comparison, and the test
pinning the case asserts a rejection.

## 6. Two copies of the advisory-lock leader election

`PgAdvisoryLeader::try_lead` (`crates/adapters/src/db/packages/audit_trail.rs`)
repeats `PgScanQueue::try_lead` (`crates/adapters/src/db/security.rs`): hold a
pooled connection, probe it with `SELECT 1`, re-elect on error,
`pg_try_advisory_lock`. A fix to one (a probe timeout, a release on shutdown)
will miss the other.

**Retired when** the scan queue uses `PgAdvisoryLeader` with its own lock id.
