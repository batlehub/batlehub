# Vulnerability scanning & SBOMs

batlehub is scanned for CVEs continuously, across every layer it ships. This page describes the
layers, how to reproduce them locally, and how to match a **future-disclosed** CVE against a build
you have already deployed.

## Layers

| Layer | Tool | Where it runs | Gate |
| --- | --- | --- | --- |
| Rust advisories | `cargo audit` (RUSTSEC) | `back-dep-audit.yaml` (PR + daily) | block |
| Rust advisories + bans + licenses + sources | `cargo deny` (`deny.toml`) | `back-dep-audit.yaml` | block |
| JS dependencies | `pnpm audit --audit-level high` | `dep-audit-frontend.yaml` (PR + daily) | block on high/critical |
| Dependency supply chain (reputation + vulns) | [postmortem](https://github.com/mlab-sh/postmortem) | `postmortem.yaml` (PR + daily) — one job per dependency root: Rust, UI, Website | block on high/critical vulns (Rust, UI); report-only (Website) |
| Container / OS layers | Trivy | `image-scan.yaml` (PR + daily, GitHub) runs Trivy directly on the proxy image (`Containerfile`), the worker image (`Containerfile.worker`, RFC 0018 — the one that carries bubblewrap, postmortem and the Trivy client) and the GuardDog variant of the worker image (`Containerfile.worker-guarddog`, which adds the optional Python scanner and is gated separately, so its toolchain answers for its own CVEs); `.forgejo/workflows/build.yaml` (the proxy and hardened images, the two it builds) polls Harbor's own scan-on-push report instead | block on fixable HIGH/CRITICAL |
| Static analysis | CodeQL + Semgrep | `codeql.yaml`, `semgrep.yaml` | CodeQL report / Semgrep block on ERROR |
| Secrets | gitleaks | `secret-scan.yaml` (PR + push) | block |
| Lint / unsafe hygiene | clippy `-D warnings` | `test.yaml` `lint` job | block |

The **daily** schedules are what turn this from a build-time snapshot into *future* CVE detection: a
CVE disclosed against a pinned dependency or a base-image layer **after** the last commit still trips
CI the next morning, with nothing in the repo having changed.

### postmortem (dependency supply chain)

`cargo audit` and `pnpm audit` only know about *published advisories*. postmortem covers the other
shape of supply-chain risk: it rebuilds the dependency forest from the committed lockfiles (no
install, no lifecycle scripts), resolves each dependency to its source repository, and scores it on
reputation and provenance signals — stars, age, last activity, archived/abandoned, unresolvable or
typosquatted names — on top of a vulnerability cross-check against `vuln.mlab.sh` (OSV/GHSA/CVE).

Its `detect()` only looks at the lockfiles sitting **directly** in the scanned directory, so
`postmortem.yaml` runs one job per dependency root — `.` (`Cargo.lock`), `ui/`, `website/` — each
uploading its own SARIF.

Those are three **separate jobs, not a matrix**, and deliberately so. postmortem stamps every SARIF
result with `artifactLocation.uri = "."` regardless of what it scanned, so the three uploads are
distinguishable only by their Code Scanning category — which `upload-sarif` derives from `GITHUB_JOB`
plus its `matrix` input, and that input's <code v-pre>${{ toJson(matrix) }}</code> default does not resolve when the
upload runs from inside a composite action, as it does here. Matrix legs would therefore share one
category and supersede one another, leaving only the last job's alerts. Distinct job ids cannot
collide. The same quirk means every alert is anchored at the repository root rather than at the
offending lockfile; read the alert message (`<pkg>@<version> [<repo>] — <signal>`) for the target.

The scan runs with `soft-fail` on so the action always reaches its SARIF upload; a separate `Gate`
step then fails the job on the scan's exit code (`0` pass, `1` gate tripped, `2` misconfigured / no
ecosystem detected) — the same report-then-gate shape as `semgrep.yaml`. The reputation thresholds
(`max-risk` / `max-dep` / `max-high` / `max-sus`) are intentionally left unset for now: scores are
reported in SARIF, and the numbers should be picked from a real baseline rather than guessed.

One caveat when reading its vulnerability list: `vuln.mlab.sh` also serves advisories that RustSec has
since **withdrawn** — `RUSTSEC-2020-0053` (`dirs` unmaintained, withdrawn 2021) and `RUSTSEC-2025-0007`
(`ring` unmaintained, withdrawn 2025) both still show up against this tree, and neither is actionable.
Check `withdrawn` on the OSV record (`https://api.osv.dev/v1/vulns/<id>`) before chasing one. Don't
allowlist them — the suppression stance below applies, and the count is harmless as long as the gate
keys on severity.

