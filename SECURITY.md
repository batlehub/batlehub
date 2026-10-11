# Security Policy

## Reporting a vulnerability

Report it **privately**, through GitHub's private vulnerability reporting:

**<https://github.com/batlehub/batlehub/security/advisories/new>**

Only the maintainers can read what you send there. Please do not report a
vulnerability in a public issue, a discussion, a pull request or a chat
channel: anything filed there is visible to everyone the moment it is posted,
before a fix exists.

Include, as far as you can:

- what is affected — the component (a registry adapter, the auth middleware,
  the grants engine, the CLI, the web console, the Helm chart) and the
  version or commit;
- the impact, and the conditions it needs (configuration, role, network
  position);
- steps to reproduce, or a proof of concept;
- whether you intend to publish, and when.

## What happens next

BatleHub is maintained by a single developer, so these are **targets**, not a
contractual SLA. They are what you can expect, and a report that misses them
will be told why.

| Step | Target |
| --- | --- |
| Acknowledgement of your report | 5 working days |
| First assessment: confirmed or not, and a severity (CVSS) | 10 working days |
| Fix released — critical | 30 days from confirmation |
| Fix released — high | 60 days from confirmation |
| Fix released — medium and low | the next scheduled release |

You are kept informed through the private advisory as the fix progresses, and
credited in the advisory unless you ask not to be.

## Disclosure

Disclosure is coordinated. The advisory is published when the fixed release
ships, or **90 days after the report** if no fix has shipped by then —
earlier if the vulnerability is being actively exploited, later only by
agreement with you.

Every published vulnerability gets, in the same release:

- a **GitHub Security Advisory**, with a **CVE** requested through GitHub
  (a CVE Numbering Authority);
- an entry under `### Security` in [`CHANGELOG.md`](CHANGELOG.md);
- an update to [`vex/batlehub.openvex.json`](vex/batlehub.openvex.json), so a
  scanner reading the project's VEX document agrees with the advisory.

## Supported versions

| Version | Security fixes |
| --- | --- |
| The latest published release | Yes |
| Any earlier release, including earlier patches of the current minor | No |

There is no long-term-support branch and no backport guarantee. Upgrading to
the latest release is how a fix is picked up. If BatleHub is ever offered
with paid support, the support period that comes with it will be stated here.

## Scope

In scope: the BatleHub server, the `batlehub` CLI, the web console, the Helm
chart in `helm/batlehub`, the container images and the release artifacts
published from this repository.

Out of scope:

- the content of the upstream registries BatleHub proxies — report a
  malicious package to the registry that hosts it;
- a deployment's own configuration, unless BatleHub's defaults or
  documentation led to it (that part is in scope);
- volumetric denial of service;
- output of an automated scanner with no demonstrated impact.

## Safe harbour

Research done in good faith — within scope, without accessing or modifying
other people's data, without degrading a service for others, and reported
through the channel above — will not be the subject of a complaint from the
project. Test against your own instance, never against someone else's.

## How the project checks itself

Vulnerability scanning of BatleHub's own dependencies, container images and
source is continuous; see
[`docs/contributing/security-scanning.md`](docs/contributing/security-scanning.md)
for the full matrix (`cargo audit`, `cargo deny`, `pnpm audit`, CodeQL,
Semgrep, gitleaks, Trivy) and for how a published advisory reaches the VEX
document. Releases carry a CycloneDX SBOM, SLSA build provenance and a
keyless Sigstore signature.
