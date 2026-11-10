# TD-0002 — What the `feat/devfile` review left for later

| | |
| --- | --- |
| **Status** | Paid |
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
`batlehub-cli audit purge --before` on a table with tens of millions of
rows becomes thousands of round trips inside one HTTP request. It can outlive
the client or ingress timeout and leave the purge half done.

**Retired when** an unsealed trail purges through `repo.purge_events_before`,
and only a sealed one pays for the rewrite.

**Paid** in `11684e9`. Reproduced first: an unsealed purge over an empty
trail store left the rows of the event repository in place, because it read
its candidates from the rewrite path. `purge_access_before` now calls
`purge_events_before` when sealing is off and keeps the rewrite only when it
is on. The in-memory repository gained the same access-class purge as the
Postgres `DELETE`; `an_unsealed_purge_is_one_delete_not_a_rewrite` pins it.
A sealed trail still purges in batches: on a large table it can outlive the
request, and rerunning it finishes the job.

## 2. A devfile blob or manifest lookup resolves every version

`crates/web/src/handlers/proxy/devfile.rs`, `artifact_for_digest`. A digest is
found by calling `resolve_metadata_uncaptured_for` once per version of the
stack, one after another. Cold, that is one upstream manifest GET per version
for each blob a `registry-library pull` asks for.

**Retired when** a digest → (version, artifact) map is built once from the
filtered index and the lookup reads it.

**Not a defect as written** (`d635f37`). Measured against the real client
and a counting upstream, the metadata cache (`metadata_ttl_secs`, 300 s by
default) answers every version walk after the first: a pull of a stack's last
version — manifest by digest, then two layers — fetched each version's
manifest once for the lookups. The other GETs are the objects being served
(each artifact has its own metadata entry), which a digest map would not
remove. `a_pull_by_digest_fetches_each_upstream_manifest_at_most_once_per_object`
pins the counts; a lookup that refetched would make both 4. What remains is
the first, cold walk, which is sequential — one round trip per version until
the digest is found — and every walk when `metadata_ttl_secs = 0`. Neither was
changed: fetching the versions concurrently would cost N GETs even when the
first one matches.

## 3. `devfile.not-anonymous` ignores pattern grants

`crates/config/src/schema/mod.rs`, `devfile_warnings`. It accepts only the
literal `releases:read` or `*` in `rbac.anonymous`. `releases:*` grants the read
through `expand_pattern`, works, and still warns on every start and reload that
every tile and pull will be refused, which pushes the operator to "fix" a
correct config.

**Retired when** the check expands the patterns the way authz does.

**Paid** in `7798b76`. Reproduced: `anonymous = ["releases:*"]` raised the
warning. The check now runs `expand_patterns` with the `Legacy` scope the RBAC
rule uses and looks for `releases:read`.

## 4. A config rejected from the editor is attributed to `config-editor`

`crates/web/src/services/reload/applier.rs`. A rejected config is recorded in
`config_changes` and on the audit stream as `user.id = "config-editor"`. An
applied one records the real admin. The `config_rejected.yml` Sigma rule
therefore cannot say who keeps submitting invalid configs.

**Retired when** `ReloadSource::AdminRequest` carries the caller's identity
through to the rejection row.

**Paid** in `9c516e2`, by another route than the one above.
`ReloadSource` is serialised in the pending-reload snapshot the API returns,
so carrying the identity in its variant would change the API.
`load_pending_from_content` takes the caller instead of a source (it was only
ever an admin request), and `reload_immediate` attributes its rejection to the
`triggered_by` it already applies under — a rejection from the immediate
reload endpoint was `config-editor` too. The file watcher's stays
`file-watcher`. `a_rejected_candidate_names_the_admin_who_sent_it` pins both
admin paths.

## 5. Retention accepts security rows expiring before the access rows

`crates/config/src/schema/audit.rs`. The rule that stops evidence expiring
before the traffic it explains is skipped when `access_retention_days = 0`
(keep forever). So `access_retention_days = 0` with
`security_retention_days = 30` is valid, and a test pins it: downloads are kept
forever while sign-ins, purges, grants and `gdpr_erase` rows go after 30 days.

**Retired when** `0` is treated as infinity in that comparison, and the test
pinning the case asserts a rejection.

**Paid** in `7da449e`. The pinning test was the reproduction: it validated
that config. `0` now counts as infinity, the case is refused, and the test
asserts the refusal. A config already holding `access_retention_days = 0`
with a non-zero `security_retention_days` no longer loads: the error names
both keys.

## 6. Two copies of the advisory-lock leader election

`PgAdvisoryLeader::try_lead` (`crates/adapters/src/db/packages/audit_trail.rs`)
repeats `PgScanQueue::try_lead` (`crates/adapters/src/db/security.rs`): hold a
pooled connection, probe it with `SELECT 1`, re-elect on error,
`pg_try_advisory_lock`. A fix to one (a probe timeout, a release on shutdown)
will miss the other.

**Retired when** the scan queue uses `PgAdvisoryLeader` with its own lock id.

**Paid** in `ce04d97`. The duplication hid a real defect, reproduced on
Postgres: the scan queue kept one connection whatever the key, so holding key
A answered `true` for key B without taking B's lock, and two processes could
both lead B. `PgScanQueue` now keeps one `PgAdvisoryLeader` per key;
`leadership_is_one_process_at_a_time_and_released_on_drop` (`pg_verdicts`)
asserts the per-key case.
