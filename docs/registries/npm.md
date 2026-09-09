# npm

Proxy and cache the npm registry, or host private npm packages. BatleHub serves the full packument metadata plus tarball downloads, gated by RBAC and the release-age gate. `npm audit` works through the proxy.

## At a glance

| | |
|---|---|
| **Config type** | `npm` |
| **Default upstream** | `registry.npmjs.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `npm publish` |
| **Air gap** | offline, the packument is composed from the held versions ([RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap)); a fresh `npm install` resolves against it |

## Proxy setup

Point npm at your registry. Replace `<registry>` with your configured registry name and set `BATLEHUB_TOKEN` if the registry requires auth:

```ini
# .npmrc (project root or ~/.npmrc)
registry=https://batlehub.example.com/proxy/<registry>/
//batlehub.example.com/proxy/<registry>/:_authToken=${BATLEHUB_TOKEN}
```

To route only a specific scope through the proxy, use `@myorg:registry=https://batlehub.example.com/proxy/<registry>/` instead. **pnpm** reads the same `.npmrc` keys unchanged.

**Yarn Berry** (Yarn 2+) does not read `.npmrc` — configure it in `.yarnrc.yml` with its own key names:

```yaml
# .yarnrc.yml
npmRegistryServer: "https://batlehub.example.com/proxy/<registry>/"
npmAuthToken: "${BATLEHUB_TOKEN}"

# Or, to route only one scope:
npmScopes:
  myorg:
    npmRegistryServer: "https://batlehub.example.com/proxy/<registry>/"
    npmAuthToken: "${BATLEHUB_TOKEN}"
```

## Publishing (local / hybrid)

### Server configuration

```toml
[[registries]]
type = "npm"
name = "internal-npm"
mode = "local"          # or "hybrid" to fall back to registry.npmjs.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

For hybrid mode add `upstreams = ["https://registry.npmjs.org"]` under the registry block.

### Client setup

Create or update `.npmrc` (per-project or `~/.npmrc`):

```ini
# Scope all @myorg packages to the private registry
@myorg:registry=https://batlehub.example.com/proxy/internal-npm/

# Auth token for that registry host
//batlehub.example.com/proxy/internal-npm/:_authToken=<your-token>
```

To use the registry for all packages (unscoped), set the global registry:

```ini
registry=https://batlehub.example.com/proxy/internal-npm/
//batlehub.example.com/proxy/internal-npm/:_authToken=<your-token>
```

### Publish

```sh
npm publish --registry https://batlehub.example.com/proxy/internal-npm/
# or, with .npmrc configured:
npm publish
```

### Verify

```sh
npm view @myorg/my-package --registry https://batlehub.example.com/proxy/internal-npm/
npm install @myorg/my-package
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/npm -->
| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/proxy/{registry}/-/npm/v1/audit/bulk` | Deprecated alias of the bulk audit endpoint — npm sends `/-/npm/v1/security/advisories/bulk`. |
| `POST` | `/proxy/{registry}/-/npm/v1/audit/quick` | Deprecated alias of the quick audit endpoint — npm sends `/-/npm/v1/security/audits/quick`. |
| `POST` | `/proxy/{registry}/-/npm/v1/security/advisories/bulk` | `npm audit`, bulk mode — the default since npm 7, on the path npm sends. |
| `POST` | `/proxy/{registry}/-/npm/v1/security/audits/quick` | `npm audit`, quick mode — on the path npm sends. |
| `GET` | `/proxy/{registry}/-/package/{package}/dist-tags` | `npm dist-tag ls`. |
| `PUT` | `/proxy/{registry}/-/package/{package}/dist-tags/{tag}` | `npm dist-tag add` — declined, with a reason the client prints. |
| `DELETE` | `/proxy/{registry}/-/package/{package}/dist-tags/{tag}` | `npm dist-tag rm`. Declined for the same reason as `add`. |
| `GET` | `/proxy/{registry}/-/ping` | `npm ping`. |
| `GET` | `/proxy/{registry}/-/v1/search` | `npm search` / `npm search --json`. |
| `GET` | `/proxy/{registry}/-/whoami` | `npm whoami`. |
| `PUT` | `/proxy/{registry}/{name}` | Publish a new npm package version (`npm publish`). |
| `GET` | `/proxy/{registry}/{package}` | Fetch package metadata (all versions / packument). |
| `GET` | `/proxy/{registry}/{package}/{version}` | Fetch package version metadata. |
| `GET` | `/proxy/{registry}/{package}/{version}/tarball` | Download npm package tarball for a specific version. |
<!-- END endpoints -->

The packument is BatleHub's own answer, not a copy of the upstream's. Two things
are rewritten before it reaches the client:

- **`dist.tarball` points back at BatleHub**, so downloads go through the proxy —
  its cache, its audit trail and its policy gates — instead of straight to the
  upstream CDN.
- **Blocked versions are removed**, and `dist-tags.latest` is recomputed to the
  newest version that is still allowed. See
  [blocking a package version](/guide/admin-policies#block-a-package-version).

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

---

## Authentication

Pass a BatleHub token as the npm auth token (`_authToken`). Anonymous access works only when the registry's RBAC grants the `anonymous` role read access.

## Notes

`npm audit` works once the registry is configured — both the quick and bulk modes are
proxied to the upstream advisory database, on the paths the npm CLI already sends:

```bash
npm audit
npm audit --fix
```

Answers are cached, and an unreachable advisory database is served from cache rather
than failed, so an outage upstream does not stop a pipeline running `npm audit`. See
[the vulnerability proxy](/use/vulnerability-proxy#_2-npm-—-npm-audit) for the cache
headers and the two deprecated aliases.

## Search

``npm search`` is answered in three steps: a cached result for that query, then the
upstream, then — when the upstream is unreachable — **the packages this registry
already holds**. An outage degrades search to what BatleHub can honestly answer
for, rather than to an error or to an empty result list.

Every response carries `X-BatleHub-Cache: hit | miss | stale`. `stale` means the
upstream could not be reached and the answer came from the cache or from the held
set, so a short result list is never silently presented as complete.

::: warning Search queries reach the upstream
Step two forwards the query string to the configured upstream. Search terms are a
record of what your organisation is looking for. Set `serve_stale = false` and
leave the registry without an upstream if you want the held-package answer and no
egress at all.
:::

Blocked versions are removed from results, and the reported total is adjusted to
match — clients paginate by offset, so a silently shortened page would make the
next one skip a result.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
