# GitLab

Proxy and cache releases, release-link assets, and source archives from a GitLab instance. Project paths may include nested groups; the release sub-path is separated by `/-/`, mirroring GitLab's own URLs. It also proxies the GitLab Packages API under `/api/v4/`.

## At a glance

| | |
|---|---|
| **Config type** | `gitlab` |
| **Default upstream** | `gitlab.com` |
| **Modes** | proxy-only |
| **Addressing** | per-package |
| **Private publish** | ❌ proxy-only |

## Proxy setup

Set `upstreams` to the instance root (e.g. `https://gitlab.com`). Replace `<registry>` with your configured registry name; add `-H "Authorization: Bearer $BATLEHUB_TOKEN"` when required:

```bash
REG="https://batlehub.example.com/proxy/<registry>"

# List releases / get a release by tag (nested groups allowed)
curl $REG/<group>/<project>/-/releases
curl $REG/<group>/<subgroup>/<project>/-/releases/v1.0.0

# Download a release link asset (matched by link name)
curl -L -O $REG/<group>/<project>/-/releases/v1.0.0/downloads/app.bin

# Source archive for a tag (format inferred from the extension)
curl -L -O $REG/<group>/<project>/-/archive/v1.0.0/source.tar.gz

# Raw file from the repository
curl -L $REG/<group>/<project>/-/raw/main/README.md
```

A `gitlab` registry also transparently caches the GitLab **Packages API** under `/api/v4/…` — ideal for the **generic** package registry:

```bash
curl -L -O https://batlehub.example.com/proxy/<registry>/api/v4/projects/<id>/packages/generic/<name>/<version>/<file>
```

For **ecosystem** registries (npm, Maven, PyPI, NuGet, Composer, …), point the matching typed adapter at the GitLab package endpoint instead so metadata URLs are rewritten and cached.

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

Three facts about a ref change what a request gets, and `[registries.refs]`
decides what each one does: following a branch is `MUTABLE_REF` (`warn` by
default), a tag that now resolves elsewhere is `TAG_MOVED` (`deny`), and a
release asset whose digest changed is `ASSET_REPLACED` (`deny`). The first
resolution of a tag is always trusted; a change is noticed on the next
resolution after `tag_ttl_secs`.

```toml
[registries.refs]
branch_ttl_secs = 60
tag_ttl_secs    = 3600
mutable_refs    = "warn"   # "warn" (default) | "deny"
tag_moved       = "deny"   # "deny" (default) | "warn"
```

GitLab resolves a tag in one call: `/repository/tags/{tag}` carries the commit
inline, and an annotated tag's own `created_at` is what dates it. Every API
call draws on the same rate-limit budget the other two forges use, so the
proxy and the scan worker cannot spend one token twice.

## Raw files

**Raw content is off unless you turn it on** — `/-/raw/{ref}/{path}` used to
be implicitly served, and a registry with no `[registries.raw]` block now
refuses it with a body that says so.

```toml
[registries.raw]
enabled        = true
max_size_bytes = 10485760
repos          = ["group/*"]
require_pinned = false
scripts        = "warn"     # "deny" by default when the registry has [registries.security]
```

## Typed API reads

```toml
[registries.api_reads]
families = ["tags", "commits", "branches"]
```

Three read-only JSON routes in BatleHub's own shape — `tags`,
`commits/{sha}`, `branches/{name}` — under `/proxy/<registry>/<project>/`.
A family the registry did not ask for answers `404`, and `contents` and
`git/blobs` are never accepted.

**Release documents are rewritten**: `assets.sources[].url` points at this
proxy's archive route, `assets.links[].url` and `direct_asset_url` at its
download route, and GitLab's own `_links` block is removed. A client that
follows the release document therefore stays behind the proxy.

## Authentication

Pass a BatleHub token as a Bearer header (`-H "Authorization: Bearer $BATLEHUB_TOKEN"`) when the registry's RBAC requires it. GitLab personal access tokens use the `PRIVATE-TOKEN` header — configure it as a custom upstream auth header on the registry to reach private projects.

## Notes

- Proxy/cache only: the first request is streamed from upstream and cached.
- The release sub-path is separated by `/-/`, exactly as in GitLab's own URLs; nested group paths are supported.
- **Provenance is `unverifiable` here, and only here.** GitLab collects a JSON evidence blob per release and signs nothing, so a release with evidence reports `PROVENANCE_UNVERIFIABLE` — informational, never a refusal on its own. A signed commit is read from `/repository/commits/{sha}/signature` and reports verified or invalid; GitHub and Forgejo never report `unverifiable`.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
