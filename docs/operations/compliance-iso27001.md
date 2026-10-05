# ISO/IEC 27001:2022 — BatleHub controls

This page maps the Annex A controls of ISO/IEC 27001:2022 that a package proxy
serves to the controls BatleHub implements. An operator whose ISMS covers a
BatleHub instance can hand it to the auditor as the starting point for the
statement of applicability.

::: warning A mapping, not a certificate
**The certificate belongs to your ISMS, not to the software.** "✅ Implemented"
means the control exists in the code, at the place named in the Evidence
column; it says nothing about whether it is enabled in your deployment or
operating over the audit period. See [Compliance](./compliance.md).
:::

**Scope**: the BatleHub server, its CLI, its scan worker and the project's own
development and vulnerability process. The organisational controls (5.1–5.14,
6.x people controls, 7.x physical controls) are yours and are not listed.

---

## 5 — Organisational controls

| Control | BatleHub control | Status | Evidence |
|---------|------------------|--------|----------|
| 5.15 – Access control | Grants of verbs on the registry → package → version hierarchy; a decision is deny unless a grant allows it | ✅ Implemented | [Access control](../guide/access-control.md), `POST /api/v1/admin/access-check` |
| 5.16 – Identity management | Identities come from your IdP (OIDC), Kubernetes service accounts, CI OIDC or static tokens; provider names are unique | ✅ Implemented | `[[auth]]` in [configuration](../guide/configuration.md), `crates/adapters/src/auth/` |
| 5.17 – Authentication information | Personal access tokens stored as SHA-256 hashes, with expiry; the value is shown once | ✅ Implemented | `crates/adapters/src/db/auth/user_tokens.rs`, `crates/adapters/src/auth/user_token.rs` |
| 5.18 – Access rights | Revoke a token, block a user, list and remove grants | ✅ Implemented | `DELETE /api/v1/auth/tokens/{id}`, `POST /api/v1/admin/users/{user_id}/block`, `batlehub-cli admin grants` |
| 5.18 – Periodic access review | Reviewing grants and tokens on a schedule | Manual process | `batlehub-cli admin grants`, `GET /api/v1/admin/audit-log?action=sign_in` |
| 5.21 – ICT supply chain | Every proxied artifact can be scanned (SBOM, CVE, verdicts), flagged by your SOC, held by a release-age gate or blocked | ✅ Implemented | [SBOM](../guide/sbom.md), [Scan worker](./scan-worker.md), `crates/core/src/rules/` |
| 5.21 – BatleHub's own supply chain | `cargo audit`, `cargo deny`, `pnpm audit`, postmortem, Trivy; no suppressions | ✅ Implemented | `.github/workflows/back-dep-audit.yaml`, `.github/workflows/image-scan.yaml`, `deny.toml` |
| 5.24–5.27 – Incident management | Playbook: severity, detection, containment, eradication, recovery, post-mortem | Documented | [Incident response](./incident-response.md) |
| 5.28 – Collection of evidence | Audit log export, and a signed seal chain whose `verify` proves the exported window was not altered | ✅ Implemented | `batlehub-cli admin export-audit-log`, `batlehub-cli admin audit verify` |
| 5.33 – Protection of records | Security-class rows are removed by their retention and by nothing else — a manual purge reaches access rows only, so a purge's own record survives later purges | ✅ Implemented | `DELETE /api/v1/admin/audit-log?before=`, `[audit] security_retention_days` |
| 5.34 – Privacy and PII | Retention classes, pseudonymisation, erasure and access export | ✅ Implemented | [GDPR mapping](./compliance-gdpr.md) |

## 8 — Technological controls

