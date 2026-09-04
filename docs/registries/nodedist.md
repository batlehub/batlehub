# Node distributions (nvm, fnm, n, mise)

Proxy and cache the `nodejs.org/dist` tree as a *typed* registry, so a Node release can be **blocked** rather than merely cached. `index.tab` and `index.json` are filtered listings; the tarballs, `SHASUMS256.txt` and its detached signatures are served byte-exact under `{version}/{file}`. The same tree is read by nvm, fnm, `n`, volta and mise, which is why the kind is named after the protocol and not after one of them.

## At a glance

| | |
|---|---|
| **Config type** | `nodedist` |
| **Default upstream** | `nodejs.org/dist` (io.js takes a second registry pointed at `iojs.org/dist`) |
| **Modes** | proxy-only |
| **Addressing** | one package (`node`), one version per release, one file per platform |
| **Private publish** | ❌ proxy-only |

## Proxy setup

Each manager reads its own mirror variable. Export it before sourcing `nvm.sh` — in `/etc/profile.d`, a `Containerfile`, or a CI job's `env:` block. Replace `<registry>` with your configured registry name:

```sh
export NVM_NODEJS_ORG_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"   # nvm
export FNM_NODE_DIST_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"    # fnm
export N_NODE_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"           # n
export NODEJS_ORG_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"       # mise

nvm ls-remote        # reads index.tab through the proxy
nvm install 22.11.0  # SHASUMS256.txt and the tarball, cached under node/v22.11.0/
```

Your administrator's registry block:

```toml
[[registries]]
name      = "node"
type      = "nodedist"
mode      = "proxy"                       # the only mode: no publish protocol
upstreams = ["https://nodejs.org/dist"]  # the default; io.js takes its own block

[registries.rbac]
# index.tab / index.json are listings (`releases:list`); the files are reads
# (`releases:read`). An install needs both.
anonymous = ["releases:read", "releases:list"]
```

`path_allow` is refused here: the kind is typed, not path-addressed. An operator who wants a Node mirror with no policy at all keeps the [generic mirror](/registries/generic); `nodedist` exists for the block.

## Blocked versions

nvm resolves **every** install through `index.tab` — including a fully specified `nvm install 22.11.0` — and prints its own *"Version '22.11.0' not found"* when the row is absent. A blocked release is removed from `index.tab` (header preserved: nvm strips line 1 unconditionally, so a lost header would eat the newest release) and from `index.json`, the same table fnm and mise read. No download is attempted, and nothing under the release directory is requested.

The `lts/*` aliases nvm derives from the `lts` column move by construction: removing the newest Jod row makes the next surviving Jod row the alias. The one honest limit: nvm caches those aliases under `$NVM_DIR/alias/lts/` when it last ran `nvm ls-remote`, so a release blocked *after* that can still be requested by alias — at which point the tarball fetch is refused with `403` and nvm reports a download failure rather than a clean not-found. The block holds; the message degrades.

`SHASUMS256.txt` is **never rewritten**: nvm verifies every download against it and a sibling `.asc`/`.sig` signs it. Blocking a release removes its row from the listing; it does not doctor its checksums.

See [blocking a package version](/guide/admin-policies#block-a-package-version) and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered).

## Authentication

nvm and fnm build their own `curl` command and have nowhere to put a header. libcurl reads `~/.netrc` without being asked, so an authenticated instance needs one entry for the proxy host:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

## Notes

- **The age gate works, with one decision to make.** The release date is read from column two of `index.tab`, so current releases carry a timestamp and a `release_age_gate` genuinely holds a release published yesterday. A release the index no longer lists reaches the gate with none; on this kind `deny_missing_timestamp` is **mandatory** — `true` refuses de-listed releases, `false` serves them — because inheriting npm's default silently is how an operator ends up believing a toolchain is quarantined when it is not. The date is the index's day at 00:00 UTC, so a 24-hour gate never holds for less than a day.
- **io.js** is a second `nodedist` registry pointed at `https://iojs.org/dist`. Its package is `iojs`, decided by the upstream URL; its `index.tab` has nine columns where Node's has eleven, and both are read by header name.
- **Migrating from a `generic` mirror.** Change `type`, drop `path_allow`, and point the client variable at `…/nodedist` instead of `…/generic`. Cached artifacts do not carry over — `generic` stores under `{registry}/repo/_/{path}`, `nodedist` under `{registry}/node/{version}/{file}` — so the cost is one cold fetch per release still in use.
- **Cache warming needs a platform.** `warm_packages = ["node@v22.11.0"]` warms one tarball per entry of `cache.warm_platforms` (`linux-x64`, `darwin-arm64`, …), defaulting to the platform this server runs on. `batlehub-cli registry suggest` reads `.nvmrc` and writes both; an alias (`lts/*`) names no release and warms nothing.

## Endpoints

<!-- BEGIN endpoints: proxy/nodedist -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/nodedist/{version}/{file}` | One file of one release: a tarball, `SHASUMS256.txt`, or a signature. |
| `GET` | `/proxy/{registry}/nodedist/index.json` | The same release table as JSON — what fnm and mise read. |
| `GET` | `/proxy/{registry}/nodedist/index.tab` | The release table nvm resolves every install through, blocked releases |
<!-- END endpoints -->

## See also

- [SDKMAN](/registries/sdkman) — the other toolchain kind of RFC 0010
- [Generic mirror](/registries/generic) — the same tree, cached with no policy
- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