Two rate limits apply, both optional to raise. Repository reputation is read from the GitHub API —
CI passes `github.token` automatically; locally, export `GITHUB_TOKEN` or every dependency comes back
`stats-failed`. Vulnerability lookups go to `vuln.mlab.sh`, capped at **8 scans/hour anonymously**
while this workflow spends 3 per run, so set the optional `VULN_MLAB_TOKEN` repository secret (and
`VULN_MLAB_TOKEN` in your shell, or `vuln_token` in `~/.postmortem/config.yml`) if runs start getting
throttled.

### Harbor scan-on-push (Forgejo build)

`registry.batleforc.fr` (Harbor) is configured to automatically scan every artifact pushed to the
`batleforc/batlehub*` repositories and to generate its own SBOM accessory, so
`.forgejo/workflows/build.yaml` doesn't run a second Trivy/Syft pass after pushing. Instead it polls
Harbor's API for the vulnerability report of the digest it just pushed, for up to ~1 minute, and
fails the job if Harbor reports a fixable HIGH/CRITICAL CVE — the same gate as before, just sourced
from Harbor instead of a local `trivy image` run. If Harbor hasn't finished scanning within that
minute, the job logs a warning and continues **without** a gate for that run (no local fallback
scan); the daily `image-scan.yaml` run on GitHub remains the backstop for that layer.

## Run the gate locally

```bash
task security        # cargo audit + cargo deny + ui/website pnpm audit + postmortem + Rust SBOM
task deny            # just the cargo-deny supply-chain gate
task audit           # just cargo audit
task ui:audit        # just the frontend audit
task postmortem      # dependency supply chain, all three roots (Rust, UI, Website)
task postmortem:rust # just one root — also postmortem:ui / postmortem:website
```

`postmortem` is provisioned by `mise install`, pinned to the same version the CI action runs.

Image scanning, secret scanning and SAST need their own tools (all provisioned by `mise install`):

```bash
# Build and scan the container image exactly as CI does
podman build -f Containerfile -t batlehub:scan .
trivy image --severity HIGH,CRITICAL --ignore-unfixed batlehub:scan

gitleaks detect --config gitleaks.toml          # secret scan
semgrep scan --config p/rust --config p/typescript
```

## SBOMs — matching a *future* CVE against a shipped build

Every release publishes two CycloneDX SBOMs:

- `sbom-rust.cdx.json` — the shipped server's Rust dependency closure (crate-level), attached to the
  GitHub release.
- `sbom-image.cdx.json` — the full container image (OS packages + binaries), attached to the
  release **and** pushed to the registry as an attestation (`actions/attest-sbom`).

When a new CVE is disclosed months later, you don't need to rebuild to know whether a deployed
version is affected — scan its SBOM:

```bash
# Match the latest advisory DB against an already-shipped SBOM
trivy sbom sbom-image.cdx.json
trivy sbom sbom-rust.cdx.json

# Or with grype
grype sbom:sbom-image.cdx.json
```

Verify the image SBOM/provenance attestation before trusting it:

```bash
gh attestation verify oci://ghcr.io/<owner>/batlehub:<version> --owner <owner>
```

## Scanning *proxied* artifacts at runtime

The layers above scan **batlehub itself**. Separately, batlehub can continuously re-check the
**packages it proxies/hosts** against newly disclosed CVEs, using the per-artifact SBOMs it already
stores (see [SBOM support](/guide/sbom)).

Enable the background task globally:

```toml
[vulnerability_scan]
enabled       = true
interval_secs = 86400                  # re-scan cadence (default: daily)
osv_api_url   = "https://api.osv.dev"  # optional; defaults to the public OSV API
batch_size    = 100
```

