---
# The security reference: one page per scanner would scatter the thing a reader
# actually needs, which is the whole matrix — nine gates, the SBOM and VEX
# workflows, and the one place each scanner's false positives are resolved. It
# crossed 4 000 words when the test-key fixtures got their own suppression
# section. `docs:structure` asks for this declaration above that line
# (RFC 0005-bis §4.5).
reference: true
---

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
| Lockfile CVEs, second opinion | [vuln-scan-action](https://github.com/mlab-sh/vuln-scan-action) (vuln.mlab.sh) | `vuln-scan.yaml` (PR) — all four roots: `Cargo.lock`, `mise.lock`, and `ui/` + `docs/` via a syft CycloneDX SBOM | **report-only**, comments on the PR — see below |
| Container / OS layers | Trivy | `image-scan.yaml` (PR + daily, GitHub) runs Trivy directly on the proxy image (`Containerfile`), the worker image (`Containerfile.worker`, RFC 0018 — the one that carries bubblewrap, postmortem and the Trivy client) and the GuardDog variant of the worker image (`Containerfile.worker-guarddog`, which adds the optional Python scanner and is gated separately, so its toolchain answers for its own CVEs); `.forgejo/workflows/build.yaml` (the proxy and hardened images, the two it builds) polls Harbor's own scan-on-push report instead | block on fixable HIGH/CRITICAL |
| Development toolchain | Trivy (`rootfs` over the installed tools) | `mise-scan.yaml` (PR touching `mise.toml`/`mise.lock`, weekly) | budget on fixable HIGH/CRITICAL — see below |
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

Two rate limits apply. Repository reputation is read from the GitHub API — CI passes `github.token`
automatically; locally, export `GITHUB_TOKEN` or every dependency comes back `stats-failed`.

Vulnerability lookups go to `vuln.mlab.sh`, capped at **8 scans/hour anonymously**, and **that budget
is shared with `vuln-scan.yaml`** — one `VULN_MLAB_TOKEN`, one quota, two workflows. postmortem
spends 3 per run and vuln-scan 5, so a single pull-request push costs the whole anonymous hour and
anything after it is throttled. `VULN_MLAB_TOKEN` is therefore no longer optional in practice: set it
once as a repository secret and both workflows pick it up, with no per-workflow configuration.
Locally, export `VULN_MLAB_TOKEN` in your shell or put `vuln_token` in `~/.postmortem/config.yml`.

### vuln-scan-action (lockfile CVEs, advisory)

`vuln-scan.yaml` scans all four dependency roots against `vuln.mlab.sh` on every pull request and
posts the result as a single comment, edited in place on each push. **It never fails the build.**

`Cargo.lock` and `mise.lock` are sent as-is. `ui/` and `docs/` go through a **CycloneDX SBOM**: the
scanner has no pnpm parser and `pnpm-lock.yaml` is the only frontend lockfile this repository keeps,
so each root is catalogued with syft — the same tool `build.yaml` already uses for the image SBOM —
and reduced by `.github/scripts/slim_sbom.py`. That script exists for two reasons, both found by
running the thing rather than reading its README:

- **Size.** A faithful syft SBOM of `ui/` is 896 KB and the endpoint refuses it with HTTP 413.
  Keeping only `type`/`name`/`version`/`purl` — all a scanner reads — brings it to 73 KB.
- **The 512-package cap.** The endpoint reads at most 512 components per request, warns once, and
  then reports the remainder as though it were the whole tree, so a truncated scan comes back
  looking clean. `ui/` holds 691 packages. The script splits the document into pieces of at most
  512, so `ui/` costs two requests and `docs/` one, and nothing is silently dropped.

`Cargo.lock` (675 crates) is still read only to that 512 limit, deliberately: it is the one root with
a complete gate of its own in `cargo audit`, so this is a second opinion on it rather than its
coverage, and converting it to CycloneDX to chunk it would mean building crate metadata on every pull
request to buy nothing.

**Why it is advisory and not a gate.** A run on 2026-09-14 returned two Rust findings, `dirs@7.0.0`
(RUSTSEC-2020-0053) and `ring@0.17.14` (RUSTSEC-2025-0007). Both are `informational = "unmaintained"`
— not vulnerabilities — and both were **withdrawn** upstream, in 2021 and 2025. `cargo audit` reads
the same RUSTSEC database and reports this lockfile clean, so a blocking gate would have failed every
pull request on two retracted advisories.

Severity is the other half. RUSTSEC-sourced findings all come back `severity: unknown`, which ranks
below `low`: `fail-on: high` can never trip and `fail-on: any` trips on exactly those two withdrawn
advisories. npm findings *do* carry a real severity — the one standing frontend finding is
`@ai-sdk/provider-utils@4.0.5`, CVE-2026-8769, `low`, which `pnpm audit --audit-level high` does not
report. So the data is not uniformly poor, just not yet uniform enough to gate on, and `fail-on` is
`none`. Re-check those two numbers before promoting it.

**Quota — this is the expensive workflow.** It spends 5 scans per run (Cargo.lock, mise.lock, two
`ui/` pieces, one `docs/` piece) and postmortem spends 3 more against the same budget. Anonymous
`vuln.mlab.sh` allows 8 per hour, so one pull-request push very nearly exhausts it. **Set
`VULN_MLAB_TOKEN`** (25/hour) — the same secret already backs `postmortem.yaml`. A 429 is thrown
rather than reported, so it fails the step whatever `soft-fail` says; the comment then says that half
of the scan did not complete rather than showing an empty report as "clean".

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

Every release publishes four CycloneDX SBOMs, one per artefact it ships:

- `sbom-rust.cdx.json` — the shipped server's Rust dependency closure (crate-level), attached to the
  GitHub release **and** attested against the binary's own digest, so the SBOM that describes a
  given `batlehub` cannot be swapped for another.
- `sbom-image.cdx.json` — the proxy container image (OS packages + binaries).
- `sbom-image-worker.cdx.json` — the worker image (RFC 0018).
- `sbom-image-worker-guarddog.cdx.json` — the GuardDog variant of the worker image.

All three image SBOMs are attached to the release **and** pushed to the registry as attestations
(`actions/attest-sbom`, `push-to-registry: true`), which is what *bound to the image* means here:
the attestation is an OCI referrer of the image digest, so it travels with the image through any
registry that understands referrers — no release page, no GitHub API, and nothing to keep in step
by hand.

The worker images matter most here and used to have no SBOM at all. They are the ones carrying a
*second* toolchain — bubblewrap, the Trivy client, postmortem, and GuardDog's whole Python tree —
so they are precisely the images whose contents a consumer cannot derive from this repository's
lockfiles.

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

## Signatures — who published this

Everything a release publishes is signed, **keylessly**: the signing identity is the release
workflow's OIDC token, recorded in Sigstore's transparency log, so there is no private key in a
secret to leak and no public key for a consumer to fetch. Verification checks an *identity* — this
workflow, on this repository — rather than a fingerprint someone has to be told to trust.

| Artefact | Mechanism | Verify with |
| --- | --- | --- |
| The three container images | `cosign sign` **by digest** | `cosign verify --certificate-identity-regexp … --certificate-oidc-issuer https://token.actions.githubusercontent.com <image>` |
| The Helm chart (OCI) | `cosign sign` by digest | the same command — an OCI chart is an image as far as cosign is concerned |
| Server binary, every CLI archive | `actions/attest-build-provenance` | `gh attestation verify <file> --repo <owner>/<repo>` |
| Every asset on the release page | `checksums.txt` + a detached Sigstore bundle | `cosign verify-blob --bundle checksums.txt.sigstore.json … checksums.txt` then `sha256sum -c checksums.txt` |

The last row is the one that works **offline-ish**: a mirror, a distro packager or an air-gapped
importer has the files and not the GitHub API, and `checksums.txt` plus its bundle is enough to
establish that this workflow produced exactly these bytes.

Signing is by digest and never by tag, everywhere. A tag is a mutable pointer, so a signature over
`:1.2.3` would attest to whatever that name resolves to at verification time — which is the property
an attacker with push access needs.

**What is not signed, and why.** The Forgejo mirror build (`.forgejo/workflows/build.yaml`, which
pushes the proxy and hardened images to Harbor on every push to a branch) is unsigned. Keyless
signing needs an OIDC issuer Sigstore's public-good instance already trusts, and a self-hosted
Forgejo is not one; signing there would mean a long-lived key in a secret, which is the thing
keyless exists to avoid. Those images are continuous builds gated on Harbor's own scan-on-push,
not releases — **the signed artefacts are the ones a release publishes**, and that is the set the
table above covers.

## VEX — what the standing findings *mean* {#vex}

A consumer who scans a published image sees the same findings the release gate accepted, and has no
way to know they were ever assessed: `.trivyignore.yaml` is read by Trivy and by nothing else, and
it never leaves this repository.

`vex/batlehub.openvex.json` is the other half. It states the same decisions in
[OpenVEX](https://openvex.dev/) — a format every mainstream scanner reads — with, for each
vulnerability, a machine-readable `justification` and the prose (`impact_statement`) that says why
the vulnerable code is unreachable in these images.

The release binds it to what it published (`.github/scripts/vex.py render`): each product is
rewritten from a repository name to the **digest** this release actually pushed, the document is
attached to the release page, and `cosign attest --type openvex` publishes it as a referrer of each
image. A consumer can then either apply it from the release page or read it off the image:

```bash
# Scan with our assessment applied
trivy image --vex batlehub.openvex.json ghcr.io/<owner>/batlehub-worker:<version>

# Or read the attestation that travels with the image
cosign download attestation --predicate-type https://openvex.dev/ns \
  ghcr.io/<owner>/batlehub-worker@sha256:… | jq -r .payload | base64 -d | jq .predicate
```

**The two files cannot drift.** `task vex` (part of `task security`, and its own job in
`image-scan.yaml`) fails when `.trivyignore.yaml` suppresses a CVE the VEX document does not state,
*and* when the VEX document states a `not_affected` that the gate is not suppressing — the first
means a consumer is told a finding is unassessed when it was, the second means a statement has
outlived the reason it was made. It also enforces that every `not_affected` carries one of
OpenVEX's five justifications *and* prose: the justification is for the scanner, the prose is for
the person who has to believe it.

The expiry stays in `.trivyignore.yaml` and is deliberately not duplicated here — that file is the
gate, and a date with two homes has one that is wrong.

## The development toolchain

`mise.toml` installs some forty tools, and until recently nothing looked at them. The one thing
that appeared to — `mise.lock` in `vuln-scan.yaml` — reads **four** of them: that endpoint's
lockfile parser understands the `cargo:` and PyPI backends and nothing else, so every tool that
arrives as a GitHub release asset (trivy, syft, helm, k6, gitleaks, node, go, task, rc, …) came back
with no findings, which is indistinguishable from a clean scan.

`mise-scan.yaml` runs `trivy rootfs` over the **installed** toolchain instead. That reads what a
lockfile scan cannot: the Go module list is embedded in every Go binary, and the Node and Python
trees are on disk as themselves. Measured on 2026-09-15 against a toolchain the lockfile scan
reported clean: 249 findings, **136** of them fixable HIGH or CRITICAL.

```bash
task mise:cve            # the same scan and report, locally
task mise:cve:budget     # record the current number as the budget CI checks against
```

**The scan root is the machine's, the count is this repository's.** Trivy takes one directory and
mise installs every tool into the same one, so a workstation's tree holds its owner's global tools
and every other project's as well — the first measurement here was 461, of which 255 belonged to
`~/.config/mise/config.toml` (`helm-ls`, `k9s`), to a sibling checkout (`etcd`,
`kube-apiserver`) and to this repo's own `examples/terraform`. A CI runner installs `mise.toml` and
nothing else, so the two numbers described different machines. `--only-tools` now narrows the
*count* to the install directories this repository asks for
(`.github/scripts/mise_repo_tools.sh` derives them, with the global config excluded), and the
report prints what it left out rather than quietly shrinking. The workflow passes the same flag,
where it is a no-op, so the local command and the gate are one command.

Two things are worth knowing before reading the table as a to-do list. **Over half of the count is
Go `stdlib`** — the Go release each binary was compiled with, which no pin in `mise.toml` changes;
it moves when the upstream project rebuilds. And a `latest` tool that carries findings has **no fix
to apply**: on 2026-09-15 every one of `helm-docs`, `gitleaks`, `lazydocker`, `syft`, `k6`, `task`,
`trivy` and `node` was already the newest release upstream had published. What the number is good
for is noticing a tool that stopped being maintained, or a new one that arrived carrying a pile —
not a weekly bump ritual.

It is a **budget**, not a zero, and that is a deliberate choice rather than leniency: none of this
ships, a CVE in `k9s` reaches nothing this project publishes, and a gate that fails a pull request
every time somebody else's tool has a bad week is a gate that gets switched off within the month.
`.github/mise-cve-budget.json` records what the toolchain currently carries; the job goes red when
the number *grows* — a tool that has gone stale, or a new tool that arrived carrying a pile of
findings. Raising it is a commit, in the open, with a reason.

Until a budget has been recorded the job reports and cannot fail, and the report says so in those
words rather than showing a green tick for a check that has nothing to check against.

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

> Every entry here has a matching statement in `vex/batlehub.openvex.json` — see
> [VEX](#vex) above — and `task vex` fails when one of them does
> not. The ignore file is what quiets the gate; the VEX document is what tells a consumer *why*.


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

### Test key fixtures

The secret scanners are right about what a committed private key is and wrong about whether it
matters, and the tree has three of them: the RSA keys `crates/adapters/src/repo/testdata/` holds for
the apk index-signing tests. They cannot be minted at run time — `include_str!` needs a file, and
aws-lc-rs generates nothing below 2048 bits, so the "key below the floor" test needs a committed
1024-bit one either way.

Each scanner has exactly one place that quiets it, and the other places fail closed and quiet:

- **Semgrep** (`generic.secrets.security.detected-private-key`) — a path pattern in `.semgrepignore`.
  A `// nosemgrep` comment with the short rule id is silently ignored for the generic secrets rules,
  and a `.pem` has nowhere to put one.
- **gitleaks** (`private-key`, `curl-auth-header`) — `gitleaks.toml`. The scan walks full history
  (`fetch-depth: 0`), so an inline `gitleaks:allow` fixes the working tree and leaves the commit that
  introduced the line flagged forever, and a path entry has to name every path the file has ever had
  — which is why the two pre-RFC-0005 `website/` paths are still listed. **Prefer a `regexes` entry**:
  it names the value that is not a secret, so it survives a rename, covers the next file to carry the
  same literal, and still flags a real credential in the file it exempts.

Keep both tight — one pattern per fixture family, never a directory a real credential could later
land in — and re-run the scanner after adding an entry (`mise` pins both), because a suppression that
parses but does not apply looks exactly like one that works.

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
