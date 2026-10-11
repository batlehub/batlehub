# Compliance

BatleHub sits on the software supply chain of whoever runs it, and that
operator is often audited. This page states which regulatory frameworks the
project is built against, whom each one binds, and what the software
contributes toward it. The position itself, and why, is
[RFC 0036](../rfc/0036-regulatory-alignment.md).

::: warning Software is never certified
**A certificate belongs to an organisation running a service, never to the
software it runs.** No ISO 27001 certificate, SOC 2 report or GDPR
attestation can name BatleHub, and nothing in these pages is a claim that the
project is compliant with anything.

What the software can owe is the *evidence* and the *controls* an auditor asks
the operator for: an audit trail that is complete and cannot be quietly edited,
personal data with a lifecycle, a way to answer a data-subject request, a
vulnerability process with stated targets. The pages below map each of those
to the clause it serves, at the file, setting, route or command that
implements it. Whether a control is enabled, configured and operating in your
deployment is yours to show.

Whom a regulation binds is stated as the project reads it. Confirm it with
counsel before relying on it.
:::

## Whom each framework binds

| Framework | Binds | BatleHub's position | What the project owes |
| --- | --- | --- | --- |
| GDPR (Reg. 2016/679) | the operator, as controller of its users' data | baseline | minimisation and storage limitation by default (Art. 5, 25); access and erasure tooling (Art. 15, 17); the security of processing it offers (Art. 32) |
| ISO/IEC 27001:2022 | the operator's ISMS | baseline | Annex A evidence: logging (8.15), monitoring (8.16), access control (5.15–5.18, 8.2, 8.5), vulnerabilities (8.8), cryptography (8.24), secure development (8.25–8.28), ICT supply chain (5.21) |
| Cyber Resilience Act (Reg. 2024/2847) | a manufacturer placing a product on the EU market in a commercial activity | baseline; **not yet binding** — the project is non-commercial | Annex I Part II vulnerability handling, an SBOM, a support period; the reporting duty of Art. 14 (in force since 2026-09-11) once commercial |
| NIS2 (Dir. 2022/2555) | essential and important entities | future goal | the controls its Art. 21(2) measures name: supply-chain security (d), vulnerability handling (e), access control and MFA (i, j); detection that makes Art. 23's 24 h / 72 h / one-month reporting possible |
| DORA (Reg. 2022/2554) | financial entities, and through contracts their ICT providers | future goal | logging and detection (Art. 9–10), backup and restoration (Art. 12), incident classification (Art. 18); for a hosted instance, the contractual terms of Art. 30 |

A **public instance** is a role of its own: its operator is a GDPR controller
for every visitor's IP address, and — if it serves a regulated client — an ICT
third-party provider under DORA.

## The mapping pages

| Page | Read it when |
| --- | --- |
| [GDPR](./compliance-gdpr.md) | personal data in the audit trail, retention, a data-subject request |
| [ISO/IEC 27001:2022](./compliance-iso27001.md) | your ISMS auditor asks which Annex A control a feature serves |
| [Cyber Resilience Act](./compliance-cra.md) | a supplier questionnaire asks how the project handles vulnerabilities |
| [NIS2 and DORA](./compliance-nis2-dora.md) | you are a regulated entity, or serve one |
| [SOC 2 checklist](./soc2-checklist.md) | your auditor works from the Trust Service Criteria |
| [SIEM integration](./siem.md) | you need the audit stream and its detection rules |

Every page uses the same status words as the SOC 2 checklist: **✅ Implemented**
means the control exists in the code, at the place named in the evidence
column; **Documented** means a written procedure exists; **Manual process**
means a process *you* run, which BatleHub cannot run for you. The NIS2 and
DORA page adds **Not built** for what is planned and does not exist yet.

## Turning the controls on

Most of the evidence above is off until it is configured, because an upgrade
must never delete data the operator did not ask it to:

```toml
[logging]
format = "json"                   # the audit stream, see siem.md

[audit]
access_retention_days   = 365     # downloads and metadata reads
security_retention_days = 1095    # sign-ins, grants, blocks, purges, config
pseudonymise_after_days = 30      # access rows: IP truncated, user agent dropped
seal_interval_secs      = 300     # hash-chained, signed windows
seal_signing_key        = "${BATLEHUB_AUDIT_SEAL_KEY}"
erasure_key             = "${BATLEHUB_AUDIT_ERASURE_KEY}"
```

With no `[audit]` table nothing expires, nothing is pseudonymised and nothing
is sealed, and the server logs one warning at start saying so. The values
above are the documented example, not a default; pick yours with whoever owns
your retention schedule.
