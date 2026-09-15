# MD5 and SHA-1

For the auditor, or the scanner, asking why a 2026 codebase computes MD5.

A dependency scan of BatleHub reports uses of MD5 or SHA-1 (SonarCloud raises
them as `rust:S4790`, CRITICAL). This page is the register: every one of them,
what it is for, and — rechecked against each protocol's current specification —
whether that protocol would accept something stronger.

There were thirteen. Four turned out not to be required by anything and **were
deleted**; nine remain, and this page says why each is there.

## The line that separates them

**None of these values is what BatleHub trusts.** Artifact integrity
that is a security decision rides on SHA-256, computed independently when bytes
are first written and re-checked on every later serve, and on the OpenPGP
signatures over the repository indexes.

The weak digests are all one of three things: a field a wire format *names* as
MD5 or SHA-1, a cache validator a client compares byte-for-byte, or the
verification side of a checksum some upstream registry advertised in SHA-1. A
collision against any of them buys an attacker nothing they could not get by
serving different bytes, because nothing gates on them.

That is the argument for the nine that remain. It is not the same as saying
every one is *unavoidable*, which is what the recheck was for — and four did not
survive it.

## The register

| # | Where | Algo | Verdict |
| --- | --- | --- | --- |
| 1 | `core/services/integrity.rs` — `sha1_hex` | SHA-1 | Mandated by Composer |
| 2 | `core/services/integrity.rs` — `verify`, `StreamingVerifier` | SHA-1 | Verification, not emission |
| 3 | `core/…/local_registry/eco_rubygems.rs` — `/versions` | MD5 | Mandated by the compact index |
| ~~4~~ | ~~`web/…/proxy/rubygems/range.rs` — ETag~~ | ~~MD5~~ | **Removed** — the ETag is a SHA-256; see [§4](#etag-md5-removed) |
| ~~5~~ | ~~`adapters/repo/deb.rs`~~ | ~~MD5, SHA-1~~ | **Removed** — optional in Debian |
| ~~6~~ | ~~`adapters/repo/pacman.rs`~~ | ~~MD5~~ | **Removed** — gone from the format |
| 7 | `adapters/repo/openpgp.rs` — fingerprint | SHA-1 | Immutable by definition |
| 8 | `core/services/listing_synthesis.rs` — composed listings | SHA-1 | Mandated by the format being imitated |
| 9 | `web/…/proxy/maven/proxy.rs` — `.md5`/`.sha1` sidecars | MD5, SHA-1 | The algorithm *is* the file extension |

Entries 4, 5 and 6 are struck through because the code no longer computes them.
Entry 4 was the one the 2026-08-31 recheck left standing as "a compatibility
choice rather than a requirement, and the one left worth revisiting"; it was
revisited and removed on 2026-09-15. See [Rechecked](#rechecked-2026-08-31) and
[§4](#etag-md5-removed).

## 1. Composer `dist.shasum`

Composer's `dist` object carries a single checksum field, `shasum`, and it is a
SHA-1. Publishing a SHA-256 in it does not degrade to "unverified" — Composer
hashes the downloaded zip with SHA-1 and compares, so every download fails.

A `sha256` dist field has been [an open feature request since
2017](https://github.com/composer/composer/issues/5940) and is not implemented.
**Nothing stronger is available.**

Scope note: `sha1_hex` is called from exactly one place, the Composer publish
handler. It is not used for npm — the npm proxy prefers `dist.integrity`
(SSRI, usually SHA-512) over `dist.shasum` and falls back only when the upstream
omits it, and the local npm packument passes through whatever `dist` the
publishing client sent, rewriting only the tarball URL.

## 2. Verifying what an upstream advertised

`verify` and `StreamingVerifier` accept SHA-1, SHA-256 and SHA-512 and pick the
algorithm from the checksum the *upstream registry* published. When a registry
advertises a SHA-1, hashing with SHA-1 is the only way to compare against it.

This is the one entry where removing the weak algorithm makes things strictly
worse: the alternative to verifying a SHA-1 checksum is not verifying at all.

## 3. The RubyGems compact index

The compact index `/versions` document is a line per gem ending in a digest, and
[the format defines that digest as an
MD5](https://github.com/rubygems/guides/blob/main/rubygems-org-compact-index-api.md)
of the gem's `/info` document:

```
RUBYGEM [-]VERSION_PLATFORM[,VERSION_PLATFORM],...] MD5
```

Bundler recomputes it to decide whether its cached `/info` copy is current. It is
a cache validator, in the algorithm the format names, and it is still current.
**Nothing stronger is available for this field.**

Note that the `Repr-Digest` header BatleHub sends on the same documents is
already SHA-256 — that is the modern, RFC 9530 digest, and it is the one the
specification actually requires.

## 4. The compact index ETag — removed {#etag-md5-removed}

`compact_response` sets the `ETag` over the document body and `holds_our_prefix`
re-derives it over a *prefix*, which is what makes Bundler's resumable range
requests answerable: if the client's validator equals the digest of our first
*N* bytes, its copy **is** our prefix and appending the tail is provably
correct (RFC 0009 §13.24).

That mechanism never needed MD5. **The etag is opaque to the client**, which
reads it back out of its own etag file and quotes it — `bundler 4.0.17`,
`compact_index_client/updater.rb`:

```ruby
etag = etag_path.read.tap(&:chomp!) if etag_path.file?
headers["If-None-Match"] = %("#{etag}") if etag
```

The only reason it had been an MD5 is that Bundler **before 2.7** synthesised a
validator itself when it had no stored etag — `SharedHelpers.digest(:MD5)
.hexdigest(IO.read(path))` over its local file — so a server etag in any other
algorithm never matched it. [Bundler 2.7.0 removed that digesting on
2025-07-16](https://bundler.io/changelog.html) as a breaking change, and there
is no trace of MD5 anywhere in 4.0.17's updater.

So the etag is now a **SHA-256** of the document, and MD5 is gone from
`range.rs` entirely. What a pre-2.7 client gets: its synthesised validator
matches nothing, `holds_our_prefix` says no, and the answer is `200` with the
whole document — exactly the behaviour it had before any of this existed.
Degraded by one full transfer, never wrong: no client is handed a spliced
document. That is the cost of dropping support for Bundler < 2.7, and it is
paid by clients three major versions behind.

## 5. Debian `Packages` and `Release` — removed

`parse_deb` used to compute MD5, SHA-1 and SHA-256 over each `.deb`; the
`Packages` stanza emitted all three, and `Release` listed every index under an
`MD5Sum:` section as well as `SHA256:`.

[The Debian repository
format](https://wiki.debian.org/DebianRepository/Format) makes the weak ones
optional and says plainly what a client may do with them:

> Clients may not use the MD5Sum and SHA1 fields for security purposes, and must
> require a SHA256 or a SHA512 field.

So they bought nothing: apt accepts an index carrying only SHA-256, the SHA-256
is what it verifies, and the OpenPGP signature over `Release` is what makes the
index trustworthy in the first place. **`DebPackage` and `ReleaseFile` no longer
carry an `md5` or `sha1` field**, the `MD5sum:`/`SHA1:` lines are gone from each
stanza, and the `MD5Sum:` section is gone from `Release`.

An uploaded `.deb` whose own `control` carries an `MD5sum` or `SHA1` field still
has it dropped — those are repository-level fields, and an upload does not get to
reintroduce one.

The cost is compatibility with apt too old to support SHA-256 — far older than
anything still receiving security updates.

## 6. Pacman `%MD5SUM%` — removed

This one was not merely optional. [pacman
6.1.0](https://gitlab.archlinux.org/pacman/pacman/-/raw/master/NEWS) dropped
md5sum support from repository databases — `repo-add` stopped writing it and
libalpm dropped both reading and validation — and
[`alpm-repo-desc(5)`](https://man.archlinux.org/man/extra/alpm-repo-db/alpm-repo-desc.5.en)
states:

> The section `%MD5SUM%` has been removed.

BatleHub was writing a field current pacman ignores. **`PacmanPackage` no longer
carries an `md5`**, and `desc_entry` no longer emits `%MD5SUM%`; `%SHA256SUM%`,
which pacman does read, is unchanged.

`PacmanPackage` is stored as JSON, and the struct has no `deny_unknown_fields`,
so metadata written before this change still deserializes — the extra key is
ignored.

## 7. The OpenPGP v4 fingerprint

A version-4 OpenPGP key fingerprint *is defined as* SHA-1 over
`0x99 || len16 || pubkey_body` (RFC 4880 §12.2), and the key ID is its low 64
bits. This is an identifier with a specified construction, not a choice of hash:
computing it any other way produces a fingerprint no client recognises, and apt's
`Signed-By:` and `rpm --import` both key off it.

RFC 9580 v6 keys use SHA-256, but apt and rpm do not consume v6 keys today.
**Immutable while the key is v4.**

## Rechecked 2026-08-31

The original triage recorded all thirteen as "mandated by a wire format". Three
entries did not survive checking that against the specifications, and two of the
three were then deleted:

- **Pacman's `%MD5SUM%` (#6)** — the field is gone from the format. Removed.
- **Debian's MD5/SHA-1 (#5)** — optional, and the specification forbids clients
  from relying on them. Removed.
- **The compact index ETag (#4)** — required only by Bundler older than 2.7.
  Kept: it still buys resumable range requests for those clients, and unlike the
  other two it is not dead output.

That took thirteen findings to nine. Nothing here was a vulnerability — no attack
was enabled by any of them, which is why they were a register rather than an
incident — but four of them were output no current client reads, and the
difference between "the format requires this" and "we have always emitted this"
is exactly what a recheck is for.

### How the removals were verified

Real `apt` accepts the resulting repository: `apt-get update` verifies the
Ed25519 OpenPGP signature over `InRelease`, indexes the package, and
`apt-get download` fetches it. Corrupting the `.deb` makes apt reject it with a
hash mismatch reported against **SHA256**, which is the point — the check that
was doing the work is still doing it.

`.github/workflows/repo-interop.yaml` runs that end to end for real `apt`, `dnf`
and `pacman` in containers, and it triggers on any change under
`crates/adapters/src/repo/`.

## Rechecked 2026-09-15 — is anything stronger available yet?

The question this register exists to keep answerable: for each entry, has the
upstream that mandates the weak algorithm published a stronger option since the
last look? Checked against the upstreams themselves rather than from memory.
**Nothing moved upstream.** No entry changes for that reason — and one of them
was never waiting on an upstream, so it is gone (entry 4, below).

| # | Entry | Upstream state on 2026-09-15 |
| --- | --- | --- |
| 1 | Composer `dist.shasum` | Still SHA-1 only. [composer#5940](https://github.com/composer/composer/issues/5940) is **open** (last activity 2025-01-22), and `src/Composer/Downloader/FileDownloader.php` on `main` still reads `getDistSha1Checksum()` and compares `hash_file('sha1', …)`. A `sha256` field would be ignored on the way in and fatal on the way out. |
| 3 | RubyGems `/versions` info checksum | Still MD5. The reference implementation, `rubygems/compact_index` on `master`, computes `Digest::MD5.hexdigest(CompactIndex.info(...))`. The format names the algorithm; there is no second field. |
| ~~4~~ | Compact index ETag | **Deleted the same day.** It was never an upstream question — Bundler ≥ 2.7 stopped digesting these responses in July 2025 — so the support floor moved past 2.6 and the etag is a SHA-256. See [§4](#etag-md5-removed). |
| 7 | OpenPGP v4 fingerprint | Unchanged, and not a hash choice. The SHA-256 path exists — RFC 9580 defines **v6** keys whose fingerprint is a SHA-256 — but that is a different key version, so adopting it renames every key we publish and depends on `apt`, `rpm` and `pacman` verifiers accepting v6. It is a key-format migration with an interop gate, tracked separately from this register if it is ever taken. |
| 9 | Maven `.md5`/`.sha1` sidecars | **Already additive.** `maven/proxy.rs` computes and serves `.sha256` and `.sha512` beside them; the weak pair survives only because a default-configured Maven resolver asks for those two file names. Nothing is gained by removing them except broken default clients. |

The npm side deserves its own line, because it is the one place where the strong
option won outright. `pacote` — the fetcher npm, and everything built on it,
uses — reads `dist.integrity` first and falls back to `dist.shasum` only when it
is absent (`lib/registry.js`: `dist.integrity ? ssri.parse(…) : dist.shasum ?
ssri.fromHex(dist.shasum, 'sha1')`). This server never relies on the fallback:
the synthesised packument in `listing_synthesis.rs` emits **`integrity` only**,
a SHA-256 SRI, and the proxy path prefers whatever `integrity` the upstream
advertised. The SHA-1 that remains anywhere near npm is in the fixtures — the
mock upstream and the two heavy-suite upstream builders — and it is there on
purpose: their job is to look like the registry npm actually talks to, and a
real packument carries `dist.shasum`. A fixture that is stronger than the thing
it imitates stops testing the fallback path a real upstream can still put us
on.

## Beside the product code: the tests and the fixtures

Five more files are pinned, and none of them is a decision — each computes a
weak digest to *check* or to *imitate* one of the entries above, so the
algorithm is chosen by the thing under test:

| Where | Algo | What it is for |
| --- | --- | --- |
| `web/tests/local_rubygems_compact_index.rs` | MD5 | Recomputes entry 3's digest to assert the server emits what Bundler expects. |
| `web/tests/air_gap.rs` | MD5, SHA-1 | Asserts entry 9's sidecars are emitted, with the values a Maven client computes. |
| `tests/heavy/upstream_audit.sh` | SHA-1 | Recomputes npm's `dist.shasum` to check what the server served. |
| `tests/heavy/upstream_dir.sh` | SHA-1 | *Produces* that `dist.shasum`: it is the npm upstream the hybrid suite installs from. |
| `perf/mock-upstream/src/main.rs` | SHA-1 | The same, for the soak load's upstream — a wrong `shasum` there is a 502 on every artifact read. |

The mock is the newest of these and is the reason to repeat the recheck
discipline rather than the last comment: it had *two* weak digests, and only one
of them was required. Its RubyGems compact-index `|checksum:` was a synthetic
SHA-1 where [the format names a
SHA-256](https://github.com/rubygems/guides/blob/main/rubygems-org-compact-index-api.md)
of the gem — wrong about the protocol as well as weak — and it emits SHA-256
there now. Only the npm `dist.shasum` is pinned.

## How these are handled in the scanner

Each has a `rust:S4790` (or `shell:S4790`) entry in `sonar-project.properties`,
pinned to the single file that speaks the protocol, with its reasoning inline.
They are configured ignores rather than per-issue dashboard resolutions so that
the justification is version-controlled and reviewable.

Eleven files are pinned: the six of the register that are product code, and the
five above. The scoping is deliberate — a weak hash *outside* them is a real
finding. Do not widen a `resourceKey` to a directory, and do not add a twelfth
entry without an argument of the same kind, which, as this page shows, means
checking the specification rather than repeating what the last comment said.

Related: [Security scanning](/contributing/security-scanning) for the full
scanner matrix and the project's no-suppressions stance on dependency
advisories.
