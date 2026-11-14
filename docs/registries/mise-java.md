# Java (mise)

Proxy and cache the JVM builds mise's `core:java` installs, as a *typed* registry, so a JDK can be **blocked**, age-gated and air-gapped rather than merely cached. mise resolves every Java version through one metadata document per release type, OS and architecture on `mise-java.jdx.dev`, then downloads the URL of the row it picked from one of eleven vendor hosts. This registry serves both: the document, filtered, and each row's archive, fetched from its vendor and verified against the row's checksum before it is cached.

## At a glance

| Property | Value |
|---|---|
| **Config type** | `mise-java` |
| **Default upstream** | `https://mise-java.jdx.dev` for the documents; each archive from its row's vendor host |
| **Modes** | proxy-only |
| **Addressing** | one package (`java`), one version per build, spelled as mise keys it (`temurin-21.0.5+11.0.LTS`, `temurin-jre-21.0.5+11.0.LTS`, `zulu-21.38.21.0`) |
| **Private publish** | proxy-only |
| **Air gap** | held archives served by their vendor URL; the metadata document composed from the held archives' rows, per platform. A lock-driven install needs nothing else |

## Proxy setup

mise reads `https://mise-java.jdx.dev/jvm/{type}/{os}/{arch}.json` and downloads the `url` of the row it chose — or, with a `mise.lock`, the URL the lock recorded, without reading the document at all. The URLs in the document are served **unchanged**, because `mise.lock` records them, so the rewrite needs one rule for the metadata host and one per vendor prefix. Replace `<registry>` with your configured registry name:

```toml
# ~/.config/mise/config.toml
[settings.url_replacements]
"regex:^https://mise-java\\.jdx\\.dev/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/$1"
"regex:^https://aka\\.ms/download-jdk/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/aka.ms/download-jdk/$1"
"regex:^https://cache-redirector\\.jetbrains\\.com/intellij-jbr/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/cache-redirector.jetbrains.com/intellij-jbr/$1"
"regex:^https://cdn\\.azul\\.com/zulu/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/cdn.azul.com/zulu/$1"
"regex:^https://cdn\\.azul\\.com/zulu-embedded/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/cdn.azul.com/zulu-embedded/$1"
"regex:^https://corretto\\.aws/downloads/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/corretto.aws/downloads/$1"
"regex:^https://d3pxv6yz143wms\\.cloudfront\\.net/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/d3pxv6yz143wms.cloudfront.net/$1"
"regex:^https://developers\\.redhat\\.com/content-gateway/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/developers.redhat.com/content-gateway/$1"
"regex:^https://download\\.bell-sw\\.com/vm/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.bell-sw.com/vm/$1"
"regex:^https://download\\.java\\.net/java/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.java.net/java/$1"
"regex:^https://download\\.oracle\\.com/graalvm/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.oracle.com/graalvm/$1"
"regex:^https://download\\.oracle\\.com/java/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.oracle.com/java/$1"
"regex:^https://gds\\.oracle\\.com/download/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/gds.oracle.com/download/$1"
"regex:^https://github\\.com/SAP/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/SAP/$1"
"regex:^https://github\\.com/Tencent/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/Tencent/$1"
"regex:^https://github\\.com/TravaOpenJDK/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/TravaOpenJDK/$1"
"regex:^https://github\\.com/adoptium/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/adoptium/$1"
"regex:^https://github\\.com/bell-sw/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/bell-sw/$1"
"regex:^https://github\\.com/dragonwell-project/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/dragonwell-project/$1"
"regex:^https://github\\.com/graalvm/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/graalvm/$1"
"regex:^https://github\\.com/ibmruntimes/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/ibmruntimes/$1"
```

`batlehub-cli setup apply --only mise` writes the whole block from the registry's own `vendor_hosts`. **These rules must come before any `github` registry's rules**: mise applies the first rule that matches, and a JDK on `github.com/adoptium/` sent to the `github` kind is cached but never blocked. `setup` orders them that way; a hand-written block has to.

## Authentication

mise sends the `~/.netrc` entry for the host a rule rewrote to, on the document and on the archive alike, so an authenticated instance needs one entry for the proxy host:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

The document is a listing (`releases:list`), an archive a read (`releases:read`). Your administrator's registry block:

```toml
[[registries]]
name = "<registry>"
type = "mise-java"
mode = "proxy"                              # the only mode: no publish protocol
# upstreams = ["https://mise-java.jdx.dev"]  # the default
# release_types = ["ga"]                     # the default; "ea" is opt-in
# vendor_hosts / redirect_hosts default to the measured vendor set

[registries.rbac]
anonymous = ["releases:read", "releases:list"]
```

## Vendor hosts and redirects

