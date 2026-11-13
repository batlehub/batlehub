# Go toolchain (mise)

Proxy and cache Go's toolchain distribution (`dl.google.com/go`) as a *typed* registry, so a Go release can be **blocked** rather than merely cached. Every archive is checked against Go's own release document (`go.dev/dl/?mode=json&include=all`) and verified against its `sha256` before it is cached; the `.sha256` beside each archive is answered from that document. Read by mise's `core:go`. Go *modules* are a different protocol: they go through a [Go module proxy](/registries/goproxy) registry.

## At a glance

| Property | Value |
|---|---|
| **Config type** | `godist` |
| **Default upstream** | `dl.google.com/go` for the archives, `go.dev/dl/?mode=json&include=all` (`metadata_url`) for the release document |
| **Modes** | proxy-only |
| **Addressing** | one package (`go`), one version per release (`1.25.4`, without the `go` prefix), one archive per platform |
| **Private publish** | proxy-only |
| **Air gap** | held archives and their `.sha256` served by path; `index.json` composed from the held archives. mise's version list comes from its versions host, so an air-gapped install is lock-driven |

## Proxy setup

mise downloads `{go.download_mirror}/go{version}.{os}-{arch}.tar.gz` and then `{that URL}.sha256`, whose body it reads as the whole digest. One rewrite rule sends both here. Replace `<registry>` with your configured registry name:

```toml
# ~/.config/mise/config.toml
[settings.url_replacements]
"regex:^https://dl\\.google\\.com/go/(.+)" = "https://batlehub.example.com/proxy/<registry>/godist/$1"
```

Setting `MISE_GO_DOWNLOAD_MIRROR=https://batlehub.example.com/proxy/<registry>/godist` does the same for one shell.

mise does **not** read its list of Go versions from this registry: it asks its versions host first (`data/go.toml`, which a [mise versions host](/registries/mise-versions) registry serves) and falls back to `git ls-remote https://github.com/golang/go`. Route the versions host too, or `mise install go@1.25` resolves against the public list.

## Authentication

mise sends the `~/.netrc` entry for the host a rule rewrote to, so an authenticated instance needs one entry for the proxy host:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Your administrator's registry block:

```toml
[[registries]]
name      = "go-toolchain"
type      = "godist"
mode      = "proxy"                        # the only mode: no publish protocol
upstreams = ["https://dl.google.com/go"]  # the default: archives and .sha256
# metadata_url = "https://go.dev/dl/?mode=json&include=all"  # the default

[registries.rbac]
# index.json is a listing (`releases:list`); the archives are reads (`releases:read`).
anonymous = ["releases:read", "releases:list"]
```

## Blocked versions

A block is written on `go@1.25.4` — the version as mise spells it; the document's `go1.25.4` matches it too. The version is read from the file name, so a blocked release is refused with `403` on its archive **and** its `.sha256`, whichever URL the client builds and whatever a `mise.lock` pins. `index.json` drops the release. mise's own list, which comes from the versions host, still shows it: the refusal is the download's, and mise reports it as a failed download.

A file the release document does not list — another version, another platform, a typo — is `404` without a request to the upstream, so a client cannot make this registry fetch an arbitrary path.

See [blocking a package version](/guide/admin-policies#block-a-package-version) and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered).

## Notes

- **No age gate.** Go's release document carries no release date, so a `release_age_gate` here would have nothing to read and would pass every version. The server refuses one on a `godist` registry at load rather than let it look like a quarantine.
- **Checksums come from another host.** The digest is `go.dev`'s, the bytes are `dl.google.com`'s, and the cache verifies one against the other before it stores anything. mise verifies the archive against the `.sha256` this registry answers, which is the same digest — so a `mise.lock` committed with its own checksum stays the end-to-end defence.
- **Migrating from a `go-dl` generic mirror.** Change `type`, drop `path_allow`, and point the rule at `…/godist/` instead of `…/generic/`. Cached archives do not carry over — `generic` stores under `{registry}/repo/_/{path}`, `godist` under `{registry}/go/{version}/{file}` — so the cost is one cold fetch per release still in use.

## Endpoints

::: details Show every endpoint

<!-- BEGIN endpoints: proxy/godist -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/godist/{file}` | One archive of one Go release, or its `.sha256`. |
| `GET` | `/proxy/{registry}/godist/index.json` | Go's release document, blocked releases removed. |
<!-- END endpoints -->

:::

## See also

- [Node distributions](/registries/nodedist) — the same design, one toolchain over
- [Go module proxy](/registries/goproxy) — Go *modules*, not the toolchain
- [mise versions host](/registries/mise-versions) — where mise reads its list of Go versions
- [Generic mirror](/registries/generic) — the same files, cached with no policy
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
