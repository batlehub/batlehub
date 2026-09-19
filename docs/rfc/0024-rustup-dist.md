---
reference: true
---

# RFC 0024 — rustup dist: the Rust toolchain as a typed registry

| Field       | Value                                                        |
| ----------- | ------------------------------------------------------------ |
| Status      | **Implemented** — phases 1, 2, 3 and 5 of §12 landed 2026-09-19 (the kind, the routes, `tests/heavy/rustup.sh`, the surface) and shipped in v1.3.0. **Two phases are outstanding and §13 says so**: `registry suggest` on `rust-toolchain.toml` (phase 4) and the air-gapped manifest render (phase 6), which ships on its own |
| Short       | rustup dist                                                   |
| Settles     | Proxying static.rust-lang.org as a typed registry, so a Rust toolchain can be blocked rather than merely cached: the channel manifest as the filtered listing, its checksum recomputed, and no signature replaced |
| Author      | Max Batleforc <maxleriche.60@gmail.com>                       |
| Co-author   | Claude Fable 5.1 <noreply@anthropic.com>                      |
| Created     | 2026-09-11                                                    |
| Supersedes  | —                                                             |
| Touches     | `crates/core`, `crates/config`, `crates/adapters`, `crates/web`, `server`, `cli`, `ui`, `docs` |

---

## 1. Summary

A BatleHub instance proxies every crate a Rust build resolves, and not the
compiler it resolves them with. The Rust toolchain comes from
`static.rust-lang.org/dist`, a file tree with no package protocol, and today the
only way through the proxy is a `generic` registry pointed at it — which caches
the bytes and enforces nothing, because a path-addressed registry has no version
to block. `nodedist` left `generic` for exactly this reason (RFC 0010); this RFC
does the same for rustup.

`type = "rustup"` serves the dist tree as one package, `rust`, whose versions
are rustup's own toolchain names — `1.98.1`, `beta-2026-09-11`,
`nightly-2026-09-05` — and one auxiliary package, `rustup`, for the installer's
self-update tree. The channel manifests (`channel-rust-{stable,beta,nightly}.toml`,
the version-named ones, and the dated ones under `{date}/`) are the filtered
listing and the enforcement chokepoint: rustup resolves **every** install through
one, so a blocked toolchain fails on rustup's own *"could not download
nonexistent rust version"* path with nothing downloaded. The component tarballs
under `{date}/` are artifacts, streamed and cached under a per-release key.

Two facts about rustup shape everything else, and the roadmap entry that
sketched this kind had one of them wrong:

- rustup verifies each manifest against its `.sha256` sidecar and refuses a
  mismatch as *"checksum failed"*. A filtered manifest therefore needs a
  **recomputed** sidecar, or every install through the proxy breaks. The
  sidecar this instance serves is always the hash of the bytes it serves.
- rustup has not verified the `.asc` signature since 1.26.0 (April 2023), when
  the experimental PGP support was removed outright. The roadmap's "verification
  is off by default and warns" describes a client that no longer exists. The
  `.asc` is relayed byte-exact as the publisher's provenance and never
  re-signed; a filtered manifest no longer matches it, and the response says so
  in a header rather than pretending otherwise.

`RUSTUP_DIST_SERVER` is the one client switch for toolchains,
`RUSTUP_UPDATE_ROOT` the one for the installer itself. rustup rewrites the
absolute `https://static.rust-lang.org` URLs inside a manifest to the configured
dist server on its own, so the manifest is served with upstream's URLs untouched
and every component fetch still lands on this instance.

### Before / after

```text
# today — the tree is cached, and nothing on it can be refused
[[registries]]
name       = "rust-dist"
type       = "generic"
upstreams  = ["https://static.rust-lang.org"]
path_allow = ["dist/**", "rustup/**"]
#   one synthetic package, no version, nothing in explore, no block possible

# with this RFC — one package, versions rustup itself would name
[[registries]]
name = "rust"
type = "rustup"
mode = "proxy"
#   block "rust" at "nightly-2026-09-05"  →  `rustup toolchain install nightly-2026-09-05`
#     error: could not download nonexistent rust version `nightly-2026-09-05`
#   block "rust" at "1.98.1" while it is stable  →  `rustup update stable` lands on 1.98.0

export RUSTUP_DIST_SERVER="https://batlehub.example.com/proxy/rust/rustup"
export RUSTUP_UPDATE_ROOT="https://batlehub.example.com/proxy/rust/rustup/rustup"
```

```mermaid
flowchart LR
    subgraph T["today — a generic mirror"]
        A1["rustup toolchain install<br/>nightly-2026-09-05"] --> A2["path cache<br/>one synthetic package"]
        A2 --> A3["installed<br/>nothing to block, nothing in explore"]
    end
    subgraph W["with this RFC — type = rustup"]
        B1["rustup toolchain install<br/>nightly-2026-09-05"] --> B2{"release blocked?"}
        B2 -->|no| B3["manifest · sidecar recomputed<br/>components cached per release"]
        B2 -->|yes| B4["404 · could not download<br/>nonexistent rust version"]
    end
```

The same command, the same client, the same bytes on the happy path. What
changes is that the request now passes through something that knows which
release it is for, so the refusal on the right is expressible at all.

---

## 2. Motivation

1. **`generic` cannot say no.** A `generic` registry on `static.rust-lang.org`
   addresses everything as one synthetic package, so there is no version for
   `BlockListRule` to match, no row in explore, no per-release statistics and
   no age gate. The cache works, which is what hides the gap: an operator who
   has to keep a bad nightly (or a compiler with a miscompilation advisory) off
   the fleet has no lever short of removing the registry. RFC 0010 §2 made the
   same argument for Node and it holds here unchanged.

