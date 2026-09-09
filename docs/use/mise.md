---
title: mise
---

# Pointing mise at BatleHub

[mise](https://mise.jdx.dev) installs tools from wherever each backend
happens to publish them: GitHub releases, npm, crates.io, `nodejs.org/dist`,
a vendor's own host. Every one of those is a direct download, and none of
them goes through a package manager you can point at a proxy with one
setting. `[settings.url_replacements]` is how mise is told to route them
here instead.

This page is the one home for that, connected and disconnected.

## 1. The connected case

Ask the CLI what your project needs, and it prints the block:

```bash
batlehub registry suggest --mise
```

It scans `mise.lock` first — the lock records the exact URL of every tool,
per platform, so the answer is derived from what the project actually
downloads rather than guessed from tool names. Add `--mise-commented` to get
a block you can commit into a shared `mise.toml` without turning it on for
everyone at once.

Order matters, and the generator handles it: mise applies the rules in
declaration order, first match wins, and matching **stops at the first hit**.
There is no fallback — the rule that matched is the only URL tried.

::: tip Credentials
For a registry that requires auth, add a `~/.netrc` entry for the proxy host
rather than embedding a token in the URL. mise reads it, and the token stays
out of shell history, CI logs and `mise doctor` output.
:::

## 2. What an air gap needs

A host with no route off the site cannot install anything mise has not
already been told about, and the failure it gives you today is a connect
timeout with no name attached. Three things change that
([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate)):

1. **The lock becomes a plan.** `mise.lock` already names every download;
   `mise plan` resolves each one onto the path it takes through this server,
   and reports the two things you cannot find out today — the tools that
   will not work at all, and the hosts nothing mirrors.
2. **The server refuses to dial out.** With `[air_gap]` on, a miss is an
   immediate `503` that names the coordinate, not a timeout, and it is
   recorded — so the list of what the next bundle needs is produced by the
   estate rather than guessed.
3. **Verification moves to the connected side.** mise's own cosign and
   attestation checks cannot run offline; instead they run once where they
   can, and what they found travels with the content.

### 2.1 Plan

```bash
batlehub mise plan --lock mise.lock --platform linux-x64 -o mise-plan.json
```

```text
28 tool(s) · 41 download(s) · 6 registries · 3 host(s) with no mirror configured
  no mirror: binaries.sonarsource.com
  unsupported: asdf:mise-plugins/mise-postgres: a git-fetched plugin backend; mise clones it, and there is no HTTP path through BatleHub
```

Planning is offline: it reads the lock and the server's registry list and
resolves nothing over the network.

`--platform` defaults to **the platform of the machine you run it on**, which
is the common case and keeps a bundle the size of the estate that asked for
it. Name several, comma-separated, for a mixed estate, or `--platform all`
for every platform the lock records. If the lock has nothing for this host,
the command says so and plans everything rather than writing an empty bundle.

A lock records two addresses for a forge release asset — the download URL and
the API's own — and both are planned. They are one artifact reached two ways,
they share a digest, and the bundle carries a single copy: the second address
costs a manifest row and nothing else. It is not optional, because the
`aqua:` and `github:` backends usually fetch the API one.

Add `--include-mise` to carry mise itself, so the bundle always contains the
binary that will read the next plan. The version is the mise on your PATH
unless `--mise-version` says otherwise.

**Read `no mirror` before anything else.** It is the answer to "is my rewrite
table complete?", and it is cheaper to answer now than at install time on a
machine with no network.

### 2.2 Seed

```bash
batlehub mise seed --plan mise-plan.json --verify
```

Every planned entry is fetched *through* BatleHub — fetching is warming — and
the digest of what the server served is compared with the lock's. The exit
status is non-zero if any entry is missing or any digest disagrees, so this
is usable as a CI gate on the connected side.

`--verify` additionally reports what the supply-chain layer
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)) said about each
entry and fails the run on a denied verdict. Strictness is the registry's
`[registries.security]`, not a flag here: a registry that requires provenance
refuses an unsigned artifact whoever asks.

### 2.3 Export, carry, import

```bash
# connected side
batlehub mise export --plan mise-plan.json --sign-key ./estate.key -o estate.bhub

# disconnected side
batlehub mise import estate.bhub
```