A row is served, and its archive fetched, only when its URL is `https://` and sits under one of `vendor_hosts`. An entry is `host/prefix/` — a path prefix ending in `/` — or a bare host where the host is one vendor's. A bare `github.com` is refused at load: it would admit any account's release assets, so GitHub entries name the owner. The default is the set measured across every document on 2026-11-14:

```toml
vendor_hosts = [
  "aka.ms/download-jdk/", "cache-redirector.jetbrains.com/intellij-jbr/",
  "cdn.azul.com/zulu/", "cdn.azul.com/zulu-embedded/", "corretto.aws/downloads/",
  "d3pxv6yz143wms.cloudfront.net/", "developers.redhat.com/content-gateway/",
  "download.bell-sw.com/vm/", "download.java.net/java/", "download.oracle.com/graalvm/",
  "download.oracle.com/java/", "gds.oracle.com/download/",
  "github.com/SAP/", "github.com/Tencent/", "github.com/TravaOpenJDK/", "github.com/adoptium/",
  "github.com/bell-sw/", "github.com/dragonwell-project/", "github.com/graalvm/",
  "github.com/ibmruntimes/",
]
```

Five of those redirect to another host. Every hop of a download must be `https://`, land on `vendor_hosts` or `redirect_hosts`, and resolve to a public address; anything else is a `502` and nothing is cached. The default `redirect_hosts` is the measured set: `download.visualstudio.microsoft.com` (from `aka.ms`), `d2xrhe97vsfxuc.cloudfront.net` (from `cache-redirector.jetbrains.com`), `objectstorage.uk-london-1.oraclecloud.com` (from `gds.oracle.com`), `access.cdn.redhat.com` (from `developers.redhat.com`) and `release-assets.githubusercontent.com` (from `github.com`).

A row whose host is in neither list is dropped from the served document — mise then answers `no metadata found for version …`, as for a version that never existed — and is counted in `batlehub_mise_java_rows_dropped_total{reason="host"}` and logged at `WARN` with the host.

## Blocked versions

A block is written on the coordinate mise keys the build by: `java@temurin-21.0.5+11.0.LTS`. The JRE of the same release is its own coordinate (`temurin-jre-21.0.5+11.0.LTS`); the glibc and musl builds of a release share one. A blocked build leaves every metadata document this registry serves, so `mise install java@temurin-21.0.5` fails with mise's own `no metadata found for version temurin-21.0.5 on linux-x64` and never asks for the archive. A lock that pins the archive's URL skips the document, and the download route refuses the same build with `403` and the block's reason: the URL is resolved to its row, and the row's coordinate is what the block is checked against.

A `release_age_gate` reads each row's `created_at`: a build younger than the gate leaves the document and its download is refused. Every row is dated, so the gate must say what an undated coordinate is (`deny_missing_timestamp`); one is undated only when no document lists its row any more.

See [blocking a package version](/guide/admin-policies#block-a-package-version) and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered).

## Notes

- **The client's URL is a lookup key, never a fetch target.** `dl/{host}/{path}` is resolved to the row of a document this registry fetched from its own upstream; a URL no document lists is `404` without any request. The index is per process and searched on a miss — a lock-driven install asks for the archive without reading a document, so the first download after a start fetches the document its file name hints at. The resolved coordinate is kept in the cache store, and an archive already held is served by its key even after its row is withdrawn.
- **Weak checksums are kept.** 30 % of the rows carry a `sha1:` checksum (every Liberica build), an `md5:` one or none. SHA-256 and SHA-512 rows are verified at cache write; SHA-1 rows are verified with SHA-1; `md5:` and checksum-less rows are cached unverified. Each such cache fill is counted in `batlehub_mise_java_weak_downloads_total{checksum}` and logged at `INFO` with the coordinate. mise verifies the archive against the same row, and a committed `mise.lock` pins the checksum it saw first.
- **A catalogue that shrinks implausibly is refused.** A document that does not parse, or has more than a quarter fewer rows than the last good one, is not served: the last good one is, with a `WARN` and `batlehub_mise_java_rows_dropped_total{reason="implausible"}`.
- **Licences.** The proxy caches vendor JDKs under their own licences; whether Oracle's terms let an estate redistribute a build internally is the operator's question, as it is for [SDKMAN](/registries/sdkman).

## Endpoints

::: details Show every endpoint

<!-- BEGIN endpoints: proxy/mise-java -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/mise-java/dl/{path}` | One archive, by its row's URL. |
| `GET` | `/proxy/{registry}/mise-java/jvm/{release_type}/{os}/{arch}.json` | One metadata document, filtered. |
<!-- END endpoints -->

:::

## See also

- [SDKMAN](/registries/sdkman) — the JVM for `sdk`, through its own candidates API
- [Go toolchain](/registries/godist) and [Node distributions](/registries/nodedist) — the same design for mise's other toolchains
- [mise](/use/mise) — every mise backend and the route it takes
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