2. **The chokepoint exists, and it is exact.** rustup builds every install from
   a manifest — `dist/channel-rust-{name}.toml` for `stable`, `beta`, `nightly`,
   `1.98`, `1.98.1` and `1.99.0-beta.5`, `dist/{date}/channel-rust-{channel}.toml`
   for a dated name — and a `404` on that document is a first-class rustup
   error (`DistError::MissingReleaseForToolchain`, *"could not download
   nonexistent rust version"*), not a download failure mid-transfer. A blocked
   toolchain can be made to fail exactly the way a toolchain that never
   existed fails, before any component is requested.

3. **The naive filter breaks every install.** `DownloadCfg::download_and_check`
   fetches `{manifest}.sha256`, takes its first 64 characters, downloads the
   manifest and refuses it if the SHA-256 of the body differs
   (`RustupError::ChecksumFailed`). A proxy that edits the manifest and relays
   upstream's sidecar has turned every `rustup update` on the fleet into
   *"checksum failed"*. The sidecar is part of the listing and has to be
   designed with it; this is the fact the roadmap entry missed.

4. **The signature story in the roadmap is stale.** rustup 1.26.0 removed
   GPG/PGP verification entirely (rustup PR 3277: *"validating the integrity of
   downloaded binaries did not rely on it, and there was no option to abort the
   installation if a signature mismatch happened"*). No shipping rustup reads
   the `.asc`. The design has to state what the relayed signature *means* after
   filtering rather than lean on a warning nobody will see.

5. **The release date is free.** Every artifact lives under `dist/{date}/` and
   every manifest carries `date = "…"`, so `ReleaseAgeGateRule` gets a
   `published_at` for every toolchain without a second request — the situation
   `nodedist` has to read `index.tab` for. A gate on nightlies ("nothing younger
   than 48 hours") is the single most requested policy on a toolchain, and it
   costs nothing here.

6. **Toolchains are pinned in files this CLI already reads around.**
   `rust-toolchain.toml` names a channel, a profile, components and targets;
   `batlehub registry suggest` scans `.nvmrc` and `.sdkmanrc` and prints the
   registry block and the client variables (RFC 0010 §6.9). A Rust project is
   the most common thing in front of this proxy and the one toolchain file it
   ignores.

---

## 3. Goals / non-goals

**Goals**

- A Rust toolchain — a stable release, a beta, a dated nightly — can be
  blocked, and a blocked toolchain fails on rustup's own not-found path with
  nothing downloaded.
- Every manifest this instance serves passes rustup's own checksum, filtered or
  not, on every rustup that speaks manifest v2.
- A blocked *current* stable, beta or nightly makes the channel alias resolve to
  the newest allowed release, the way `dist-tags.latest` is repaired for npm
  and `candidates/default` for SDKMAN.
- A component an operator does not want served (`rust-docs`, 24 MB per
  target and the one most installs never open; `rust-mingw`) can be denied, and a profile install proceeds without
  it the way it already proceeds without `rust-mingw` on Linux.
- Every component tarball and every `.sha256` sibling is cached under a
  per-release coordinate that explore, statistics and retention can see.
- The age gate works on toolchains with the date the tree already carries.
- The installer's own tree (`rustup-init`, `release-stable.toml`) goes through
  the same registry, so a fleet's bootstrap does not need a second one.
- `rust-toolchain.toml` and `rust-toolchain` feed `registry suggest` and cache
  warming.

**Non-goals**

- **`local`/`hybrid` mode.** There is no publish protocol; `supports_local_mode()`
  answers `false` as it does for `nodedist`.
- **Manifest v1** (`dist/channel-rust-{name}`, no extension: a newline list of
  tarball names). rustup only falls back to it when v2 is absent; the route
  answers `404` so a blocked toolchain's fallback ends on the same rustup
  error, and nothing else is served in a format with no hashes.
- **The undated tarball copies at `dist/{file}`.** The release process
  duplicates stable tarballs beside the dated directory. rustup never requests
  them (manifest URLs are dated) and a copy without a date has no coordinate.
- **Hosting `sh.rustup.rs`.** The bootstrap script is a different host; it
  reads `RUSTUP_UPDATE_ROOT`, which is the switch this RFC documents.
- **Blocking by target triple**, or a component *allow* list. A block is a
  statement about a release; a denied component is a statement about the
  registry. Nothing finer has been asked for.
- **Re-signing.** No `.asc` is generated, and none is rewritten, for the reason
  RFC 0008-bis §11 q6 gives: the OpenPGP crates depend on `rsa`, which
  `deny.toml` bans, and replacing the publisher's provenance with this
  instance's would be a lie even if it compiled.
- **Rewriting manifest URLs to this instance's host.** rustup does it
  client-side (§4.4); doing it server-side would make the served document
  differ from upstream for no reader that needs it.
- **The `staging/` tree** rustup can be pointed at for release testing.

---

## 4. User-facing design

### 4.1 Configuration

```toml
[[registries]]
name      = "rust"
type      = "rustup"
mode      = "proxy"                              # the only mode: no publish protocol
upstreams = ["https://static.rust-lang.org"]     # the default; an internal mirror goes here

# Components never served, whatever the manifest says. A profile install
# proceeds without them; `rustup component add <one of these>` gets rustup's
# own "does not contain component". Empty — the default — denies nothing.
deny_components = ["rust-docs", "rust-mingw"]

# Target triples cache warming fetches for a warmed toolchain. Empty means the
# triple this server runs on (RFC 0010 §6.9's `warm_platforms`, same field).
warm_platforms = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
warm_packages  = ["rust@1.98.1", "rust@nightly-2026-09-05"]

[registries.rbac]
# Manifests, their checksums and manifests.txt are listings (`releases:list`);
# every file under {date}/ and the rustup/ tree is a read (`releases:read`).
# An install needs both.
anonymous = ["releases:read", "releases:list"]

# An age gate on this kind must say what it does with a coordinate that has
# no date. Every toolchain has one; the installer's own binary never does.
[[registries.rules]]
kind                   = "release_age_gate"
min_age_secs           = 172800
deny_missing_timestamp = false
```

- `upstreams` absent means `https://static.rust-lang.org`. The value is the
  **root** of the tree — the host, not `…/dist` — because the registry serves
  two subtrees of it (`dist/` and `rustup/`) plus `manifests.txt` at the root.
- `deny_components` absent or empty means the manifest is served as upstream
  wrote it. Names are component package names as the manifest spells them
  (`rust-docs`, `clippy-preview`), before `[renames]` are applied — a user asks
  for `clippy`, the manifest calls it `clippy-preview`, and the deny list uses
  the manifest's name because that is the table the filter edits.
- `warm_platforms` reuses RFC 0010's field with the same absent-means-this-host
  rule; the values are target triples rather than SDKMAN platform names.

### 4.2 The client side

```bash
# Toolchains — every manifest and every component tarball
export RUSTUP_DIST_SERVER="https://batlehub.example.com/proxy/rust/rustup"
# The installer itself — release-stable.toml, rustup-init, self-update
export RUSTUP_UPDATE_ROOT="https://batlehub.example.com/proxy/rust/rustup/rustup"

rustup toolchain install 1.98.1 --profile minimal   # channel-rust-1.98.1.toml, then rustc/cargo/rust-std
rustup update nightly                                # channel-rust-nightly.toml
rustup component add clippy                          # the same manifest, one more tarball
curl -sSf https://sh.rustup.rs | sh                  # bootstraps from RUSTUP_UPDATE_ROOT/dist/{triple}/rustup-init
```

The double `rustup` in `RUSTUP_UPDATE_ROOT` is upstream's own layout
(`static.rust-lang.org/rustup/…`) under the registry's protocol prefix, the
same way `docs/registries/generic.md` already spells it. `rust-toolchain.toml`
needs no change: rustup reads it and resolves the channel through the same
variable. mise's `rust` backend drives rustup and inherits both variables.

**rustup sends no credentials to its dist server.** Its downloader is reqwest
(the curl backend is deprecated since 1.28.2): no header can be configured and
`~/.netrc` is not read. `rustup-init.sh` is `curl` and does read it, which
covers the bootstrap and nothing after. A `rustup` registry therefore needs
`anonymous` read — the situation the VS Code gallery is in, and stated the
same way on the registry page — or an authenticating ingress. Credentials in
the URL (`https://user:token@host/…`) do work, because reqwest turns them
into Basic auth and rustup's rewrite copies them into every component URL,
but rustup prints URLs in its errors and progress, so the token lands in
build logs; `docs/registries/generic.md` already says why that is the
last resort.

### 4.3 Coordinates

| Request (under `…/proxy/{reg}/rustup/`) | `PackageId` | Cache key |
| --- | --- | --- |
| `dist/2026-09-03/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz` | `rust` / `1.98.1` / `rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz` | `rust/rust/1.98.1/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz` |
| `dist/2026-09-05/rustc-nightly-aarch64-apple-darwin.tar.xz.sha256` | `rust` / `nightly-2026-09-05` / `rustc-nightly-aarch64-apple-darwin.tar.xz.sha256` | `rust/rust/nightly-2026-09-05/…` |
| `dist/2026-09-11/rust-src-beta.tar.xz` | `rust` / `beta-2026-09-11` / `rust-src-beta.tar.xz` | `rust/rust/beta-2026-09-11/rust-src-beta.tar.xz` |
| `dist/channel-rust-stable.toml` | `rust`, version unused | metadata, `manifest:stable` |
| `dist/2026-09-05/channel-rust-nightly.toml` | `rust`, version unused | metadata, `manifest:nightly-2026-09-05` |
| `dist/channel-rust-stable.toml.sha256` | derived from the entry above | not cached: computed from the served manifest |
| `manifests.txt` | `rust`, version unused | metadata, `versions` |
| `rustup/archive/1.29.1/x86_64-unknown-linux-gnu/rustup-init` | `rustup` / `1.29.1` / `x86_64-unknown-linux-gnu/rustup-init` | `rust/rustup/1.29.1/x86_64-unknown-linux-gnu/rustup-init` |
| `rustup/dist/x86_64-unknown-linux-gnu/rustup-init` | `rustup` / *(version from `release-stable.toml`)* / same | the archive entry's key |
| `rustup/release-stable.toml` | `rustup`, version unused | metadata, `rustup-release` |

**One package, and its versions are rustup's toolchain names.** The thing an
admin blocks is *a Rust release*; `rustup toolchain install <that name>` is the
sentence the block has to defeat, so the version is spelled the way that
sentence spells it. The mapping from a dated file to a version is read off the
file name and the directory, with no manifest fetch:

| File name contains | Directory | Version |
| --- | --- | --- |
| `-1.98.1-` (a full version) | any | `1.98.1` |
| `-nightly-` or `-nightly.` | `2026-09-05/` | `nightly-2026-09-05` |
| `-beta-` or `-beta.` | `2026-09-11/` | `beta-2026-09-11` |

A component name can contain dashes (`rust-std`, `rustc-dev`,
`llvm-tools-preview`) and so can a target triple, so the parser walks the
dash-separated tokens for the first one that is `nightly`, `beta` or shaped
`\d+\.\d+\.\d+`; what precedes it is the component, what follows is the target
(absent for `rust-src`). A name with no such token is a `400` at the edge.

A beta has two spellings upstream — `beta-2026-09-11` and the manifest's
`1.99.0-beta.5` — and rustup accepts both as toolchain names. The coordinate is
the dated one, because it is derivable from the request alone; `manifests.txt`
maps one to the other, and explore shows both (§6.2). Blocking by the
`1.99.0-beta.5` spelling is §11 q3.

**The `rustup` package** exists so that the installer's binary has a coordinate
that is a version and never a moving name (RFC 0019's lesson: a cache entry is
a commit, not a branch). `rustup/dist/{triple}/rustup-init` is what
`rustup-init.sh` fetches and it names no version; the handler reads the version
out of `rustup/release-stable.toml` and serves the archive entry under it. The
bytes are identical upstream by construction — the release process copies the
archive into `dist/`.

### 4.4 Behaviour rules

**Three ways to serve a manifest, and a header that says which.** Every
manifest response carries `X-BatleHub-Manifest`:

| Value | Meaning |
| --- | --- |
| `upstream` | Byte-exact. Nothing was blocked, nothing denied, the upstream is the default host. The relayed `.asc` verifies against this body. |
| `filtered` | Upstream's document for this name with denied components edited out (below). The `.asc` no longer matches. |
| `repaired` | A different manifest than the name asked for: the alias's current release is blocked and the newest allowed release was served instead. `X-BatleHub-Version` names it. |

**The sidecar is always the hash of what this instance serves.** The route for
`{manifest}.sha256` never fetches upstream's file. It renders the manifest
exactly as the manifest route would — same block set, same deny list, same
repair — hashes the bytes, and answers `"{hex}  {file name}\n"`, the format the
release tooling writes and the first 64 characters of which rustup reads. For
an `upstream` manifest this is byte-identical to upstream's sidecar. For a
`filtered` or `repaired` one it is the only sidecar that lets rustup proceed.
rustup also stores the first 20 characters as the toolchain's `update-hash`
and skips the download when it matches; because the hash is a function of the
served bytes, two instances (or one instance after a restart) agree on it, and
a change to the block set is seen by the client as an update — which is what
it is.

**Exact names are refused; aliases are repaired.** A name is *exact* when it
denotes one release: `1.98.1`, `1.99.0-beta.5`, any `{date}/` manifest. It is
an *alias* when it moves: `stable`, `beta`, `nightly`, `1.98`. The rule:

- An exact name whose release is blocked answers `404` — the manifest, its
  sidecar and its `.asc` alike — and rustup prints *"could not download
  nonexistent rust version `1.98.1`"*. The v1 fallback rustup then tries
  (`channel-rust-1.98.1`, no extension) is `404` too, so the message is the
  same one an unpublished version gets.
- An alias whose current release is blocked is repaired to the newest allowed
  release the alias could denote, read from `manifests.txt`:
  - `stable` → the newest `channel-rust-{x.y.z}.toml` whose version is allowed;
    `1.98` → the newest allowed `1.98.z`.
  - `nightly` / `beta` → the newest dated `{date}/channel-rust-{channel}.toml`
    whose date is allowed, walking back at most **21 days** — the same
    `RUSTUP_BACKTRACK_LIMIT` rustup applies when it hunts for a nightly with a
    missing component, so a client and the proxy give up at the same horizon.
  - Nothing allowed within the horizon → `404`, and rustup's own error.

  The repaired body is served under the alias's name with its own `date`, its
  own dated URLs and a recomputed sidecar. rustup sees a manifest whose
  update-hash differs from the one it installed, and installs what it
  describes; for `stable` that is a downgrade, which is the block doing its job.

**Denied components are edited the way upstream edits a component that failed
to build.** For each name in `deny_components`, the served manifest:

- drops every `[[pkg.rust.target.{t}.components]]` and
  `[[pkg.rust.target.{t}.extensions]]` table whose `pkg` is that name, for every
  target — `Manifest::get_profile_components` resolves a profile against this
  list and silently skips names it does not find, which is how `rust-mingw`
  sits in every profile and installs on no Linux host;
- replaces the body of every `[pkg.{name}.target.{t}]` table with
  `available = false`, the state 316 tables in today's stable manifest are
  already in.

Both edits apply, and the first is the one the client answers from: with the
name gone from the target's component list, `rustup component add rust-docs`
stops before any request with rustup's own *"toolchain '…' does not contain
component 'rust-docs' for target '…'"* — and a `help: did you mean …` naming
the nearest component still listed. That is measured in `tests/heavy/rustup.sh`
§6; the *"unavailable for download"* an `available = false` alone would produce
is the message rustup prints for a component it can still see, and is not the
one this filter produces.

`[profiles]` and `[renames]` are left alone; both tolerate a name the targets
do not carry. The edit is textual and section-aware — the document is a
sequence of `[header]` blocks, and the filter removes or rewrites whole blocks
by header — so an `upstream` manifest is never re-serialised and the failure
mode of an upstream format change is a component we failed to remove, never a
document rustup cannot parse (the RFC 0010 §4.4 argument for SDKMAN's table).

**One cost is stated rather than hidden.** A dateless `nightly` install with an
already-installed component that is now denied makes rustup walk back up to
21 dated manifests looking for one that has it, and none will. Each is a
~1 MB document fetched once and served from the metadata cache after; the
client ends on rustup's own *"skipping nightly with missing component"* trail
and error. The registry page says so beside `deny_components`.

**Upstream's URLs are left in the manifest.** `DownloadCfg::url` replaces
`https://static.rust-lang.org` with `RUSTUP_DIST_SERVER` in every component URL
when the variable is set to anything else. The manifest is served with those
URLs as written, so the `upstream` case stays byte-exact and the fetch still
lands here. The one exception is an internal mirror: a manifest whose URLs name
the configured non-default upstream is normalised to `https://static.rust-lang.org`
so rustup's rewrite catches them — otherwise the component tarballs bypass the
proxy, the Terraform `X-Terraform-Get` lesson in a new coat. That document is
`filtered`, and its sidecar is recomputed like any other.

**`.asc` is the publisher's, or nothing.** `{manifest}.asc` is relayed
byte-exact from upstream with the same `X-BatleHub-Manifest` header as the
manifest it signs. When the header says anything but `upstream`, the signature
does not verify the served bytes, and a tool that checks it will say so — that
is the correct outcome, and better than a signature this instance could mint.
No rustup since 1.26.0 reads the file.

**Two more documents answer "what is stable", and they agree.**
`dist/channel-rust-stable-date.txt` is served as the `date` of whatever
`channel-rust-stable.toml` serves, and `manifests.txt` — the release tooling's
list of every manifest path, 5 156 lines today — is served with blocked
releases' lines removed. Neither is read by rustup; both are read by people
and scripts, and RFC 0010 §4.4's rule applies: filtering one document and not
its twin leaves a second, unfiltered answer to the same question.

**`.sha256` beside a tarball is a file.** `rust-std-….tar.xz.sha256` under a
dated directory is an artifact of that release, served byte-exact like
`SHASUMS256.txt` on `nodedist`. rustup does not read it (the manifest carries
`hash`/`xz_hash`); humans and mirrors do. Only a *manifest's* sidecar is
computed, because only a manifest is ever edited.

**The installer's tree is cached, and blocked at the file.**
`rustup/release-stable.toml` is relayed byte-exact — it names one rustup
version and there is no list to repair it from — so a blocked `rustup`
version is refused at the archive fetch with the download gate's `403`, and
`rustup self update` reports a download error rather than a clean not-found.
The block holds; the message degrades; the page says so, as `nodedist`'s does
for the LTS alias (RFC 0010 §4.4).

### 4.5 Validation

`AppConfig::validate()` rejects:

| Condition | Rationale |
| --- | --- |
| `type = "rustup"` with `mode = "local"`/`"hybrid"` | No publish protocol; `supports_local_mode()` already produces this error for `generic`, `nodedist` and `sdkman`. |
| `deny_components` on a registry whose type is not `rustup` | A silently ignored option is a misconfiguration that looks like a proxy bug — the same class as `broker_url` off `sdkman`. |
| A `deny_components` entry that is not a manifest package name (`[A-Za-z0-9_.-]+`) | It is matched against `[pkg.{name}…]` headers; a value with a dot-path or a space matches nothing and the operator believes it enforces. |
| `deny_components` naming `rustc`, `cargo` or `rust-std` | Every profile needs all three; the registry would serve manifests nothing can install from. `minimal` is the floor. |
| `path_allow` on a `rustup` registry | Not path-addressed; the existing validator refuses it on every such kind (RFC 0010 §13.1). |
| A `release_age_gate` rule with no explicit `deny_missing_timestamp` | Toolchains always carry a date and the `rustup` package never does; the two answers are opposite postures for the installer and the field must be chosen, as on `sdkman`/`nodedist`. |
| `upstreams` entry whose path ends in `/dist` | The value is the tree root; a `…/dist` root would put manifests at `…/dist/dist/…`. Rejected with a message naming the fix, because it is the most likely mistake for someone migrating from the `generic` example. |

Warnings (logged and surfaced to the admin):

| Condition | Behaviour |
| --- | --- |
| A non-default upstream | Logged once at reload: manifest URLs will be normalised (§4.4), and `manifests.txt` must exist on the mirror or aliases will not be repaired. No network probe — validation is offline by design (RFC 0010 §13.1). |
| `warm_platforms` entry that is not shaped like a target triple (`{arch}-{vendor}-{os}[-{env}]`) | Served as given; warming reports the manifest's "no such target" as a per-entry failure rather than refusing the config. |

---

## 5. Architecture

### 5.1 The protocol as `static.rust-lang.org` serves it

No proxy in this subsection: this is the tree as it is, and every number was
observed on the wire while writing this RFC. The client read is rustup 1.29.

| Request | Answers | Type · size | What rustup does with it |
| --- | --- | --- | --- |
| `dist/channel-rust-stable.toml.sha256` | the manifest's digest, as `"{hex}  {file name}\n"` | `binary/octet-stream` · 91 B | reads the first 64 characters; stores the first 20 as the toolchain's `update-hash` and skips the install when it matches |
| `dist/channel-rust-stable.toml` | the v2 manifest | `binary/octet-stream` · 898 637 B | parses it, and **refuses a digest mismatch** |
| `dist/channel-rust-stable.toml.asc` | a detached PGP signature | `binary/octet-stream` · 801 B | nothing at all (below) |
| `dist/channel-rust-1.98.0.toml` | the same document for a version-named release | 898 637 B | as above |
| `dist/{date}/channel-rust-nightly.toml` | the same for a dated channel | 871 349 B on 2026-01-15 | as above |
| `dist/{date}/{pkg}-{version}-{triple}.tar.xz` | one component archive | `application/x-tar` · 202 560 136 B for the whole `rust` package | unpacks it, verified against the manifest's `xz_hash` |
| `dist/{date}/{…}.tar.xz.sha256` | that archive's digest | 110 B | nothing in the install path |
| `manifests.txt` | every manifest path ever published, oldest first | `text/plain` · 324 863 B | nothing — it is release tooling's file, and §4.4's repair walks it |
| `rustup/release-stable.toml` | the installer's own current version | 40 B | `rustup self update` |
| `rustup/dist/{triple}/rustup-init[.exe]` | the installer binary | | the bootstrap script, and self-update |

**The manifest is one TOML document per release, and it is large.** Its shape,
quoted from `channel-rust-stable.toml` as served:

```toml
manifest-version = "2"
date = "2026-09-03"

[pkg.cargo.target.x86_64-unknown-linux-gnu]
available = true
url = "https://static.rust-lang.org/dist/2026-09-03/cargo-1.98.1-x86_64-unknown-linux-gnu.tar.gz"
hash = "…"
xz_url = "https://static.rust-lang.org/dist/2026-09-03/cargo-1.98.1-x86_64-unknown-linux-gnu.tar.xz"
xz_hash = "…"

[[pkg.rust.target.x86_64-unknown-linux-gnu.components]]
pkg = "rustc"
target = "x86_64-unknown-linux-gnu"
```

Around 21 900 lines of it: one `[pkg.{component}.target.{triple}]` table per
component and target, each naming two archives and their digests, plus a
`components` list per target that says which of them a profile pulls in. The
document's own `date` is the directory every archive in it lives under.

**Every URL inside it is absolute and names `static.rust-lang.org`**, and
that is not a problem, because rustup rewrites the prefix to the configured dist
server itself. This is the single fact that separates this tree from Terraform's
`X-Terraform-Get` and Galaxy's `download_url`: the listing is relayed with its
URLs untouched and the component fetch still arrives here.

**What rustup verifies, and what it does not.** The downloader hashes what it
reads (`src/download/mod.rs`, `with_hasher`, SHA-256) and the dist layer
compares:

- the manifest against its `.sha256` sidecar, as a hard error —
  `RustupError::ChecksumFailed`, and `src/dist/mod.rs` carries the comment that
  it is surfaced *"rather than silently treating the toolchain as up to date"*.
  A served manifest that does not match the sidecar served beside it installs
  nothing.
- each component archive against the manifest's `hash` or `xz_hash`.
- the `.asc`: **nothing**. There is no PGP or signature handling anywhere in
  `src/dist/mod.rs`; the experimental support was removed in rustup 1.26.0.

**Credentials: none.** `src/download/mod.rs` has no authorization, credential,
username or password handling of any kind, so the only way to reach an
instance that requires identity is userinfo inside `RUSTUP_DIST_SERVER`.

**The spellings.** A toolchain is a channel (`stable`, `beta`, `nightly`), a
version (`1.98.1`, `1.99.0-beta.5`), a partial version (`1.98`), or either with
a date (`nightly-2026-09-05`); the manifest for it is
`channel-rust-{that name}.toml`, with the dated forms living under `{date}/`
instead. A component archive is `{pkg}-{release version}-{triple}.tar.{gz,xz}`,
where the release version is the toolchain's *version* and never its channel
name: `nightly` resolves to `cargo-1.99.0-nightly-…`, which is why a cache
key built from the requested name and one built from the served release are
different keys (§4.3).

```mermaid
sequenceDiagram
    participant R as rustup 1.29
    participant U as static.rust-lang.org
    R->>U: GET dist/channel-rust-stable.toml.sha256
    U-->>R: 91 B · "#123;hex#125;  channel-rust-stable.toml"
    Note over R: first 20 chars == stored update-hash?<br/>equal → nothing to do
    R->>U: GET dist/channel-rust-stable.toml
    U-->>R: 898 637 B TOML · date = 2026-09-03
    Note over R: sha256(body) == sidecar,<br/>else ChecksumFailed
    Note over R: rewrite every static.rust-lang.org<br/>URL to RUSTUP_DIST_SERVER
    R->>U: GET dist/2026-09-03/rustc-1.98.1-x86_64-unknown-linux-gnu.tar.xz
    U-->>R: application/x-tar
    Note over R: sha256 == xz_hash from the manifest
    R->>U: GET dist/2026-09-03/rust-std-1.98.1-….tar.xz
    U-->>R: …one request per component in the profile
```

One install is: a sidecar, a manifest, and one archive per component in the
profile. That is the whole protocol — which is why the manifest is the only
place a policy can act, and why the sidecar cannot be relayed if the manifest
is edited.

### 5.2 The manifest is a document and a checksum

```mermaid
sequenceDiagram
    participant R as rustup
    participant B as BatleHub
    participant U as static.rust-lang.org
    R->>B: GET dist/channel-rust-stable.toml.sha256
    B->>U: GET dist/channel-rust-stable.toml (cached 300 s)
    U-->>B: manifest, 900 KB
    Note over B: version 1.98.1 blocked?<br/>→ repair via manifests.txt<br/>deny_components → edit sections<br/>sha256(served bytes)
    B-->>R: "#123;hex#125;  channel-rust-stable.toml"
    R->>B: GET dist/channel-rust-stable.toml
    Note over B: same render, same bytes
    B-->>R: manifest · X-BatleHub-Manifest: filtered
    Note over R: sha256(body) == sidecar ✓<br/>URLs rewritten to RUSTUP_DIST_SERVER
    R->>B: GET dist/2026-09-03/rust-std-1.98.1-….tar.xz
    B-->>R: streamed, cached under rust/1.98.1/…
```

The invariant: **the sidecar route and the manifest route render from the same
inputs, so they can never disagree.** Both go through one function,
`rustup::render_manifest(upstream_body, blocked, denied, repair)`, and the
sidecar is `sha256` of its output. There is no cached "filtered manifest" that
the sidecar could drift from; the metadata cache holds upstream's bytes, and
the filter runs on read (it is a linear scan of section headers, cheap against
a 1 MB document). A block added between the two requests changes both answers
together, which rustup reads as "the manifest changed" and retries — the
correct outcome.

### 5.3 Where a block becomes effective

```mermaid
flowchart TD
    A["manifest request"] --> B{"exact or alias?"}
    B -->|"exact: 1.98.1 · #123;date#125;/…"| C{"blocked?"}
    C -->|yes| D["404 — rustup: could not download<br/>nonexistent rust version"]
    C -->|no| E["serve · filtered or upstream"]
    B -->|"alias: stable · beta · nightly · 1.98"| F{"current release blocked?"}
    F -->|no| E
    F -->|yes| G["walk manifests.txt<br/>newest allowed, ≤ 21 days for dated channels"]
    G -->|found| H["serve that manifest · repaired<br/>X-BatleHub-Version"]
    G -->|none| D
    E --> I["component fetch: dist/#123;date#125;/#123;file#125;"]
    H --> I
    I --> J{"coordinate blocked?"}
    J -->|yes| K["403 from the download gate"]
    J -->|no| L["stream · cache under rust/#123;version#125;/#123;file#125;"]
```

The invariant, as RFC 0010 §5.3 states it: a release removed from a listing is
also unresolvable by exact name, and both read the blocked set on each request
rather than a snapshot. The `403` at the file is diagnosis, not enforcement —
a client that reached it has a manifest from before the block, and the
hiding is what governs resolution.

### 5.4 What the two `.sha256` mean

| File | Who reads it | Source |
| --- | --- | --- |
| `channel-rust-*.toml.sha256` | rustup, on every manifest fetch, and refuses a mismatch | **computed** from the served manifest |
| `{component}.tar.xz.sha256` | nobody in the install path — rustup verifies the tarball against the manifest's `xz_hash` | **relayed** byte-exact, an artifact of its release |

The distinction is the whole design: an edited document needs its own
checksum, an unedited file keeps upstream's. Nothing else on the tree is ever
edited, so nothing else's checksum is ever computed.

---

## 6. Detailed design

### 6.1 `crates/core` — the registry kind

`RegistryKind::Rustup` is added to the enum and to `ALL`. The wildcard-free
matches then refuse to compile until answered (`registry_kind.rs`,
`upstream_detail`, `blocking`, `listing_synthesis`, `builders.rs`), which is
the intended pressure. `supports_local_mode() = false`,
`requires_explicit_upstream_in_proxy_mode() = false`,
`is_path_addressed() = false`; the rest:

| | `rustup` |
| --- | --- |
| `listing_filter()` | `Filtered("channel manifest", ["manifest"])`, `Filtered("manifest checksum", ["manifest-sha256"])`, `Filtered("manifests.txt", ["versions"])`, `Filtered("channel-rust-stable-date.txt", ["stable-date"])` |
| `readme_support()` | `None("a toolchain release is a manifest and a set of tarballs; the dist tree carries no prose")` |
| `upstream_detail()` | `Document("versions")` — `manifests.txt`, with the date per release and the two beta spellings |
| `fetchable_by_version()` | `None("a Rust release is a manifest plus one tarball per component per target, so \"fetch this version\" needs a target and a profile")` |
| `warm_artifact()` | `Some` — for each `warm_platforms` triple, the `minimal` profile's components as `.tar.xz`, resolved from the version's manifest (§6.9) |
| `blocking_package_name()` | identity — `rust` and `rustup` are two packages and block independently |

`docs:listing-coverage` and `docs:readme-coverage` generate the registry
page's support table from the first two rows.

`DocumentKind` gains `MANIFEST = Secondary("manifest")`,
`MANIFEST_SHA256 = Secondary("manifest-sha256")`, `STABLE_DATE =
Secondary("stable-date")` and `RUSTUP_RELEASE = Secondary("rustup-release")`.
The manifest kind is qualified by the channel name in the cache key
(`manifest:nightly-2026-09-05`), the way NuGet's registration page is keyed by
package — one document per name, never one per registry.

### 6.2 `crates/core` — `services/rustup.rs` and `blocking/rustup.rs`

`services/rustup.rs` — the protocol vocabulary, no I/O:

- `ToolchainName::parse(&str)` — rustup's own grammar
  (`stable|beta|nightly|\d+\.\d+(\.\d+)?(-beta(\.\d+)?)?`, optional `-{date}`),
  with `is_exact()` and `channel()` so the handler and the filter agree on
  what an alias is. The regex is rustup's, quoted in the doc comment with the
  version it was read from.
- `coordinate_of(date: &str, file: &str) -> Result<(String /*version*/, ComponentFile)>`
  — the §4.3 table. `ComponentFile { component, target: Option<String>, ext }`
  is what warming and explore's per-version file list use.
- `Manifest<'a>` — a **section index** over the upstream text, not a TOML
  parse: `date`, `rust_version` (`[pkg.rust].version`, first token), and
  `sections: Vec<(header, byte_range)>`. Built by one pass over lines that
  start with `[`. `toml` is not used on the read path: a 900 KB document
  parsed into a value tree on every manifest request is the wrong cost for
  four fields and a list of headers, and re-serialising it would end the
  byte-exact property. `toml` **is** used in tests to assert the filtered
  output still parses, and in the air-gap composition (§6.11).
- `render_manifest(body, blocked, denied) -> Cow<str>` — returns `Borrowed`
  when nothing applies, so `upstream` is a pointer, not a copy.
- `sidecar_line(bytes, file_name) -> String`.
- `ManifestsTxt` — parse `manifests.txt` into `{version → (date, path)}`
  rows: `channel-rust-{x.y.z}.toml` lines give stable versions and their dates,
  `{date}/channel-rust-nightly.toml` and `-beta.toml` give the dated channels,
  `channel-rust-{x.y.z}-beta.{n}.toml` gives the second beta spelling for the
  same date. `newest_allowed(alias, blocked, horizon)` is the repair walk of
  §4.4.

`blocking/rustup.rs`, dispatched from `blocking::strip`:

- `DocumentKind::Versions` (`manifests.txt`) → `with_text(doc, strip_manifests_txt)`:
  drop lines whose manifest denotes a blocked coordinate. Line order and every
  other line untouched.
- `DocumentKind::MANIFEST` → the blocked check is on the *document's own*
  coordinate (`date` + `rust_version` → `1.98.1` / `nightly-{date}` /
  `beta-{date}`): blocked ⇒ `CoreError::NotFound` for an exact name, ⇒ repair
  for an alias. The repair needs a second document (`manifests.txt`) and a
  second fetch, which is the composition RubyGems' `GEM` arm and SDKMAN's
  `candidates/default` already delegate to their handler; the same is done
  here (§6.5). Denied components are then edited by `render_manifest`.
- `DocumentKind::STABLE_DATE` → the `date` of the manifest `stable` renders to.

The blocked-set spelling is normalised so `nightly-2026-09-05` and
`2026-09-05` on the `rust` package compare equal (`blocking::normalize` gains a
`Rustup` arm). A **manifest** spelling — `1.99.0-beta.5` for a release the
coordinate calls `beta-{date}` — is resolved the other way, at the admin API
when the block is written, because the mapping is a `manifests.txt` lookup and
`normalize` is a pure function no read path may pay I/O for (§11 decision 3).
The tripwire that catches a block whose renormalised spelling
matched nothing applies to both.

### 6.3 `crates/config`

- `RegistryConfig::deny_components: Vec<String>`, documented as rustup-only;
  the §4.5 rejections beside the existing `broker_url`/`index_url` checks.
- `warm_platforms` is reused with its doc comment extended to name triples.
- `CURRENT_CONFIG_VERSION` does **not** move: the field is optional and the
  kind is additive.

### 6.4 `crates/adapters` — `registry/rustup/`

A directory, per the layout rule in `CLAUDE.md`: two subtrees, a repair walk
that composes two documents, and a name grammar would crowd out the request
logic in one file.

- `client.rs` — `RustupRegistryClient { http, base, … }`.
  - `resolve_metadata(pkg)` → `HEAD {base}/dist/{date}/{file}` for a component
    (the date is recovered from the version, or for a stable release from
    `manifests.txt`, cached); `published_at` is the release date at 00:00 UTC,
    RFC 0010 §13.1's rule. For the `rustup` package a `HEAD` on the archive
    path and `published_at: None`.
  - `fetch_artifact(pkg)` → streamed from the dated path. No redirect chain:
    the tree serves its own bytes (a `302` would be a mirror's, and goes
    through `ssrf::fetch_following_redirects` as SDKMAN's does).
  - `fetch_version_document(pkg, kind)` → `MANIFEST` (`text/plain` — upstream
    serves it as `binary/octet-stream`; rustup ignores the type and a browser
    is better served by text), `Versions` (`manifests.txt`), `STABLE_DATE`,
    `RUSTUP_RELEASE`. The manifest fetch is keyed by the channel name; the
    metadata cache holds **upstream's** bytes, and the render runs on read.
  - `list_versions("rust")` → `manifests.txt`, newest last, in the coordinate
    spelling; `list_versions("rustup")` → the one version `release-stable.toml`
    names.
- `models.rs` — `ReleaseStable { schema_version, version }` and nothing else;
  the manifest deliberately has no DTO (§6.2).
- `tests.rs` — `mockito`, spanning both files: the standalone-`tests.rs`
  exception, because the tests exercise the repair walk across the client and
  the core parser.

The fixture is a real manifest, trimmed to three targets and every package
table, checked in under `crates/adapters/src/registry/rustup/fixtures/` with
the date it was taken. Trimmed rather than synthetic so the section grammar
under test is the release tooling's, not ours.

### 6.5 `crates/web` — handlers and routes

`handlers/proxy/rustup/`, prefix `/proxy/{registry}/rustup/`:

| Route | Handler |
| --- | --- |
| `GET dist/channel-rust-{name}.toml` | `manifest` — document; exact ⇒ `404` when blocked, alias ⇒ repaired; `X-BatleHub-Manifest` |
| `GET dist/channel-rust-{name}.toml.sha256` | `manifest_sha256` — computed from the render above |
| `GET dist/channel-rust-{name}.toml.asc` | `manifest_asc` — byte-exact upstream, `404` with the manifest, same header |
| `GET dist/{date}/channel-rust-{channel}.toml[.sha256|.asc]` | the same three, dated: always exact |
| `GET dist/channel-rust-{name}` | `manifest_v1` — `404`, always (§3) |
| `GET dist/channel-rust-stable-date.txt` | `stable_date` — document |
| `GET dist/{date}/{file}` | `dist_file` — `proxy_stream`, tarballs and their `.sha256` |
| `GET manifests.txt` | `manifests_txt` — filtered document |
| `GET rustup/release-stable.toml` | `release_stable` — passthrough document |
| `GET rustup/archive/{version}/{triple}/{file}` | `rustup_archive` — `proxy_stream`, package `rustup` |
| `GET rustup/dist/{triple}/{file}` | `rustup_bootstrap` — resolves the version through `release-stable.toml`, then as `rustup_archive` |

Three obligations from the existing rules:

- **Validate at the edge.** `{name}` and `{channel}` through
  `ToolchainName::parse` (a `400` for anything else — the segment reaches a
  cache key); `{date}` as `\d{4}-\d{2}-\d{2}`; `{file}` through
  `coordinate_of`, which rejects `..`, separators and a name with no version
  token; `{triple}` and `{version}` on the installer routes through
  `validate_path_safe`. `validate_coordinate` in `ProxyService::handle` and
  `ensure_safe_key` in the storage backends remain the deeper guards.
- **`body = T` on every success response.** The text documents take
  `ProtocolDocument`; `dist_file`, `rustup_archive` and `rustup_bootstrap`
  take `ArtifactBytes`; the sidecar takes a new `ChecksumLine` marker in
  `handlers/schemas.rs`, because it is neither an upstream document nor bytes.
- **Route ordering.** `dist/channel-rust-{name}.toml.sha256` must register
  before `.toml`, and both before `dist/{date}/{file}`; `manifests.txt`
  before nothing (it is at the root, but it must not match `dist/{file}`
  if that route ever appears). The conformance fixture asserts the matched
  pattern for each (§10).

The `manifest` handler is the one place the repair composes two documents:
it calls `fetch_proxy_document` for the manifest, and on a blocked alias
again for `manifests.txt`, then once more for the manifest the walk chose.
The sidecar handler calls the same function and hashes; it is the only route
whose body is not a document or an artifact.

### 6.6 `server`

`builders.rs`'s exhaustive `match` forces one arm:
`RustupRegistryClient` from `resolve_urls(&reg.upstreams, "https://static.rust-lang.org")`
with `reg.deny_components` handed to the render. No `main.rs` change.

### 6.7 Rules

`DenyLatestRule` and `BlockListRule` need nothing — they read the coordinate.
`ReleaseAgeGateRule` reads `published_at`; on this kind every `rust`
coordinate has one (the directory *is* the date) and every `rustup` coordinate
has none. The mandatory `deny_missing_timestamp` of §4.5 is RFC 0010 §6.7's
rule applied to a kind where the split is by package rather than by
availability: with `true`, the installer's own binary is refused through the
proxy and toolchains are gated; with `false`, the binary passes ungated. The
registry page states both in one line each.

A gate on `nightly` interacts with repair: a 48-hour gate blocks today's and
yesterday's nightly, and `channel-rust-nightly.toml` repairs to the newest
nightly older than that. That is the intended reading of "nothing younger than
48 hours" and it needs no special case — the gate's verdict is part of the
blocked set the walk consults.

### 6.8 `ui` and `docs`

- `ui/src/config/registryTypes.ts` — a `REGISTRY_TYPE_DEFS` entry with the
  §4.2 exports as the setup snippet, the `~/.netrc` block for authenticated
  instances, and the server block of §4.1. Labelled *Rust toolchain (rustup,
  mise)*, because nobody searches a console for "dist".
- `docs/registries/rustup.md`, with its support and endpoint tables generated
  (`task docs:readme-coverage`, `docs:listing-coverage`, `docs:endpoints`),
  and the four lines the page must carry: what `X-BatleHub-Manifest` means,
  that the `.asc` verifies only an `upstream` manifest, the 21-day walk a
  denied component can cost a dateless nightly, and the age-gate consequence
  for the installer.
- `docs/registries/index.md` and the `/registries/` sidebar in
  `docs/.vitepress/nav/en.ts` — one sidebar per page, or `task docs:audience`
  fails. `docs/registries/generic.md` keeps its `RUSTUP_DIST_SERVER` example
  and gains one line pointing here for anyone who needs policy on it.
- `ROADMAP.md` — the entry moves to done and its signature sentence is
  corrected; `docs/guide/roadmap.md` is regenerated.

### 6.9 `cli` — `rust-toolchain.toml`, and warming

`batlehub registry suggest` gains two inputs beside `.nvmrc` and `.sdkmanrc`
(`cli/src/api/suggest.rs`):

- `rust-toolchain.toml` — `[toolchain] channel = "…"`, optional `profile`,
  `components`, `targets`.
- `rust-toolchain` — one line, the channel.

Both produce the §4.1 block and the §4.2 exports. A concrete channel
(`1.98.1`, `nightly-2026-09-05`) is warmed; an alias (`stable`, `nightly`) is
reported the way `.nvmrc`'s `lts/*` is — it names no release, rustup resolves
it through the manifest. `targets` feed `warm_platforms`.

Warming a `rust` version for a triple fetches the manifest and then the
`minimal` profile's components for that target as `.tar.xz` — `rustc`, `cargo`,
`rust-std`, and `rust-mingw` where the target lists it. That is the set every
install needs; `rust-docs` and the rest arrive on first demand. No
`warm_profile` knob: `minimal` is the floor §4.5 already protects, and a
knob would be a second way to spend more than the ~120 MB per triple `minimal`
costs (rustc 80 MB, rust-std 31 MB, cargo 12 MB, all `.tar.xz`) on a
one-line file's say-so (§11 q2).

### 6.10 `tests/heavy/rustup.sh`

A heavy suite, `config.rustup.toml` beside it, `task test:rustup-heavy`, an
entry in `task test:heavy`, and a row in the `heavy-client` matrix of
`.github/workflows/test.yaml`. rustup is on the runner image and the script
records the version it asserts against (`rustup --version`, in the transcript
and quoted in the conformance fixture); the suite redirects `RUSTUP_HOME` and
`CARGO_HOME` into the run's temp directory so it can never touch the runner's
own toolchain, and takes a fresh registry name per run (the recipe every
toolchain suite already follows). The two releases are read from the
instance's own `manifests.txt` — the newest stable and the one before it —
rather than pinned here, because "the newest stable" is what claims 4 and 5
are about and a pinned pair stops being that pair six weeks after it is
written.

The suite is a **closed world**: every client process runs with `HTTP(S)_PROXY`
pointed at a closed port and only the loopback exempted (the denial `mise.sh`
§4 and `airgap.sh` use), so the sole host a Rust build can reach is this
instance. The server keeps its own egress, which is the point of a proxy.
What it proves, on the wire, through the tap:

1. The world is closed: a client process fetching `static.rust-lang.org` or
   `index.crates.io` directly fails. Asserted first and on its own, because
   every claim below is only evidence if it holds — a phase that passes with
   egress open is green for the wrong reason.
2. `rustup toolchain install <ver> --profile minimal` reads
   `channel-rust-<ver>.toml.sha256`, then the manifest, then exactly the
   `minimal` components for the host triple, through the proxy; rustup's own
   checksum passed (the sidecar rule, on an `upstream` manifest).
3. **The build.** A crate whose dependency has a transitive dependency of its
   own is resolved through a `cargo` registry on the same instance, compiled by
   the compiler installed in claim 2 — by absolute path, with that toolchain's
   `bin` first on `PATH`, so what compiles is provably what was served — and
   the binary is *run*, with the denial still in force. The index documents and
   every `.crate` are on the transcript; the program's own output is the
   assertion.
4. With the current stable blocked through the admin API, `rustup toolchain
   install <that version>` exits non-zero on rustup's own *"could not download
   nonexistent rust version"* path, its `.sha256` answered `404`, and
   **nothing** under `dist/{its date}/` was requested.
5. With the same block in force, `rustup toolchain install stable` installs the
   previous release: the response carried `X-BatleHub-Manifest: repaired` and
   `X-BatleHub-Version: <that release>`, and `rustc --version` agrees. That
   install re-fetches every component from a fresh `RUSTUP_HOME`, so
   `batlehub_artifact_cache_hits_total` moves and upstream is not asked again.
6. On a second registry with `deny_components = ["rust-docs"]`, the same
   install succeeds — rustup verified the checksum of a `filtered` manifest,
   the sentence this RFC exists for — and `rustup component add rust-docs` is
   refused on rustup's own unavailable-component path, with no request for the
   denied tarball. A second registry rather than a hot reload: the two policies
   are then in one transcript.
7. `rustup-init` fetched with `RUSTUP_UPDATE_ROOT` pointed at the proxy comes
   through `rustup/dist/{triple}/rustup-init` and runs. `sh.rustup.rs` is
   deliberately not used: it is a host the closed world forbids, which is the
   situation the route exists for.

Everything in §4.4 about rustup's behaviour is read from rustup 1.29's source;
this script is what turns "read" into "observed", and RFC 0009 §12 is the
record of what skipping that step costs.

### 6.11 Air gap (RFC 0008-bis)

A disconnected instance answers a manifest from what it holds: the bundle
carries the manifest as a held document, and the offline render marks every
`[pkg.{c}.target.{t}]` whose tarball is not held as `available = false` and
drops it from the target's component list — the same edit as
`deny_components`, driven by inventory instead of config — with the sidecar
computed as always. rustup then installs exactly the components the estate
imported and reports the rest as unavailable, on its own path. This is the row
RFC 0008-bis §4's table gains, and it is phase 6: it depends on nothing above
but the render, and ships on its own.

**Deliberately untouched**, so reviewers do not go looking:

- `crates/adapters/src/registry/path_proxy.rs` — `generic` keeps serving the
  same tree for anyone who wants it cached without policy; nothing is removed
  from it.
- `crates/core/src/services/blocking/nodedist.rs` — the closest sibling, and
  not reused: `index.tab` is a table with one row per release, a manifest is
  one release with 939 tables. The shape is different enough that sharing a
  filter would share nothing.
- `crates/core/src/rules/release_age.rs` — the gate needs no new branch; the
  date arrives through `published_at` as it does for every kind.
- The OpenPGP question — no crate is added, no `.asc` is produced. RFC
  0008-bis §11 q6 already decided it, and nothing here reopens it.

---

## 7. Security considerations

- **Trust boundary.** The proxy sits between a client that verifies (the
  manifest against a sidecar, each tarball against the manifest's hash) and an
  upstream it already trusts. After this RFC the client verifies the manifest
  against a sidecar *this instance* computed. That is a real change: the
  instance can, by construction, serve any manifest it likes with a matching
  checksum. The mitigation is that it could already — a `generic` registry
  can serve any bytes at any path, and rustup's checksum only ever proved that
  the manifest and the sidecar came from the same place. What the sidecar
  never proved, and still does not, is provenance; that was the `.asc`, which
  no client checks. An operator who needs the manifest verified to upstream
  keeps `X-BatleHub-Manifest: upstream` (no denies, no blocks on the served
  alias) and verifies the relayed `.asc` out of band.
- **The tarball hashes are upstream's.** `hash` and `xz_hash` are never
  edited, so a component that reaches the client is verified against the
  value the release process wrote. A repair serves a different manifest, not
  a modified one, and its hashes are upstream's too. The one thing the edit
  can do is *remove* a component; it cannot substitute one.
- **URL normalisation cannot redirect a fetch off-site.** The only rewrite is
  from the configured upstream's base to `https://static.rust-lang.org`, the
  string rustup then replaces with `RUSTUP_DIST_SERVER`. A manifest URL on any
  other host is left alone, which means rustup fetches it directly — the
  mirror's problem, logged as a warning naming the host when the render sees
  one, never silently proxied through the SSRF guard on the client's behalf.
- **Attacker-controlled inputs.** Every path segment is validated at the edge
  (§6.5) before it becomes a storage key; the manifest body is upstream's and
  is scanned by section header, never evaluated. A hostile upstream can make
  the filter miss a section (an `available = false` we failed to write); it
  cannot make the proxy serve a file it did not ask for.
- **Denied components fail closed.** A denied component is unavailable in the
  manifest *and* its tarball coordinate is refused at the download gate, so a
  client holding an old manifest cannot fetch it either.
- **No new unauthenticated surface, but a kind that needs one.** The two
  actions are the existing `releases:list` (manifests, checksums,
  `manifests.txt`) and `releases:read` (files), and the registry page states
  that an install needs both, as `nodedist`'s does. rustup itself sends no
  credentials (§4.2), so a `rustup` registry is in practice `anonymous` for
  both or fronted by an ingress that authenticates — the same posture the VS
  Code gallery page records (`docs/registries/vscode-marketplace.md`), with
  the same consequence: what this
  registry serves is what upstream serves, filtered, and never a private
  artifact.
- **The installer route serves a binary the client runs.** `rustup-init` is
  relayed byte-exact from a coordinate that names its version; the archive
  path and the bootstrap path serve the same bytes. rustup-init.sh does not
  verify a checksum, so the proxy is exactly as trusted as the mirror it
  replaces, no more.

---

## 8. Alternatives considered

| Alternative | Why rejected |
| --- | --- |
| Keep `generic` and add a `path_deny` glob | It can refuse a dated directory, which refuses *every* channel released that day (stable and nightly share `dist/{date}/`), and it cannot repair `stable`; rustup then fails mid-install on a `403` instead of on its own path. And it still has no version to show. |
| Filter the manifest and relay upstream's `.sha256` | Every install through the proxy fails with rustup's *"checksum failed"* (§2.3). This is what a literal reading of the roadmap entry builds. |
| Rewrite manifest URLs to the proxy's own host | rustup already rewrites them client-side; a server-side rewrite makes every manifest `filtered`, costs the byte-exact case and the `.asc`'s meaning, and buys nothing for a client with `RUSTUP_DIST_SERVER` set. |
| Delete a denied component's `[pkg.{c}]` table outright | `[profiles]` and `[renames]` still name it, and a manifest whose rename target has no table is a shape no rustup has ever been handed. `available = false` plus removal from the target lists is the state upstream itself produces for a component that failed to build, and every rustup handles it. |
| Re-sign the manifest with this instance's key | No OpenPGP without `rsa` (banned); rustup would not read it; it would replace the publisher's provenance with the estate's (RFC 0008-bis §11 q6). |
| Answer `404` for a blocked alias instead of repairing | `rustup update` on every machine in the fleet would fail until the block is lifted, which makes the block a cost the operator pays rather than a policy. `nodedist` gets the repair for free from `index.tab`'s ordering; here it has to be built, and `manifests.txt` is what makes it cheap. |
| Parse the manifest with `toml` and re-serialise | 900 KB into a value tree per request, and the output is never byte-exact, so `upstream` would not exist and neither would a verifiable `.asc`. |
| Make `nightly-{date}` and `{date}` both first-class version spellings | One coordinate, one row in explore; the normaliser accepts both on the block side, which is where the ambiguity actually arrives. |

---

## 9. Rollout and compatibility

- **Default behaviour** when not configured: nothing changes. `generic`
  registries on `static.rust-lang.org` keep working; the roadmap entry and the
  `generic` page point at this kind for policy.
- **Config migration**: none. `deny_components` is optional,
  `warm_platforms` exists, `CURRENT_CONFIG_VERSION` stays.
- **Operator prerequisites**: egress to `static.rust-lang.org` (the tree serves
  its own bytes; no CDN redirect chain). `limits.max_artifact_size_bytes`
  large enough for the combined `rust-{version}-{triple}.tar.xz` (203 MB
  today for x86_64 Linux) — rustup installs per component and never fetches
  it, but `warm` and a curious operator can; the registry page says so.
- **Migrating a fleet from `generic`**: `RUSTUP_DIST_SERVER` changes from
  `…/generic` to `…/rustup`; `RUSTUP_UPDATE_ROOT` from `…/generic/rustup` to
  `…/rustup/rustup`. Toolchains already installed are untouched; the next
  `rustup update` re-reads the manifest through the new registry.
- **Rollback**: remove the registry block and restore the `generic` one.
  Cached artifacts under `rust/…` keys stay in storage until retention takes
  them; nothing in the database references the kind beyond the registry row.

---

## 10. Test plan

- **Unit** (`crates/core/src/services/rustup.rs`): the name grammar against
  rustup's own accepted and rejected set; `coordinate_of` on every component
  name in the fixture plus `rust-src` (no target), `rustc-dev` cross-target
  extensions, and a name with no version token; `render_manifest` returns
  `Borrowed` when nothing applies, edits only the named sections, and its
  output round-trips through `toml` — the one place `toml` is on the read
  side; `sidecar_line` matches upstream's sidecar for the untouched fixture;
  `ManifestsTxt::newest_allowed` for `stable` with the newest blocked, `1.98`
  with all `1.98.z` blocked, `nightly` with a 21-day horizon that finds
  nothing.
- **Unit** (`crates/core/src/services/blocking/rustup.rs`): the `manifests.txt`
  strip; the exact-vs-alias decision; the `Rustup` normaliser and its tripwire.
- **Adapter** (`crates/adapters/src/registry/rustup/tests.rs`, `mockito`):
  manifest fetched once for a sidecar and a manifest request; `published_at`
  is the directory date at 00:00 UTC; a `404` manifest maps to `NotFound`; the
  bootstrap route resolves through `release-stable.toml`; URL normalisation
  on a non-default upstream and *only* then.
- **Integration** (`crates/web/tests/local_rustup_registry.rs`): the eleven
  routes; blocked exact ⇒ `404` on `.toml`, `.sha256` and `.asc`; blocked
  alias ⇒ `repaired` with `X-BatleHub-Version`; the sidecar of a filtered
  manifest equals the SHA-256 of the body the manifest route returns, asserted
  by fetching both; denied component absent from every target list and
  `available = false` in its own tables; `rustup_publish_traversal_version_returns_400`
  is moot (no publish) — its equivalent is a traversal in `{date}` and
  `{file}` answering `400`; `openapi_contract` sees `body = T` on every success.
- **Conformance** (`crates/web/tests/protocol_conformance.rs`): a `RUSTUP`
  fixture, each line quoting the rustup 1.29 source location that sends it
  (`dist/mod.rs` `manifest_v2_url`, `download.rs` `download_hash`,
  `manifestation.rs` `ComponentBinary::download`, `self_update.rs`, and
  `rustup-init.sh`'s `_url`).
- **Heavy** (`tests/heavy/rustup.sh`): §6.10. The load-bearing part of the
  plan.
- **Existing suites** that must pass unchanged: the whole of `crates/web`
  (every exhaustive-match answer is a compile-time proof, and
  `every_advertised_filter_is_reachable_from_dispatch` proves the four
  listings reach a filter); `tests/heavy/nvm.sh` and `sdkman.sh`, which share
  `warm_platforms`; `tests/heavy/pathproxy.sh`, which proves `generic` still
  serves the same tree.

---

## 11. Decisions and open questions

### Resolved

| # | Question | Decision |
| --- | --- | --- |
| 1 | Compute the manifest's `.sha256` or relay it? | **Compute, always, from the served bytes.** Relaying breaks every install through a filtered manifest (§2.3); computing is byte-identical to upstream's when nothing was edited, so there is no case where relaying is better. |
| 2 | Warm a configurable profile? | **No; `minimal`.** It is the floor §4.5 protects and the set every install needs. A knob is a second way to fetch 500 MB per triple from a one-line file. |
| 3 | Blocking a beta by its manifest spelling (`1.99.0-beta.5`) | **Accept both, resolved at the admin API rather than in the normaliser.** A security advisory's own spelling is then usable verbatim, and because the mapping is a `manifests.txt` lookup and `blocking::normalize` is pure, it happens once when the block is written instead of on every read. Phase 2, with the normaliser. Decided 2026-09-12. |
| 4 | Serve the `.asc` of a filtered manifest? | **Yes, byte-exact, with the header saying the manifest is not what it signs.** No client reads it; a tool that does should fail, and that failure is the truth. Dropping it would hide the edit; re-signing is banned. |
| 5 | Repair horizon for dated channels? | **21 days**, rustup's own `RUSTUP_BACKTRACK_LIMIT` default, so the proxy and the client give up at the same place. |
| 6 | Delete a denied component's tables or mark them unavailable? | **Mark `available = false` and drop it from the target lists.** It is the state upstream produces for a component that failed to build, and every rustup handles it. |
| 7 | Is `manifests.txt` a hard dependency? | **For repair, yes; for install, no.** Without it every install still works and an alias whose release is blocked answers `404`. A mirror that lacks it is warned about once at reload. |

### Still open

Nothing. The one question this draft opened is row 3 above.

---

## 12. Implementation phases

| Phase | Content |
| --- | --- |
| 1 | `crates/core`: `RegistryKind::Rustup` and its exhaustive answers; `services/rustup.rs` (name grammar, `coordinate_of`, the section index, `render_manifest`, `sidecar_line`, `ManifestsTxt`); `blocking/rustup.rs`; the `Rustup` normaliser. `crates/config`: `deny_components` and the §4.5 rules. Tree green with no route yet — the kind validates and refuses to build at startup until phase 2, so phase 1 and 2 land together (RFC 0010 §13.1's lesson). |
| 2 | `crates/adapters/src/registry/rustup/`; `builders.rs`; `handlers/proxy/rustup/` with all eleven routes, `ChecksumLine`, the conformance fixture, `local_rustup_registry.rs`. **Useful on its own**: every install through the proxy, blocked exact names, denied components, repaired aliases. |
| 3 | `tests/heavy/rustup.sh`, `config.rustup.toml`, the task and the matrix row. Runs before phase 2 is called done, not after. |
| 4 | `cli`: `rust-toolchain.toml` / `rust-toolchain` in `registry suggest`; warming of the `minimal` profile per `warm_platforms` triple. |
| 5 | `ui` entry, `docs/registries/rustup.md`, sidebar, `generic.md` pointer, `ROADMAP.md` and the regenerated roadmap page; the §13 revision note against the tree. |
| 6 | Air gap: the held-manifest render and the RFC 0008-bis table row, proven in `tests/heavy/airgap.sh`. Ships on its own. |

---

## 13. Implementation notes

Phases 1, 2, 3 and 5 landed together on 2026-09-19 and shipped in v1.3.0. This
section is written from the tree rather than kept as the work went, so it
records where the built thing differs from §1–§12 and what is still owed, and
not the order things were found in.

### 13.1 What landed

| §12 | Where it is |
| --- | --- |
| 1 | `RegistryKind::Rustup` and its exhaustive answers; `crates/core/src/services/rustup.rs` (the name grammar, `coordinate_of`, `render_manifest`, `sidecar_line`, `ManifestsTxt`); `crates/core/src/services/blocking/rustup.rs` (`strip_manifests_txt`) |
| 2 | `crates/adapters/src/registry/rustup/`; `RegistryKind::Rustup` in both `server/src/builders.rs` matches, defaulting to `https://static.rust-lang.org`; `crates/web/src/handlers/proxy/rustup/` with `render.rs` beside it; `crates/web/tests/local_rustup_registry.rs` |
| 3 | `tests/heavy/rustup.sh` and `tests/heavy/config.rustup.toml`, declared in `registry_kind_coverage.rs` as this kind's live phase |
| 5 | The `ui/src/config/registryTypes.ts` entry, `docs/registries/rustup.md` and its French translation, the `docs/.vitepress/nav/` sidebar item, and the `ROADMAP.md` row with the regenerated roadmap page |

`deny_components` is enforced end to end rather than only validated:
`ConfigSchema::validate_registry_deny_components` refuses it on any other kind
and refuses a name every profile needs, `HotConfig::deny_components` carries it
per registry, and the handler passes it to `render_manifest`, which drops the
denied `[pkg.…]` blocks and flips the per-target ones to `available = false`.

### 13.2 Six route handlers, not eleven

§12 phase 2 says "all eleven routes". The surface is the one §4 describes and
the count is six, because two of the handlers take a path pattern that covers a
family rather than a file: `rustup_dist_root` answers every channel document
under `dist/` and `rustup_dist_dated` every one under `dist/{date}/`, each
including the `.sha256` sidecar for the document beside it. The other four are
literal paths — `manifests.txt`, `rustup/release-stable.toml`,
`rustup/archive/{version}/{triple}/{file}` and `rustup/dist/{triple}/{file}` —
and the registration order in `crates/web/src/lib.rs` is literal-first for the
reason noted there. Nothing in §4 is unserved; the number in §12 counted
documents.

### 13.3 Two phases are outstanding

- **Phase 4 — `registry suggest` on `rust-toolchain.toml`.** Not built. A
  project pinning its toolchain still has to write the `[[registries]]` block by
  hand; `cli/src/api/suggest.rs` reads `mise.lock`, `mise.toml` and the usual
  manifests and does not read `rust-toolchain.toml` or `rust-toolchain`.
  `warm_platforms` exists on the registry config and is not yet driven from a
  toolchain file.
- **Phase 6 — the air-gapped manifest render.** Not built, and the gap is
  pinned rather than left to be discovered:
  `a_rustup_channel_is_refused_while_its_held_dist_files_are_served` in
  `crates/web/tests/air_gap.rs` asserts the *refusal* — an `[air_gap]` instance
  holding the component tarballs still has no channel manifest to resolve them
  through, because a manifest is composed at import for the four kinds RFC
  0008-bis §13.6 names and `rustup` is not one of them. `listing_synthesis.rs`
  has no `RegistryKind::Rustup` arm, which is the air gap CLAUDE.md's step 5
  warns is silent: it costs no compile error and no failing test. The test is
  what makes it loud.

One phase-5 residue: `docs/registries/generic.md` gained no pointer at
`rustup`, so the paragraph that sends a reader from a `generic` mirror of
`nodejs.org/dist` to [`nodedist`](/registries/nodedist) has no counterpart
sending them from a mirror of `static.rust-lang.org` to this kind.
