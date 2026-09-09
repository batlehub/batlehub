# Forgejo / Gitea

Proxy and cache release assets, source archives, and raw files from a [Forgejo](https://forgejo.org) or Gitea instance. It reuses the GitHub-style URL scheme, and also proxies the Forgejo/Gitea package registry under `/api/packages/`.

## At a glance

| | |
|---|---|
| **Config type** | `forgejo` |
| **Default upstream** | `codeberg.org` |
| **Modes** | proxy-only |
| **Addressing** | per-package |
| **Private publish** | ❌ proxy-only |
| **Air gap** | offline, the release listing and the release by tag are composed from the held assets |

## Proxy setup

Set `upstreams` to the instance root (e.g. `https://codeberg.org`). Address a repository by `<owner>/<repo>`; replace `<registry>` with your configured registry name and add `-H "Authorization: Bearer $BATLEHUB_TOKEN"` when required:

```bash
REG="https://batlehub.example.com/proxy/<registry>"

# List releases / get a release by tag
curl $REG/<owner>/<repo>/releases
curl $REG/<owner>/<repo>/releases/tags/v1.0.0

# Download a release asset by filename
curl -L -O $REG/<owner>/<repo>/releases/download/v1.0.0/app.tar.gz

# Source tarball / zip for a tag, branch, or commit
curl -L -O $REG/<owner>/<repo>/tarball/v1.0.0
curl -L -O $REG/<owner>/<repo>/zipball/v1.0.0

# Raw file
curl -L $REG/<owner>/<repo>/raw/main/README.md
```

A `forgejo` registry also transparently caches the Forgejo/Gitea **package registry** at `/api/packages/{owner}/…` — ideal for the **generic** package registry:

```bash
curl -L -O https://batlehub.example.com/proxy/<registry>/api/packages/<owner>/generic/<name>/<version>/<file>
```

For **ecosystem** registries (npm, Maven, PyPI, Composer, NuGet, …), point the matching typed adapter at the package endpoint instead so metadata URLs are rewritten and cached.

## Blocked versions

The release listing drops a blocked release, newest-first order intact, so a
client never selects a release whose assets it will then be refused.

A release is identified by its **tag**, and the same release is tagged `1.2.3`
in one repository and `v1.2.3` in the next. A block matches either spelling, so
it does not depend on which habit the repository follows.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## What a version is here

A package registry names immutable things. A forge does not: `main` is
whatever it points at when you ask, and a tag can be moved. So every forge
request resolves its ref to a **commit** before anything is fetched, and that
commit is what the cache, the metadata and the supply-chain verdict key on
([RFC 0019](/rfc/0019-git-forge-registries-refs-releases-raw)).

Every forge response carries the resolution:

| Header | Meaning |
| --- | --- |
| `X-BatleHub-Ref-Kind` | `tag`, `branch` or `commit` |
| `X-BatleHub-Resolved-Commit` | the commit that answered |
| `X-BatleHub-Ref-Previous-Commit` | what the same ref answered with last time, when that differs |
| `X-BatleHub-Ref-Requested` | the ref as you spelled it — the only place it survives once the coordinate has become the commit |

Three facts about a ref change what a request gets, and
`[registries.refs]` decides what each one does:

| Fact | Code | Default |
| --- | --- | --- |
| the request followed a branch | `MUTABLE_REF` | `warn` — served, and said so |
| a tag now resolves to a different commit | `TAG_MOVED` | `deny` |
| a release asset's digest changed since the bytes were cached | `ASSET_REPLACED` | `deny` |

A moved tag and a replaced asset are the two forge-native ways to swap bytes
under a stable coordinate, which is why they are refused by default. The first
resolution of a tag is always trusted — there is nothing to compare it with —
and a change is noticed on the next resolution after `tag_ttl_secs` (one hour
by default).

On a registry with [`[registries.security]`](/guide/configuration#security)
these facts ride the version's verdict, so a warned branch answers with
`X-BatleHub-Verdict: warned` and `batlehub why` explains it. Without that
section there is no verdict to carry them: a `deny` is a plain `403` naming
the code, and a `warn` is the headers above. That is the honest degradation,
and it is the reason `X-BatleHub-Ref-Previous-Commit` exists.

```toml
[registries.refs]
branch_ttl_secs = 60      # how long a branch → commit resolution is trusted
tag_ttl_secs    = 3600    # and a tag's; also the detection latency of a moved tag
mutable_refs    = "warn"  # "warn" (default) | "deny"
tag_moved       = "deny"  # "deny" (default) | "warn"
```

## Raw files

**Raw content is off unless you turn it on.** It used to be implicitly served;
a registry that has no `[registries.raw]` block now refuses it, and the refusal
says so in the body.

```toml
[registries.raw]
enabled        = true
max_size_bytes = 10485760          # 10 MiB; must not exceed [limits].max_artifact_size_bytes
repos          = ["forgejo/*"]         # owner/repo globs; empty allows any repository
require_pinned = false             # true refuses a branch ref for raw content
scripts        = "warn"            # "warn" | "deny" | "ignore"
```

A file over the ceiling is refused, never truncated. `scripts` looks at the
file's extension (`.sh`, `.bash`, `.ps1`, `.py`, `.bat`, …) and, under `deny`,
at its first bytes as well — so an extensionless payload that starts with a
shebang is refused too. `scripts` defaults to **`deny`** on a registry that has
a `[registries.security]` profile and to `warn` on any other: opting into a
quarantine is opting into "nothing unscanned is served", and a script passed
through is the one artifact no scanner here reads.

Raw content is always served as `application/octet-stream` with
`X-Content-Type-Options: nosniff`, so a raw HTML file cannot execute as a
document on this origin.

## Typed API reads

The release listing and the release by tag are served already. Three more
read-only JSON routes are available on request:

```toml
[registries.api_reads]
families = ["tags", "commits", "branches"]
```

| Family | Route |
| --- | --- |
| `tags` | `GET /proxy/<registry>/<owner>/<repo>/tags` |
| `commits` | `GET /proxy/<registry>/<owner>/<repo>/commits/<sha>` |
| `branches` | `GET /proxy/<registry>/<owner>/<repo>/branches/<name>` |

They answer BatleHub's own shape, not the forge's: there is no upstream URL in
them to follow, no field that means something different per forge, and no room
for a passthrough to grow into one. A family the registry did not ask for
answers `404`. `contents` and `git/blobs` are never accepted — they are raw
content by another door, and `[registries.raw]` is where that decision lives.

**Release documents are rewritten.** `tarball_url`, `zipball_url` and each
asset's `browser_download_url` are repointed at this proxy, and the forge's own
API links are removed. A client that reads the release document instead of
building a path — `mise`, `gh` — therefore stays behind the proxy, with its
policy, its cache and its audit trail.

## Authentication

Pass a BatleHub token as a Bearer header (`-H "Authorization: Bearer $BATLEHUB_TOKEN"`) when the registry's RBAC requires it. For **private instances**, configure a bearer token as the registry's upstream auth in the server config.

## Notes

- Proxy/cache only: the first request is streamed from upstream and cached.
- The URL scheme is identical to [GitHub](/registries/github).

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
