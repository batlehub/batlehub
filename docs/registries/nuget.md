# NuGet (.NET)

Proxy and cache the NuGet gallery for `dotnet`, or host private packages. BatleHub synthesises the v3 service index (`index.json`) so all resource URLs point back at the proxy, gated by RBAC and the release-age gate.

## At a glance

| | |
|---|---|
| **Config type** | `nuget` |
| **Default upstream** | `api.nuget.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `dotnet nuget push` |
| **Air gap** | offline, the flat index is composed from the held packages and the registration page from each package's `.nuspec` read at import; `dotnet restore` reads the flat index |

## Proxy setup

Add the source with the CLI. Replace `<registry>` with your configured registry name:

```sh
dotnet nuget add source \
  https://batlehub.example.com/proxy/<registry>/nuget/v3/index.json \
  --name batlehub \
  --username __token__ \
  --password $BATLEHUB_TOKEN
```

Then install as usual:

```sh
dotnet add package Newtonsoft.Json
dotnet restore
```

## Publishing (local / hybrid)

NuGet packages are `.nupkg` files (ZIP archives containing a `.nuspec` manifest). BatleHub implements the [NuGet v3 protocol](https://learn.microsoft.com/en-us/nuget/api/overview), compatible with `dotnet` CLI, `nuget.exe`, and any NuGet v3 client.

### Config

```toml
[[registries]]
type = "nuget"
name = "internal-nuget"
mode = "local"          # or "hybrid" to fall back to api.nuget.org

[registries.rbac]
user  = ["releases:read"]
admin = ["*"]
```

For hybrid mode add `upstreams = ["https://api.nuget.org"]`.

### Configure dotnet / nuget.config

**CLI (one-time):**
```bash
dotnet nuget add source \
  https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json \
  --name internal-nuget \
  --username __token__ --password <api-token>
```

**`nuget.config` (project-level):**
```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="internal-nuget"
         value="https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json" />
  </packageSources>
  <packageSourceCredentials>
    <internal-nuget>
      <add key="Username" value="__token__" />
      <add key="ClearTextPassword" value="<api-token>" />
    </internal-nuget>
  </packageSourceCredentials>
</configuration>
```

### Publish with dotnet nuget push

Pack your project first, then push:

```bash
dotnet pack MyLib.csproj -c Release

dotnet nuget push bin/Release/MyLib.1.0.0.nupkg \
  --api-key <api-token> \
  --source https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json
```

The publish endpoint accepts `multipart/form-data` (as sent by `dotnet nuget push`). On success it returns **201 Created**.

### Yank a version

```bash
curl -X DELETE \
  -H "Authorization: Bearer <api-token>" \
  "https://batlehub.example.com/proxy/internal-nuget/nuget/v2/package/mylib/1.0.0"
```

### Consume a package

```bash
# Add the package — dotnet fetches the index, resolves the version, downloads the .nupkg
dotnet add package MyLib --version 1.0.0 --source internal-nuget

# Restore all project dependencies
dotnet restore
```

### Verify

```bash
# Service index should return JSON with "version": "3.0.0"
curl -s https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json | jq '.version'

# Flat container version list after publish
curl -s https://batlehub.example.com/proxy/internal-nuget/nuget/v3/flat/mylib/index.json
# → {"versions":["1.0.0"]}
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/nuget -->
| Method | Path | Description |
|--------|------|-------------|
| `PUT` | `/proxy/{registry}/nuget/api/v2/package` | Publish a `.nupkg` to the local registry. |
| `PUT` | `/proxy/{registry}/nuget/api/v2/symbolpackage` | Publish a `.snupkg` symbol package. |
| `DELETE` | `/proxy/{registry}/nuget/v2/package/{id}/{version}` | Yank (unlist) a NuGet package version from the local registry. |
| `GET` | `/proxy/{registry}/nuget/v3/autocomplete` | `SearchAutocompleteService` — package-id completion. |
| `GET` | `/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}` | Download a NuGet package artifact (`.nupkg`, `.nuspec`, checksum, etc.). |
| `GET` | `/proxy/{registry}/nuget/v3/flat/{id}/index.json` | Return the list of available versions for a NuGet package (flat container). |
| `GET` | `/proxy/{registry}/nuget/v3/index.json` | Return a NuGet v3 service index pointing all resource URLs back to this proxy. |
| `GET` | `/proxy/{registry}/nuget/v3/query` | Search for NuGet packages. |
| `GET` | `/proxy/{registry}/nuget/v3/registration5/{id}/index.json` | Return NuGet v3 registration metadata for a package. |
| `GET` | `/proxy/{registry}/nuget/v3/vulnerabilities/index.json` | Proxy the NuGet vulnerability database index. |
| `GET` | `/proxy/{registry}/nuget/v3/vulnerabilities/page/{page}` | Proxy a single page of NuGet vulnerability records. |
<!-- END endpoints -->

---

## Blocked versions

Both of NuGet's listing documents hide an administratively blocked version.

- The **flat index** (`/v3/flat/{id}/index.json`) — what `dotnet restore`
  resolves a version range against — drops the version outright.
- The **registration pages** drop the leaf and recompute each page's `count`,
  `lower` and `upper`; a page left empty is removed. Registrations whose pages
  are served by URL rather than inline pass through unfiltered and are logged.

Version spellings are folded before comparison, so a block recorded as
`1.0.0.0` hides a listing that spells the same release `1.0.0`.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Pass the BatleHub token as the source password (username `__token__`), or as `--api-key` on push. The `X-NuGet-ApiKey` header is normalised to `Authorization: Bearer` internally, so `--api-key $BATLEHUB_TOKEN` is accepted as a Bearer token.

## Notes

- `dotnet list package --vulnerable` works automatically — BatleHub advertises a `VulnerabilitiesUrl` resource in the v3 service index and proxies the upstream vulnerability catalogue. See [Using BatleHub → security auditing](/use/#security-audit).
- A `401` on push usually means the token lacks `releases:publish` (or admin) on the registry.

## Search

``dotnet package search`` is answered in three steps: a cached result for that query, then the
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

## A plain-HTTP instance needs an explicit opt-in

NuGet refuses an `http:` package source outright. If BatleHub is not behind TLS —
a local instance, or an internal network — the source entry needs
`allowInsecureConnections`:

```xml
<add key="batlehub" value="http://localhost:8080/proxy/my-nuget/nuget/v3/index.json"
     allowInsecureConnections="true" />
```

Measured against dotnet 10.0.400. Without it the CLI stops before making any
request, with a message pointing at <https://aka.ms/nuget-https-everywhere>.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
