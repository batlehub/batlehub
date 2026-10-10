# Cyber Resilience Act — BatleHub controls

This page maps the parts of the Cyber Resilience Act (Reg. 2024/2847) that
concern a software product's manufacturer to what the BatleHub project does.
Unlike the other mapping pages, most rows here are owed by the **project**,
not by the operator.

::: warning Not yet binding
**The CRA binds a manufacturer placing a product on the EU market in the
course of a commercial activity.** BatleHub is non-commercial free software
today, so the project reads it as outside the Act's scope. The day BatleHub is
offered with paid support or monetised another way, the maintainer becomes a
manufacturer and the rows below apply from the first commercial release; they
are built now so that day is not a retrofit. An organisation that integrates
BatleHub into a product it sells is itself that product's manufacturer.

"✅ Implemented" means the control exists in the code or the repository, at
the place named in the Evidence column. See [Compliance](./compliance.md).
:::

**Scope**: the BatleHub repository, its release pipeline and its vulnerability
process.

---

## Annex I, Part II — Vulnerability handling

| Requirement | Control | Status | Evidence |
|-------------|---------|--------|----------|
| II(1) – Identify components; an SBOM | A CycloneDX SBOM for the Rust workspace and for the image, attached and attested on every release | ✅ Implemented | `.github/workflows/build.yaml` |
| II(2) – Remediate without delay | Stated targets: acknowledgement in 5 working days, assessment in 10, critical fix in 30 days, high in 60 | Documented | `SECURITY.md` § What happens next |
| II(3) – Regular tests and reviews | Dependency, container, SAST and secret gates on every pull request and daily | ✅ Implemented | `.github/workflows/back-dep-audit.yaml`, `.github/workflows/image-scan.yaml`, `.github/workflows/codeql.yaml`, [Vulnerability scanning](../contributing/security-scanning.md) |
| II(4) – Disclose fixed vulnerabilities | A GitHub security advisory with a CVE, a `### Security` changelog entry and a VEX statement, in the same release | Documented | [Publishing an advisory](../contributing/security-scanning.md#advisory), `CHANGELOG.md`, `vex/batlehub.openvex.json` |
| II(5) – Coordinated disclosure policy | 90 days from report, or at the fix | Documented | `SECURITY.md` § Disclosure |
| II(6) – A contact address for reports | GitHub private vulnerability reporting; the public issue template was removed | ✅ Implemented | `SECURITY.md` § Reporting a vulnerability, `.github/ISSUE_TEMPLATE/config.yml` |
| II(7) – Distribute updates securely | Release images built in CI with attested SBOMs | ✅ Implemented | `.github/workflows/build.yaml` |
| II(8) – Updates free of charge, with advisories | Every fix ships in a public release with its advisory | Documented | `SECURITY.md` § Supported versions |

## Annex I, Part I — Essential cybersecurity requirements

| Requirement | Control | Status | Evidence |
|-------------|---------|--------|----------|
| I(2)(a) – No known exploitable vulnerability at release | `cargo audit`, `cargo deny` and `pnpm audit` gate the build; `advisories.ignore = []` | ✅ Implemented | `deny.toml`, `task security` |
| I(2)(b) – Secure by default | A missing retention, an unsigned seal chain, sealing without a stream and similar half-configurations are rejected or warned about at start | ✅ Implemented | `GET /api/v1/admin/config/warnings` |
| I(2)(d) – Protection from unauthorised access | Authentication providers, grants, IP blocking, rate limiting; every refusal is an audit event | ✅ Implemented | [Access control](../guide/access-control.md), [SIEM integration](./siem.md) |
| I(2)(e) – Confidentiality | TLS terminated at the ingress; tokens stored hashed | Manual process | [Production hardening](./production-hardening.md) |
| I(2)(f) – Integrity | Artifact checksums verified on cache write; publish signatures; a signed chain over the audit trail | ✅ Implemented | `crates/core/src/services/integrity.rs`, `crates/core/src/services/signature.rs`, `batlehub-cli admin audit verify` |
| I(2)(g) – Data minimisation | Audit retention classes and pseudonymisation | ✅ Implemented | [GDPR mapping](./compliance-gdpr.md) |
| I(2)(l) – Security monitoring | Audit events for every security-relevant action, a JSON stream and Sigma rules | ✅ Implemented | `[logging] format = "json"`, `deploy/siem/sigma/` |
| I(2)(m) – Secure data removal | Erasure of one data subject; retention expiry | ✅ Implemented | `batlehub-cli admin gdpr erase` |

## Articles 13 and 14 — Manufacturer's obligations

| Article | Control | Status | Evidence |
|---------|---------|--------|----------|
| 13(8) – Support period | The latest published release receives security fixes; no LTS branch. A paid offer would state its support period here | Documented | `SECURITY.md` § Supported versions |
| 13(2) – Cybersecurity risk assessment | A written assessment of the product | Manual process | not written; owed at the first commercial release |
| 14 – Report actively exploited vulnerabilities (24 h early warning to the CSIRT and ENISA) | Reporting through the single reporting platform | Manual process | owed at the first commercial release |
| Annex II – Information for users | Installation, configuration, hardening and the vulnerability contact | Documented | [Installation](../guide/installation.md), [Production hardening](./production-hardening.md), `SECURITY.md` |

---

## Gaps

| Gap | Plan |
|-----|------|
| No written cybersecurity risk assessment | At the first commercial release |
| No Art. 14 reporting procedure | At the first commercial release; it reuses the advisory process above |
| No `security.txt` | The docs site is served under a path on a host the project does not own; `SECURITY.md` is the policy until the project has its own domain (RFC 0036 §6.5) |
