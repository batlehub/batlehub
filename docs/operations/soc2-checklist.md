# SOC 2 Trust Service Criteria — BatleHub Controls

This document maps each relevant SOC 2 Trust Service Criterion (TSC) to the controls implemented in BatleHub.

::: warning A mapping, not an audit result
**BatleHub has not been audited, and software cannot be.** The certified entity
in any SOC 2 report is the organisation running a service, not the software it
runs — so this page exists to give *your* auditor somewhere to start, and
nothing on it is a claim about the project's compliance status.

"✅ Implemented" below means exactly one thing: the control exists in the code,
at the file or endpoint named in the Evidence column. It says nothing about
whether it is enabled in your deployment, configured correctly, monitored, or
operating effectively over a period — which is the whole of what a Type II audit
examines. Several rows say "Manual process", and those are processes *you* would
have to run.

Use this as evidence to submit and as a gap list to work from. Do not cite it as
a result.
:::

**Scope**: BatleHub proxy-cache server (package proxy, local registry, admin API).

---

## CC6 — Logical and Physical Access Controls

| Criterion | Control | Status | Evidence |
|-----------|---------|--------|---------|
| CC6.1 – Protect logical access credentials | API tokens are SHA-256 hashed before DB storage; plaintext never persisted | ✅ Implemented | `crates/adapters/src/db/postgres/user_tokens.rs` |
| CC6.1 – Token expiry | `expires_at` enforced on every API call | ✅ Implemented | `crates/adapters/src/auth/user_token.rs` |
| CC6.1 – Token revocation | `revoked_at` soft-delete; revoked tokens rejected immediately | ✅ Implemented | `DELETE /api/v1/auth/tokens/{id}` |
| CC6.2 – Role-based access | Anonymous / User / Admin roles with RBAC policy rules per registry | ✅ Implemented | `crates/core/src/rules/rbac.rs` |
| CC6.2 – Group-based access | OIDC group claims mapped to per-registry resource grants | ✅ Implemented | `RbacRule::with_groups()` |
| CC6.3 – Remove access | Token revocation API; user-block API disables all requests from a user | ✅ Implemented | `POST /api/v1/admin/users/{id}/block` |
| CC6.6 – Network access restriction | IP allowlist/blocklist enforced in request middleware | ✅ Implemented | `crates/web/src/middleware/ip_block.rs` |
| CC6.7 – Transmission encryption | TLS terminated at load balancer; internal requests use HTTPS clients | Manual process | Deploy with TLS termination |
| CC6.8 – Prevent unauthorized software | Registry type allowlist in config; SBOM generation and vuln scanning | ✅ Implemented | `docs/contributing/security-scanning.md` |

---

## CC7 — System Operations

| Criterion | Control | Status | Evidence |
|-----------|---------|--------|---------|
| CC7.1 – Detect configuration changes | Config change log stored in `config_changes` table | ✅ Implemented | `GET /api/v1/admin/config/changes` |
| CC7.2 – Monitor for anomalies | Rate limiting per IP and per user; anomaly counters in Prometheus | ✅ Implemented | [`[otel]` configuration](/guide/configuration#_3-7-otel-optional), `deploy/prometheus-alerts.yaml` |
| CC7.3 – Evaluate security events | Audit log captures every download, block, unblock, delete (by hand and by retention policy) with user, timestamp, IP; filterable by action | ✅ Implemented | `GET /api/v1/admin/audit-log?action=…` |
| CC7.3 – IP/UA in audit log | ip_address and user_agent columns in access_events (migration 029) | ✅ Implemented | `crates/adapters/migrations/029_audit_ip_ua.sql` |
| CC7.4 – Respond to security incidents | See `docs/operations/incident-response.md` | Manual process | Documented |
| CC7.5 – Disclose security incidents | Incident response playbook includes notification steps | Manual process | `docs/operations/incident-response.md` |

---

## CC8 — Change Management

| Criterion | Control | Status | Evidence |
|-----------|---------|--------|---------|
| CC8.1 – Authorise changes | Pull request review required (GitHub/Forgejo branch protection) | Manual process | `docs/operations/change-management.md` |
| CC8.1 – Config changes tracked | All admin config changes stored in `config_changes` table with identity | ✅ Implemented | `GET /api/v1/admin/config/changes` |
| CC8.1 – Dependency updates | `cargo audit`, `cargo deny`, `pnpm audit` gates in CI | ✅ Implemented | `.github/workflows/back-dep-audit.yaml` |

---

## CC9 — Risk Mitigation

| Criterion | Control | Status | Evidence |
|-----------|---------|--------|---------|
| CC9.1 – Identify risks | CVE scanning via `cargo audit` + Trivy + OSV | ✅ Implemented | `docs/contributing/security-scanning.md` |
| CC9.2 – Vendor risk | SBOM generated per release (CycloneDX); supply-chain scanning via socket.dev badge | ✅ Implemented | `GET /api/v1/sbom/export` |

---

## A1 — Availability

| Criterion | Control | Status | Evidence |
|-----------|---------|--------|---------|
| A1.1 – Current processing capacity | Prometheus metrics + Grafana dashboard; capacity planning in docs | ✅ Implemented | `deploy/grafana/batlehub-production.json`, `docs/guide/configuration.md` |
| A1.2 – Environmental protections | Health endpoint; Prometheus alert for `BatleHubDown` | ✅ Implemented | `GET /healthz`, `GET /api/v1/admin/health`, `deploy/prometheus-alerts.yaml` |
| A1.3 – Backup and recovery | Postgres pg_dump + S3 rclone sync; restore runbooks | Documented | `docs/operations/disaster-recovery.md` |

---

## Compliance Export

The audit log can be exported for auditors via:

```bash
# JSON export (last 30 days)
batlehub admin export-audit-log --from 2026-06-01T00:00:00Z --format json --output audit.json

# CSV export (for spreadsheet review)
batlehub admin export-audit-log --from 2026-06-01T00:00:00Z --format csv --output audit.csv
```

Or via the Admin UI → **Audit Log** → **Export** button.

---

## Gaps / Remediation Status

| Gap | Priority | Plan |
|-----|----------|------|
| TLS enforcement not configured by BatleHub itself | Low | Document TLS termination requirement in deployment guide |
| IP/UA extraction not yet wired into proxy_stream callers | Medium | Thread HttpRequest through proxy handlers (planned next sprint) |
| Incident response runbook not yet tested | Medium | Schedule tabletop exercise |