Each run pages through every stored CycloneDX SBOM, queries the [OSV](https://osv.dev) database for
the components' PURLs, and records findings. Findings appear per-version in the Package Explorer and
the admin package detail view. Like the daily CI schedules, this turns a one-time cache into *future*
CVE detection: a vulnerability disclosed against a cached package after it was proxied surfaces on the
next scan.

To act on findings, add a `cve_gate` rule to a registry. Warn-only (the default) surfaces the finding
without blocking; `block = true` denies downloads of affected versions at or above `min_severity`:

```toml
[[registries.rules]]
kind         = "cve_gate"
min_severity = "high"        # unknown | low | medium | high | critical
block        = true
bypass_roles = ["admin"]
```

See [Adding a vulnerability scanner source](/contributing/adding-a-vulnerability-scanner) for the API
requirements and checklist when integrating another CVE database alongside OSV.

## Suppressions

The stance is **no suppressions**: `.cargo/audit.toml` and `deny.toml` (`advisories.ignore = []`)
both keep the ignore list empty. If an advisory is genuinely non-actionable, prefer upgrading or
patching the dependency; only add an ignore with an inline justification and a tracking issue.

The hard case is a transitive advisory with no version to upgrade *to*, and there is a worked
example in the tree. RUSTSEC-2026-0258 (`h2`, unbounded empty DATA frames) is fixed in h2 0.4.16 —
already present for the hyper/reqwest path — but `actix-http` still requires the 0.3 line and no
0.3 backport exists, so no `cargo update` could resolve it. It was closed by removing the
*feature* that wanted the crate: `actix-web` is declared `default-features = false` without
`http2`, which drops `h2 0.3` from the tree entirely. Two things make that safe to rely on rather
than rediscover — the reasoning lives next to the declaration in `Cargo.toml`, and `h2 <0.4` is in
`deny.toml`'s `[bans].deny`, so re-enabling the feature fails CI instead of silently restoring the
advisory. Check whether a feature can be dropped before concluding an advisory is unfixable.

### An advisory inside a third party's binary

`.trivyignore.yaml` is the third case: a CVE in a dependency of a **prebuilt binary the image
copies in**, where the fix is neither an upgrade of ours nor a feature we can drop. The worked
example is the pair of grpc-go advisories against `/usr/local/bin/trivy`. Trivy vendors
`google.golang.org/grpc` v1.82.1; both are fixed upstream, and no Trivy release carries the fix
yet, so `TRIVY_VERSION` in `Containerfile.worker` has nowhere to move.

The entries are pinned to that one path, each says why the vulnerable code is unreachable here —
the advisories are against xDS *servers*, and the worker runs `trivy … --server <endpoint>`, the
client half — and each carries an `expired_at`, so the gate reopens on its own instead of the
entry outliving its reason. The file is the register: read it before renewing an entry, and check
the `go.mod` of the Trivy tag first, because the version that closes it retires the entry rather
than renewing it.

### Scanner rule ignores are a different thing

The stance above is about **dependency advisories** — a CVE in something we pull in, where the fix
is an upgrade. It does not govern a static-analysis rule that is simply wrong about this codebase.
Those are handled in `sonar-project.properties` as `sonar.issue.ignore.multicriteria` entries, each
pinned to one file and carrying its reasoning inline, so the justification is version-controlled
rather than clicked away in a dashboard.

The largest group is the MD5/SHA-1 uses (`rust:S4790`), which are wire-format requirements of the
package protocols BatleHub speaks. That group started at thirteen and is now nine: rechecking each
against its specification found four that no protocol required, and those were deleted rather than
ignored. [MD5 and SHA-1](/operations/weak-hashes) is the register, with the specification for each
— and it is the model for an entry here: check the spec, do not repeat the last comment.

### Duplicate versions

`[bans].multiple-versions` is `deny`, with every known duplicate enumerated in `skip`. The list is
not a suppression in the sense above — nothing is being silenced, each entry names the third-party
crate that holds the older line — but it is maintenance, and it works one way: **a duplicate the
list does not already account for fails the build**.

That was measured before it was switched on. Twenty crates resolve to more than one version out of
440 on a Linux host and not one is reachable from this repository: `rpm 0.27.1` is the latest
release and still wants `enum-display-derive` (syn 1) and the `digest 0.10` family; `argon2 0.6`
exists only as a release candidate and this is password hashing; the rest belong to actix, sqlx,
jsonwebtoken, ring and the AWS SDK. `cargo update` moves five unrelated packages and resolves none
of them. `cargo-deny` sees sixteen more than `cargo tree` does, because it reads the graph for every
target — which is correct here, since the CLI is released for `x86_64-pc-windows-msvc` and
restricting `[graph].targets` to Linux would also stop advisories being reported for that artefact.

**Security outranks tidiness, and the file enforces the order rather than asking you to remember
it.** A red `bans` check is pressure, and pressure is where the wrong fix gets made — so:

1. If upgrading a crate to close a RUSTSEC advisory creates a duplicate, the upgrade stands and the
   `skip` is added in the same commit. An advisory is never resolved by keeping an old line because
   the tree looks tidier, and a duplicate is never resolved by downgrading.
2. `skip` has no power over `[bans].deny`. cargo-deny refuses to load the file at all when a crate
   appears in both — *"a crate was specified in both `skip` and `deny`"* — so the vulnerable lines
   named there (`rsa`, `rustls 0.21`, `h2 <0.4`, …) cannot be silenced by adding them to the skip
   list, by accident or under deadline. That is a property of the tool, not a convention.
3. Only then: name who holds the old line, and check whether it can be upgraded away before adding
   a `skip` for it.
