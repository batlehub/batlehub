# NIS2 and DORA — what exists, what is planned

NIS2 (Dir. 2022/2555) binds essential and important entities; DORA (Reg.
2022/2554) binds financial entities and, through their contracts, the ICT
providers they rely on. Neither binds software, and no regulated operator runs
BatleHub yet, so the project treats both as a **future goal**: this page lists
what already serves their measures and what RFC 0036's last phase would add
when the first regulated operator needs it.

::: warning Not yet a target the project meets
The rows under *What phase 7 adds* are **not built**. They are listed so that a
regulated operator can see the gap before adopting BatleHub, and so the
baseline (GDPR, ISO 27001, the CRA) does not have to be rebuilt to reach them.
"✅ Implemented" means the control exists in the code, at the place named in
the Evidence column. See [Compliance](./compliance.md).
:::

---

## NIS2 — Art. 21(2) risk-management measures

| Measure | BatleHub control | Status | Evidence |
|---------|------------------|--------|----------|
| (b) – Incident handling | Incident playbook; audit stream and Sigma rules for detection | Documented | [Incident response](./incident-response.md), [SIEM integration](./siem.md) |
| (c) – Business continuity, backup | Backup and restore runbooks; stateless replicas | Documented | [Disaster recovery](./disaster-recovery.md), [High availability](../guide/high-availability.md) |
| (d) – Supply-chain security | Scanning, SOC flags, verdicts and release-age gates on every proxied artifact; an exposure report of who pulled a flagged version | ✅ Implemented | [Scan worker](./scan-worker.md), `batlehub-cli admin exposure` |
| (e) – Vulnerability handling and disclosure | The project's private reporting, targets and advisories | Documented | `SECURITY.md`, [CRA mapping](./compliance-cra.md) |
| (h) – Cryptography | Ed25519 signatures on publishes and on the audit seal chain | ✅ Implemented | `crates/core/src/services/signature.rs`, `[audit] seal_signing_key` |
| (i) – Access control | Grants and verbs; every authentication event audited | ✅ Implemented | [Access control](../guide/access-control.md), [ISO 27001 mapping](./compliance-iso27001.md) |
| (j) – Multi-factor authentication | Enforced at your IdP; BatleHub does not check it | Manual process | phase 7 below |

## NIS2 — Art. 23 reporting

Art. 23 asks an entity for an early warning within 24 hours of becoming aware
of a significant incident, a notification within 72 hours and a final report
within one month. The reporting is the operator's; what BatleHub supplies is
the evidence the reports are written from.

| Obligation | BatleHub control | Status | Evidence |
|------------|------------------|--------|----------|
| Becoming aware | Sigma rules over the audit stream; Prometheus alerts | ✅ Implemented | `deploy/siem/sigma/`, `deploy/prometheus-alerts.yaml` |
| The facts of the incident | Audit log for the window, exported | ✅ Implemented | `batlehub-cli admin export-audit-log --from <start> --to <end>` |
| The facts were not altered | The seal chain verifies for the window, and its head matches the SIEM's copy | ✅ Implemented | `batlehub-cli admin audit verify --from <start> --to <end> --head <digest>` |
| The reports themselves | 24 h / 72 h / one month | Manual process | [Incident response § NIS2 Art. 23](./incident-response.md#nis2-reporting) |

## DORA — Art. 9–12, 18, 30

| Article | BatleHub control | Status | Evidence |
|---------|------------------|--------|----------|
| 9 – Protection and prevention | Access control, IP blocking, rate limiting, hashed tokens | ✅ Implemented | [Production hardening](./production-hardening.md) |
| 10 – Detection | Audit stream, Sigma rules, Prometheus alerts | ✅ Implemented | [SIEM integration](./siem.md) |
| 11 – Response and recovery | Incident playbook | Documented | [Incident response](./incident-response.md) |
| 12 – Backup and restoration | Backup and restore runbooks | Documented | [Disaster recovery](./disaster-recovery.md) |
| 12 – Restoration tested, with an RPO/RTO | — | Not built | phase 7 below |
| 18 – Incident classification | — | Not built | phase 7 below |
| 30 – Contractual terms with an ICT provider | — | Not built | phase 7 below |

---

## What phase 7 adds — not yet built

[RFC 0036 §12](../rfc/0036-regulatory-alignment.md#_12-implementation-phases)
names these, to be built **when a regulated operator needs them**. None exists
today.

| Addition | Serves | What it would be |
|----------|--------|------------------|
| MFA enforcement | NIS2 21(2)(j), DORA 9 | A provider setting that refuses a sign-in whose OIDC `acr` / `amr` claims do not show a second factor |
| KMS-held signing keys | NIS2 21(2)(h), DORA 9 | The publish, APK, VS Code and seal signing keys held in a KMS, with a written rotation procedure |
| Restore test in CI | DORA 12 | A scheduled restore of a backup into a fresh instance, with a stated RPO and RTO it is measured against |
| Incident classification | DORA 18, NIS2 23 | Classification fields (severity, affected services, data impact) on notifications |
| Art. 30 contract annex | DORA 30 | A template annex for a hosted offer: service levels, audit and access rights, exit, sub-contracting |

Until then, a regulated operator covers these with its own controls: MFA at the
IdP, keys in its own secret store, its own restore drills and its own
classification in the ticket.