A bundle is a tar of three things:

```text
manifest.json     the plan, plus what was verified about each entry
blobs/<sha256>    content-addressed, so identical bytes ship once
manifest.sig      an ed25519 detached signature over manifest.json
```

Blobs are named by their own digest, so a blob that does not hash to its name
is rejected on the way in without reference to any signature — and the
signature is checked **before a single blob is read**. An import is
idempotent by bundle id: carrying the same bundle twice writes once.

Ed25519 because it is the only signature BatleHub verifies in-process. Cosign
signatures are ECDSA over an x509 identity, which this server cannot check,
so it records the **verdict** rather than the signature and labels it as
evidence about a past check — never as a live one.

## 3. Turning the air gap on

```toml
[air_gap]
enabled             = true
bundle_trusted_keys = ["3b1f…"]   # hex ed25519 public keys accepted on import
record_misses       = true
miss_retention_days = 90
```

See [configuration](/guide/configuration#air-gap) for the full reference. Two
refusals at load are worth knowing about before you try them: an egress
proxy alongside `enabled = true` is a contradiction, and `enabled = true`
with no trusted keys would accept any bundle at all.

### 3.1 The catch-all rule

```bash
batlehub registry suggest --mise --mise-catch-all
```

appends one final rule sending anything no other rule matched to
`/_air-gap/unmirrored/{host}/…`. That route **fetches nothing**: it answers
`501`, names the host and records it. An unmirrored host becomes a line in
the console instead of a connect timeout on a workstation.

It must be last, which is why the generator appends it rather than telling
you to — and it is preceded by an identity rule for this server's own host,
because the catch-all matches *every* https URL and would otherwise send a
backend you had already pointed at BatleHub into the `501` sink.

## 4. Reading what is missing

```bash
batlehub admin air-gap-missing
batlehub admin bundles
```

or the console's **Air gap** page under Operations. The miss log is one row
per `(registry, key)` with a counter — mise retries, and the log must not
grow with the retries — sorted most-asked first, which is the order to build
the next plan in. Two columns say what the row means for the next plan:
**Requested**, the version the client asked for when its request named one
(mise's release by tag does, so `mise install gh@2.61.0` against an
instance holding 2.60.0 reads *requested v2.61.0, held v2.60.0*), and
**Held**, what the instance had of that tool.

A coordinate an administrator **blocked** never appears there. A blocked
package is not a gap in the mirror, and proposing it for the next bundle
would be undoing the block by accident.

## 5. What will not work

- **`asdf:` and `vfox:` plugin backends.** mise fetches those over git, and
  there is no HTTP path through a proxy. `mise plan` lists them under
  `unsupported`, with the backend named.
- **A host you have no registry for.** The plan says so; add a registry or
  drop the tool.
- **Live signature verification on the disconnected side.** It cannot run
  there: `github_attestations`, `github.slsa`, `aqua.cosign`, `aqua.slsa` and
  `aqua.minisign` each reach Sigstore or the forge before installing, and a
  transparency log is not cacheable. Turn all five off; the
  [runbook](/operations/air-gap) has the block. The checksum in `mise.lock` is
  still verified locally — it needs nothing but the bytes — and what replaces
  the signature is BatleHub's verdict, recorded when it could be and carried
  across with them.
- **A version the bundle did not carry.** A bundle carries artifacts and
  the entry that finds them, not the forge's own documents. Since
  [RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap) the disconnected
  instance *composes* the release document from the assets it holds, so
  `mise install some-tool@1.2.3` works without a lock when 1.2.3's asset was
  bundled — mise reads the release by tag, finds the asset, installs. For a
  version that was not bundled the release is a `503`, recorded under the
  `document` kind with the tag as **Requested**, and mise's fallback to the
  release list finds only the held versions. The lock is still the bill of
  materials: an install **from `mise.lock`** attempts exactly one URL, the
  asset the lock names, and `mise plan` reads the lock rather than the
  config because that is the list the bundle must carry. Keep
  `lockfile = true` and commit the lock.

## See also

- [Registries overview](/registries/) · [Configuration](/guide/configuration#air-gap)
- [The air-gap runbook](/operations/air-gap) — build, carry, import, reconcile
- [RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate) — why it is shaped this way
