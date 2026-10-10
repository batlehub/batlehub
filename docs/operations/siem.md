# SIEM integration

BatleHub writes its audit trail twice: as rows in `access_events`, which the
console's audit log reads, and — when `[logging] format = "json"` — as one JSON
line per row on stdout, which a collector ships to a SIEM. The line is emitted
where the row is written, with the same fields, so a detection rule and an
export of the table never disagree about what happened. The repository ships
Sigma rules for that stream under `deploy/siem/sigma/`.

This page is the reference for the stream and the rules. Turning it on is one
setting, documented in [configuration § `[logging]`](../guide/configuration.md#logging).

## Turning it on

```toml
[logging]
format = "json"
```

Every log line becomes a JSON object. The audit lines are the ones with
`event.dataset = "batlehub.audit"` (equivalently `target = "batlehub::audit"`);
every other line is an ordinary log line in the same format. There is no file
sink and nothing to rotate: the collector reads stdout.

The default, `text`, writes exactly what this server wrote before the stream
existed — and no audit lines at all, because a download per line is noise in a
log a human reads.

## Field reference

| Field | Example | Meaning |
| --- | --- | --- |
| `event.dataset` | `batlehub.audit` | The selector. Every audit line has it, no other line does |
| `event.kind` | `event` | ECS constant |
| `event.category` | `authentication` | `authentication`, `iam`, `configuration` or `package` — fixed per action |
| `event.action` | `credential_rejected` | The action, in the audit log's own snake_case vocabulary |
| `event.outcome` | `denied` | `allowed`, `denied` or `error` |
| `event.reason` | `blocked: malware flagged by osv (…)` | Why a request was refused; empty when allowed |
| `event.id` | UUID | The row's id in `access_events` |
| `user.id` | `alice` | The principal, as the authenticating provider names it; empty for anonymous; `system`, or `system:<task>`, for what the process did on its own — an automatic IP ban, an audit lifecycle run, the scheduled storage coherence check are `system`, a block the upstream-disappearance audit placed is `system:upstream-audit`. Match the `system` prefix to find them all |
| `user.roles` | `user` | `anonymous`, `user` or `admin` |
| `source.ip` | `203.0.113.9` | The caller, as the proxy-trust rules resolved it — never a raw `X-Forwarded-For` |
| `user_agent.original` | `npm/10.9.0` | The client's `User-Agent` |
| `batlehub.registry` | `npm` | The registry, for package events |
| `package.name`, `package.version` | `left-pad`, `1.3.0` | The coordinate, for package events |
| `batlehub.audit.detail` | `token_id=… name="ci"` | What a non-package event is about — a token, a provider, a grant's subject. Never a secret |
| `batlehub.audit.throttled_count` | `20` | Attempts a throttled writer held back before this row (see below) |
| `batlehub.audit.persisted` | `true` | Whether the database write this line mirrors succeeded |
| `span.request_id` | `7f0c…` | The HTTP request the event belongs to; on every line of that request |

A field with nothing to say is an empty string, not an absent key.

`batlehub.audit.persisted = false` means the line was emitted and the row was
**not** stored — a database outage. The stream is then the only record, which
is the case for shipping it at all.

## What is recorded

Every action the audit log records is on the stream: downloads and listings,
blocks, deletions, grants, retention and cache runs, purges. Six actions exist
for authentication:

| `event.action` | When | `batlehub.audit.detail` |
| --- | --- | --- |
| `sign_in` | An OIDC sign-in completed | `provider=<name>` |
| `sign_in_failed` | A callback failed; `event.reason` is the class — `state_mismatch`, `token_exchange`, `claims`, `nonce`, … | `provider=<name>` when known |
| `token_create` | A personal access token was minted | `token_id=<uuid> name="<name>"` |
| `token_revoke` | A personal access token was revoked | `token_id=<uuid>` |
| `token_new_source` | A token was accepted from an address other than its last one | `token_id=<uuid> previous_ip=<ip>` |
| `credential_rejected` | A credential was presented and no provider accepted it | — |

No line ever carries a token value, a token hash, an authorization code or a
password.

**`credential_rejected` is throttled**: one row per source IP per minute, per
server process. The attempts held back are counted, and the count rides on the
*next* row written for that IP as `batlehub.audit.throttled_count`. A burst of
twenty-one refused attempts in a minute is therefore one row with a count of 0,
then — if the attempts continue — a row a minute later with a count of 20. With
*n* replicas each writes its own rows.

**`token_new_source`** is checked when the token's last-used time is written,
at most once a minute per token. A token with no recorded address yet — the
first use after an upgrade — is not "new".

Configuration reloads join the stream as `config_applied` and
`config_rejected`, the latter for a candidate the file watcher or the console
editor refused.

## The shipped rules

| Rule | Fires on | Level |
| --- | --- | --- |
| `audit_purge.yml` | any `audit_purge` | high |
| `grant_to_anonymous.yml` | a `grant_write` whose subject is `*` or `role:anonymous` | high |
| `blocked_package_pulled.yml` | a download of a coordinate a malware flag or a `MALWARE_SIGNAL` verdict blocks | high |
| `credential_rejected_burst.yml` | a `credential_rejected` row carrying a `throttled_count` of 10 or more | medium |
| `sign_in_failed_burst.yml` | 5 `sign_in_failed` from one `source.ip` in 10 minutes | medium |
| `denied_download_burst.yml` | 50 denied downloads from one `source.ip` in 5 minutes | medium |
| `bulk_pull.yml` | one `user.id` downloading 300 distinct packages in 10 minutes | medium |
| `token_new_source.yml` | any `token_new_source` | low |
| `retention_or_cache_clear.yml` | `cache_clear`, `retention_run`, `tombstone_compact` | low |
| `config_rejected.yml` | any `config_rejected` | low |

The thresholds are starting points. `bulk_pull.yml` in particular should sit
above the largest dependency graph your builds resolve, and
`retention_or_cache_clear.yml` is meant to be restricted to outside your
maintenance window with your SIEM's own time filter, which Sigma cannot express.

Convert them for your backend with sigma-cli:

```bash
uvx --from sigma-cli sigma plugin install splunk
uvx --from sigma-cli sigma convert -t splunk --without-pipeline deploy/siem/sigma/
```

The field names are already ECS, so no processing pipeline is needed.
`deploy/siem/README.md` in the repository carries a Vector and a Fluent Bit
snippet for shipping the stream.

## Adding a rule

1. Write it under `deploy/siem/sigma/`, one file per rule. A burst rule is two
   documents in one file: a named base rule and the correlation that counts it.
2. Add the file to `deploy/siem/fixtures/expected.json` — `true` if the
   recorded stream should make it fire, `false` if it should stay quiet — and,
   if the stream has nothing to test it against, add lines to
   `fixtures/stream.jsonl` that do and lines that nearly do.
3. Run `task siem:check`. It runs `sigma check` strictly, then replays the
   stream through every rule, and fails if a rule reads a field the server
   does not emit. That last check reads the field list from
   `crates/core/src/services/audit_stream.rs`, so a renamed field breaks the
   build instead of silently blinding a rule.

## Not covered yet

- **Tamper evidence.** A signed hash chain over the trail, and the
  `audit_chain_gap.yml` rule that watches it from the SIEM side, land with
  RFC 0036's phase 5. Until then, a copy of the stream held outside the
  database is the only defence against an edited row.
- **Retention and pseudonymisation** of the rows themselves are phase 4. The
  stream is unaffected: what a collector has stored is governed by the
  collector's own retention.
