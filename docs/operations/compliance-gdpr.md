# GDPR — BatleHub controls

This page maps the articles of the General Data Protection Regulation (Reg.
2016/679) that touch a package proxy to the controls BatleHub implements. The
operator is the controller of the personal data an instance holds; BatleHub is
the tool it processes that data with.

::: warning A mapping, not a compliance statement
**Software cannot be GDPR-compliant; a processing activity can.** "✅
Implemented" means the control exists in the code, at the place named in the
Evidence column. Whether it is enabled, and whether your retention periods and
lawful bases are the right ones, is a decision for you and your DPO. See
[Compliance](./compliance.md).
:::

**Scope**: the BatleHub server and its database. A copy of the audit stream
shipped to a SIEM is a separate copy that *you* control: pseudonymisation and
erasure in the database do not reach it ([SIEM integration](./siem.md)).

---

## The personal data BatleHub stores

| Where | What | Why |
| --- | --- | --- |
| `access_events` | `user_id`, `ip_address`, `user_agent`, `detail` | the audit trail: who read, published, blocked or signed in, from where |
| `user_tokens` | owner, token name, `last_used_at`, `last_used_ip` | personal access tokens; the address feeds `token_new_source` |
| `user_blocks` | `user_id`, `blocked_by`, `reason` | an admin's decision to refuse a user |
| `ip_blocks`, `ip_violation_counters` | client IP addresses | automatic and manual IP blocking |
| `published_by`, package ownership grants | the publisher's and owner's user id | who published a version and who administers a package |

Token values are never stored (only a hash), and no audit row or stream line
carries a token, a hash, an OIDC code or a password.

---

## Art. 5 — Principles

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 5(1)(c) – Data minimisation | The audit row holds an id, an address and a user agent, never request bodies or credentials | ✅ Implemented | `crates/core/src/entities/access_log.rs` |
| 5(1)(e) – Storage limitation | Two retention classes: access rows (`Download`, `ViewMetadata`) and security rows (every other action, auth events included), each expired by the lifecycle job | ✅ Implemented | `[audit] access_retention_days`, `[audit] security_retention_days` |
| 5(1)(e) – Storage limitation | Access-class rows lose precision after a set age: IP truncated to /24 (IPv4) or /48 (IPv6), user agent dropped | ✅ Implemented | `[audit] pseudonymise_after_days` |
| 5(1)(e) – Choosing the periods | Retention periods that fit your purposes and legal obligations | Manual process | your record of processing |
| 5(1)(f) – Integrity | Closed windows of the trail are hash-chained and signed; rewriting a row without the key is detected | ✅ Implemented | `[audit] seal_interval_secs`, `[audit] seal_signing_key`, `batlehub-cli admin audit verify` |
| 5(2) – Accountability | Each lifecycle run, export and erasure is itself a security-class audit event (`audit_lifecycle_run`, `gdpr_export`, `gdpr_erase`) | ✅ Implemented | `GET /api/v1/admin/audit-log?action=gdpr_erase` |

## Art. 6 — Lawful basis

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 6(1)(f) – Legitimate interest | Security-class rows keep their full IP for their whole retention, because the source of a sign-in, grant or purge *is* the evidence; the shorter access-class retention keeps the two proportionate | Manual process | document the basis in your record of processing |
| 6(1)(c) – Legal obligation | Where a law obliges you to keep a record longer, set `security_retention_days` to it | Manual process | `[audit] security_retention_days` |

## Art. 15–21 — Data-subject rights

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 15 – Right of access | Export every row about one subject, including the publication and ownership rows erasure keeps; recorded as `gdpr_export` | ✅ Implemented | `batlehub-cli admin gdpr export --user <id>`, `GET /api/v1/admin/gdpr/export?user_id=<id>` (verb `audit:read`) |
| 17 – Right to erasure | Replaces the subject's id with `erased:<hmac>` in `access_events`, token rows and block rows; rows of one subject stay linkable to each other and to nobody | ✅ Implemented | `batlehub-cli admin gdpr erase --user <id>`, `POST /api/v1/admin/gdpr/erase` (verb `gdpr:erase`), `[audit] erasure_key` |
| 17(3) – Exemptions | Publication and ownership rows are retained under legitimate interest: "who published this" stays answerable for every consumer of the package | Documented | [RFC 0036 §4.2](../rfc/0036-regulatory-alignment.md#_4-2-behaviour-rules) |
| 17 – Erasure against an open decision | Refused for a subject with an open quarantine or block decision unless forced | ✅ Implemented | `batlehub-cli admin gdpr erase --user <id> --force` |
| 17 – Who may erase | `gdpr:erase` is its own verb; `audit:purge` does not imply it | ✅ Implemented | [Access control § verbs](../guide/access-control.md#verbs) |
| 12 – Answering within a month | Receiving, verifying and answering the request | Manual process | [Incident response § PII handling](./incident-response.md#pii-handling) |
| 21 – Right to object | Weighing an objection against your legitimate interest | Manual process | — |

Erasure needs `erasure_key`, an HMAC secret. Keep it **outside the database** —
an environment variable or a secret store — or the pseudonym is reversible by
whoever reads the database.

## Art. 25 — Data protection by design and by default

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 25(1) – By design | Pseudonymisation and expiry run only on sealed, closed windows, and each run is chained into the seal record | ✅ Implemented | `[audit]`, `audit_seals` table |
| 25(2) – By default | No `[audit]` table keeps the old behaviour (nothing expires) and logs a warning at start; the console lists it | ✅ Implemented | `GET /api/v1/admin/config/warnings` |
| 25(2) – By default | Setting the `[audit]` table, so the default warning does not stand in production | Manual process | your configuration |

## Art. 28, 30, 32–34 — Processors, records, security, breaches

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 28 – Processors | A SIEM, a hosting provider or a log pipeline that receives the stream is your processor | Manual process | [SIEM integration](./siem.md) |
| 30 – Record of processing | The inventory above is the input; the record is yours | Manual process | this page |
| 32 – Access control | Grants and verbs per registry, OIDC sign-in, hashed and expiring tokens | ✅ Implemented | [Access control](../guide/access-control.md), `crates/adapters/src/db/auth/user_tokens.rs` |
| 32 – Encryption in transit | TLS terminated at the ingress | Manual process | [Production hardening](./production-hardening.md) |
| 32 – Encryption at rest | Postgres and object-storage encryption | Manual process | your database and bucket settings |
| 32 – Resilience and restore | Backup and restore runbooks | Documented | [Disaster recovery](./disaster-recovery.md) |
| 33 – Breach notification (72 h) | Detecting the breach and notifying the supervisory authority | Manual process | [Incident response](./incident-response.md), the audit stream |
| 33(5) – Documenting a breach | The audit log export and a verified chain for the incident window | ✅ Implemented | `batlehub-cli admin export-audit-log`, `batlehub-cli admin audit verify --from <start> --to <end>` |

## Art. 44 — Transfers

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 44 – Transfers to third countries | Every outbound request the server makes, and what it carries | Documented | [What leaves this instance](./egress.md) |
| 44 – Where the instance and its SIEM run | Choosing the region | Manual process | — |

---

## Gaps

| Gap | Plan |
|-----|------|
| A copy already shipped to a SIEM is not pseudonymised or erased | The collector's own retention governs it; set it to match `[audit]` |
| `source.ip` is not recorded on admin actions (`grant_write`, `audit_purge`, …) | Follow-up in RFC 0036 §13 |