| Control | BatleHub control | Status | Evidence |
|---------|------------------|--------|----------|
| 8.2 – Privileged access rights | Admin actions need admin verbs; `gdpr:erase` is its own verb, not implied by `audit:purge` | ✅ Implemented | [Access control § verbs](../guide/access-control.md#verbs) |
| 8.5 – Secure authentication | OIDC sign-in; every sign-in, failure, token creation and revocation and refused credential is an audit event | ✅ Implemented | `sign_in`, `sign_in_failed`, `token_create`, `token_revoke`, `credential_rejected` in [SIEM integration](./siem.md#what-is-recorded) |
| 8.5 – MFA | Enforced at your IdP; BatleHub does not yet check the `acr`/`amr` claims | Manual process | [NIS2 and DORA](./compliance-nis2-dora.md) |
| 8.7 – Protection against malware | Scan worker in a sandbox (GuardDog, Trivy, operator-supplied YARA rules); a match is a finding the policy can block on | ✅ Implemented | [Scan worker](./scan-worker.md), `[scanners.yara] rules_dir` in [configuration](../guide/configuration.md#scanners-and-worker) |
| 8.8 – Technical vulnerabilities (yours) | CVE scanning of proxied artifacts; exposure report of who pulled a flagged version | ✅ Implemented | [Incident response § who pulled a flagged version](./incident-response.md#who-pulled-a-flagged-version) |
| 8.8 – Technical vulnerabilities (the project's) | Private reporting, stated targets, advisories with a CVE, VEX | Documented | `SECURITY.md`, [CRA mapping](./compliance-cra.md) |
| 8.9 – Configuration management | Every applied or rejected reload is recorded with its actor and diff; validation warnings are listed | ✅ Implemented | `GET /api/v1/admin/config/changes`, `GET /api/v1/admin/config/warnings` |
| 8.10 – Information deletion | Retention expiry by class; erasure of one subject | ✅ Implemented | `[audit]`, `batlehub-cli admin gdpr erase` |
| 8.11 – Data masking | Access-class rows pseudonymised after a set age | ✅ Implemented | `[audit] pseudonymise_after_days` |
| 8.13 – Information backup | Postgres and object-storage backup and restore runbooks | Documented | [Disaster recovery](./disaster-recovery.md) |
| 8.15 – Logging | Every download, publish, block, grant, purge, config change and authentication event in `access_events`, with user, time, IP and user agent | ✅ Implemented | `GET /api/v1/admin/audit-log` |
| 8.15 – Logs protected from tampering | Signed hash chain over closed windows; truncation detected against the SIEM's last `audit_seal` line | ✅ Implemented | `[audit] seal_interval_secs`, `batlehub-cli admin audit verify --head <digest>`, `deploy/siem/sigma/audit_chain_gap.yml` |
| 8.15 – A copy off the host | The JSON audit stream on stdout, for a collector | ✅ Implemented | `[logging] format = "json"`, [SIEM integration](./siem.md) |
| 8.15 – Write-once storage | Immutable storage for the shipped copy (S3 Object Lock, a WORM bucket) | Manual process | your collector's sink |
| 8.16 – Monitoring activities | Sigma rules over the stream; Prometheus alerts | ✅ Implemented | `deploy/siem/sigma/`, `deploy/prometheus-alerts.yaml` |
| 8.16 – Watching the alerts | Someone reading them | Manual process | — |
| 8.20 – Network security | IP blocking, rate limiting, trusted-proxy rules | ✅ Implemented | `crates/web/src/middleware/ip_block.rs`, [Production hardening](./production-hardening.md) |
| 8.24 – Use of cryptography | Ed25519 publish and seal signatures; HMAC-signed flag pushes; TLS at the ingress | ✅ Implemented | `crates/core/src/services/signature.rs`, `[audit] seal_signing_key` |
| 8.24 – Key management | Holding signing keys in a KMS and rotating them | Manual process | [NIS2 and DORA](./compliance-nis2-dora.md) |
| 8.25–8.28 – Secure development | Required review, clippy with `-D warnings`, CodeQL, Semgrep, gitleaks, fuzz targets | ✅ Implemented | `.github/workflows/codeql.yaml`, `.github/workflows/semgrep.yaml`, `.github/workflows/secret-scan.yaml`, [Change management](./change-management.md) |
| 8.32 – Change management | Pull-request review, migrations, config change log | Documented | [Change management](./change-management.md) |

---

## Gaps

| Gap | Plan |
|-----|------|
| MFA is not checked by BatleHub itself | RFC 0036 phase 7 — [NIS2 and DORA](./compliance-nis2-dora.md) |
| Signing keys are files or environment variables, not KMS-held | RFC 0036 phase 7 |
| `source.ip` is not recorded on admin actions | Follow-up in RFC 0036 §13 |
