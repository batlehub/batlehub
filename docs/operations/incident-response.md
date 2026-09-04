# Incident Response Playbook — BatleHub

This playbook covers security and availability incidents for BatleHub deployments. Follow the phases in order.

::: warning Guidance, not a commitment
A template for the playbook *you* write, not a service the project runs. The
severity levels, response times and notification recipients below are examples —
replace them with your organisation's, because the only people who can meet them
are yours. Nobody is paged by this page.
:::

---

## Severity Levels

| Level | Definition | Response SLA |
|-------|-----------|-------------|
| **P0 — Critical** | Data breach, token compromise, registry unavailable >15 min | Respond immediately (24/7) |
| **P1 — High** | Unauthorized access to admin endpoints, persistent DoS | Within 2 hours |
| **P2 — Medium** | Anomalous access patterns, failed-but-caught attack | Within 8 hours |
| **P3 — Low** | Policy misconfiguration, expired token alerts | Next business day |

---

## Phase 1 — Detection

### Automated signals

| Source | Alert | Where to check |
|--------|-------|----------------|
| Prometheus | `BatleHubDown`, `BatleHubHighErrorRate`, `BatleHubHighDenyRate` | `deploy/prometheus-alerts.yaml` |
| Audit log | Spike in `denied` outcomes, unknown user IDs in `user_id` column | `GET /api/v1/admin/audit-log` |
| Rate limiter | Sustained 429 responses from one IP | Audit log filter by IP |
| Container scan | Trivy HIGH/CRITICAL finding in deployed image | `.github/workflows/image-scan.yaml` |
| Gitleaks | Secret exposed in commit | `.github/workflows/secret-scan.yaml` |

### Manual detection

Query the audit log for anomalies:

```bash
# All denied events in the last hour
batlehub admin audit-log --denied-only --from "$(date -u -d '1 hour ago' +%FT%TZ)"

# Access events from a specific IP
batlehub admin audit-log | jq '[.[] | select(.ip_address == "1.2.3.4")]'

# What disappeared, and whether a person or a policy took it
batlehub admin audit-log --action delete,retention_reclaim --from <start>

# Export a full 24-hour window for offline analysis
batlehub admin export-audit-log --from <start> --to <end> --format csv --output incident-$(date +%Y%m%d).csv
```

---

## Phase 2 — Containment

Act within the first 15 minutes for P0/P1. Do not wait for root-cause analysis before containing.

### Block a suspicious IP

```bash
batlehub admin ip-blocks add 1.2.3.4
```

Or via the Admin UI → **IP Blocks** → Add.

### Revoke a compromised token

```bash
batlehub admin token revoke <token-id>
```

Or via the Admin UI → **Users** → select user → Revoke token.

### Block a user account

```bash
batlehub admin users block <user-id>
```

Blocks all future requests from that user ID until unblocked.

### Quarantine a malicious package

```bash
# Block the package name across all versions
batlehub admin block <registry> <package-name>

# Or target a specific version
batlehub admin packages unlist <registry> <package-name> <version>
```

### Flag a version from your SOC tooling

A `[[flag_sources]]` entry ([configuration §3.11](/guide/configuration#flag-sources))
lets the SOC platform push the verdict itself, signed, without a console
login — and the push is the record the exposure report reads.

```bash
BODY='{"flags":[{"external_id":"CASE-2026-0912","registry":"npm","package_name":"left-pad",
  "version":"1.3.1","kind":"malware","effect":"hard_block","summary":"credential stealer in postinstall"}]}'
SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$FLAG_SECRET" | sed 's/^.* //')"
curl -sS -X POST "$BATLEHUB/api/v1/flags/soc" \
  -H "Content-Type: application/json" -H "X-Hub-Signature-256: $SIG" --data "$BODY"
```

`version_range = "*"` flags every version of the package. On a registry with
a `[registries.security]` profile the version is denied at once; elsewhere
`FlagsRule` refuses it on the next request. Lift it with
`DELETE /api/v1/flags/soc/CASE-2026-0912` (signed over the empty body): the
row stays as a tombstone so the report below still answers.

### Who pulled a flagged version {#who-pulled-a-flagged-version}

The exposure report joins the access log to the flags, one row per consumer,
coordinate and flag, and says how many of the pulls **preceded** the flag —
the retroactive case, where the developer did nothing wrong and still has
the artifact.

```bash
batlehub admin exposure --source soc --min-effect gate --when before-flag
batlehub admin exposure --registry npm --package left-pad --from 2026-09-01T00:00:00Z
batlehub admin flags list --include-dead          # what every source has said, tombstones too
```

`GET /api/v1/admin/exposure/export?format=csv` is the same rows as a file for
the ticket. Read the **coverage** block before quoting the numbers: it says
how many registries have an SBOM extractor (so the CVE scan reaches them),
when that scan last ran per registry, and which sources have pushed — a
report that has never scanned knows only what was pushed.

### Isolate a replica

If one replica is compromised, remove it from the load balancer before forensics. BatleHub state lives in Postgres and S3 — the replica itself is stateless.

---

## Phase 3 — Eradication

1. **Rotate credentials** — generate new API tokens for all service accounts; update downstream consumers.
2. **Patch the vulnerability** — if a CVE triggered the incident, apply the patch, run `cargo audit` locally, and rebuild the image.
3. **Re-run SBOM scan** — `task security` to confirm the patched dependency tree is clean.
4. **Review RBAC rules** — use the RBAC simulator (`POST /api/v1/admin/access-check`) to validate that the affected policy gap is closed.

---

## Phase 4 — Recovery

1. **Deploy patched image** — run `cargo build --release` or trigger CI; push patched container.
2. **Verify health** — `GET /api/v1/health` returns `200` on all replicas.
3. **Unblock legitimate traffic** — remove IP blocks and unblock users that were collateral.
4. **Monitor** — watch Prometheus for 30 minutes after restoration; confirm error rate returns to baseline.

---

## Phase 5 — Post-Mortem

Within 5 business days of a P0/P1 incident:

1. Write a timeline (detection → containment → eradication → recovery).
2. Identify root cause and contributing factors.
3. List corrective actions with owners and due dates.
4. Update this playbook if any step was missing or unclear.
5. Archive the exported audit log for the incident period (`export-audit-log --format csv`).

Post-mortem template: none yet. Write the first one against the five points
above and keep it under `operations/` so the next incident starts from a form
rather than from a blank page.

---

## PII Handling

Audit log entries contain user IDs and IP addresses. If a GDPR/CCPA deletion request arrives:

1. Identify the user's ID from their account.
2. Export their records: `export-audit-log | jq '[.[] | select(.user_id == "X")]'`
3. Provide a copy to the user if required by your jurisdiction.
4. To purge from the database, run the anonymization migration (planned feature) or a targeted `UPDATE access_events SET user_id = 'anonymized', ip_address = NULL WHERE user_id = 'X'` with DBA oversight.

---

## Contacts

Populate these before deploying to production:

| Role | Contact |
|------|---------|
| On-call engineer | — |
| Security lead | — |
| Legal / DPO | — |
| Upstream registry contacts (GitHub, npm, PyPI) | — |
