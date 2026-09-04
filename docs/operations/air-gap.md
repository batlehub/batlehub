---
title: Air-gap runbook
---

# Air-gap runbook

For the estate whose workstations have no route off the site. Read
[pointing mise at BatleHub](/use/mise) first — this page is the operational
loop, not the design.

::: warning Guidance, not a commitment
A template for the runbook *you* write. The cadence, the approvals and the
media below are examples; replace them with your organisation's.
:::

---

## The loop

```text
connected side                          disconnected side
──────────────                          ─────────────────
1. plan      ← the project's lock
2. seed      ← fetch + verify
3. export    ← signed bundle
                         ══ carry ══→   4. import
                                        5. read what is still missing
                                        └──────── feeds step 1 ────────┘
```

The loop closes on itself. What the disconnected side could not serve is
what the next plan carries, and nobody has to guess.

---

## 1. Build the plan

On a machine that can reach both the project's source and a **connected**
BatleHub:

```bash
batlehub mise plan --lock mise.lock --platform linux-x64,darwin-arm64 \
  --include-mise -o mise-plan.json
```

`--platform` defaults to this machine's; name the ones the estate actually
runs, or pass `all`. `--include-mise` adds mise's own release, so the estate
can upgrade the tool that reads the next plan without a second trip across
the gap.

Before going further, read the two lists it prints:

| Line | What to do |
| --- | --- |
| `no mirror: <host>` | Add a registry for it, or accept that the tool needing it will not install. |
| `unsupported: <tool>` | A git-fetched backend. There is no HTTP path; the tool has to be replaced or installed another way. |

A plan with neither is a plan the bundle can satisfy completely.

## 2. Seed and verify

```bash
batlehub mise seed --plan mise-plan.json --verify
```

Non-zero exit means the bundle would be incomplete or wrong. Do not build
one from a failed seed: the failure is either a missing mirror (fix step 1)
or a digest disagreement, which means the lock and the upstream no longer
agree and is worth understanding before it is carried across a gap.

## 3. Export

```bash
batlehub mise export --plan mise-plan.json --sign-key ./estate.key -o estate.bhub
```

```text
estate.bhub · 41 entries · 38 blob(s) · signed
  signed by 3b1fa9c2…
  the disconnected instance must list that key in [air_gap].bundle_trusted_keys
```

The signing key is a file, not a flag: a key on a command line is a key in
the shell history and in every process listing on the machine. The export
prints the **public** half, which is the value the disconnected instance
needs — copy that line rather than deriving it from the private key, which is
how a private key ends up on a command line.

**Keep the public half in the disconnected instance's config** and the
private half wherever your organisation keeps signing material. The
disconnected side accepts nothing else.

Fewer blobs than entries is normal and is the point: the bundle is
content-addressed, so an artifact reachable at two addresses ships once.

## 4. Carry and import

```bash
batlehub mise import estate.bhub
```

```text
signature ok (3b1fa9c2) · 41 blob(s) · 0 rejected
```

The signature is verified before a single blob is read. A rejection line
names what was refused and why — a blob whose bytes do not hash to its name,
a key that is not a storage key, a registry this instance does not have.
Rejections do not fail the whole import: the rest lands, and the count is
what you check.

Importing the same bundle twice writes once and says so.

## 5. Reconcile

```bash
batlehub admin air-gap-missing --kind artifact
batlehub admin air-gap-missing --kind unmirrored_host
```

or the console's **Air gap** page. The first list is content the next bundle
should carry. The second is hosts nothing mirrors — each one is a rewrite
rule you do not have, and no bundle will ever fix it.

Feed both into the next plan, and purge what you have satisfied:

```bash
batlehub admin air-gap-missing            # confirm the list first
curl -X DELETE "$BATLEHUB/api/v1/admin/air-gap/missing?before=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  -H "Authorization: Bearer $TOKEN"
```

---

## What an operator should expect to see

| Symptom | Meaning |
| --- | --- |
| `503` with `"code": "content_unavailable"` | Normal. The instance does not hold it and will not fetch it. The body names the coordinate; it is already recorded. |
| `501` with `"code": "unmirrored_host"` | The catch-all rule caught a host nothing mirrors. Nothing was fetched. |
| `403` naming a block | Not a gap. An administrator blocked this coordinate, and it will not be proposed for the next bundle. |
| A hybrid registry behaving as local | Expected, and warned about at load: the fall-through to upstream cannot happen here. |
| A `503` under `--kind document` | A client asked for a *listing* — a release list, a packument, a flat index — and a bundle carries artifacts, not documents. An install from a lock does not need one; anything resolving a version at install time does. |

Both lists on the page name registries that hold **nothing at all**, which is
worth reading first: such a registry refuses every request, and an empty miss
log beside it means "nobody has asked yet", not "complete".

## Before the gap

Two things are worth doing while the instance can still be reached:

- **Seed it.** An air-gapped registry with no content answers `503` to
  everything, which is correct and useless. The server says so once at boot
  and the Air gap page keeps saying it until a bundle lands.
- **Check the trusted keys.** `enabled = true` with an empty
  `bundle_trusted_keys` is refused at load, and a malformed key is refused
  too — but a key that is *valid and wrong* is only discovered at the first
  import, on the wrong side of the gap.
- **Make sure every project commits its `mise.lock`,** with
  `lockfile = true`. It is the difference between an install that works and
  one that does not: from a lock, mise attempts exactly the asset URL the
  bundle carries; without one it asks the forge to resolve the version
  against the release list, which is a document, and gets a `503`.
- **Turn mise's own signature verification off on the disconnected side.**
  All five of them:

  ```toml
  [settings]
  github_attestations = false
  [settings.github]
  slsa = false
  [settings.aqua]
  cosign = false
  slsa = false
  minisign = false
  ```

  Each reaches Sigstore or the forge before it will install anything, and a
  transparency log is not something a proxy can cache — an offline inclusion
  proof proves nothing. Left on, an install gets through download and
  checksum and then fails at the attestation fetch; turn only that one off
  and SLSA stops it at the release document instead.

  The checksum in `mise.lock` is still verified locally — it needs nothing but
  the bytes — and BatleHub's own verdict crossed with them. See
  [RFC 0008 §14.8](/rfc/0008-mise-in-an-air-gapped-estate) for exactly how
  much of the connected-side verification is built.

## See also

- [Pointing mise at BatleHub](/use/mise) · [Configuration](/guide/configuration#air-gap)
- [Disaster recovery](/operations/disaster-recovery) · [Incident response](/operations/incident-response)
