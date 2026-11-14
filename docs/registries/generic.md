# Generic mirror

A proxy-only, path-addressed mirror of any plain HTTP file tree — for upstreams that have no package protocol at all: toolchain tarballs (`nodejs.org/dist`, `static.rust-lang.org`, `dl.google.com/go`) and single-binary vendor CDNs (`get.helm.sh`, `binaries.sonarsource.com`). Every request streams `{upstream}/{path}` and caches it on the first miss. There is no publish, index, or signing model.

## At a glance

| Property | Value |
|---|---|
| **Config type** | `generic` |
| **Default upstream** | none — set `upstreams` explicitly (mandatory) |
| **Modes** | proxy-only |
| **Addressing** | path-addressed |
| **Private publish** | proxy-only |
| **Air gap** | no index to compose; a held file is served by path |

## Proxy setup

Both `upstreams` **and** a `path_allow` allowlist are mandatory — without the allowlist a mirror of a shared host would relay every unrelated path on it. Your administrator sets them in the registry config:

```toml
[[registries]]
name       = "node-dist"
type       = "generic"
mode       = "proxy"
upstreams  = ["https://nodejs.org/dist"]   # required — no default exists
path_allow = ["v*/**"]                      # required — use ["**"] to allow all
```

`path_allow` patterns are globs where `*` stays inside one path segment and only `**` crosses `/`: `v*/**` admits `v24.18.0/node-v24.18.0-linux-x64.tar.gz`, and `go*` admits `go1.25.4.linux-amd64.tar.gz` but not `go1.25/sub/file`.

::: warning Changed in RFC 0047 phase 2
Before it, `*` also crossed `/`, so `go*` admitted every path that started with `go`. A `path_allow` that relied on that now refuses those paths with `403`; write `**` where a separator must be crossed:

```toml
path_allow = ["go*"]     # before: also admitted go1.25/sub/file
path_allow = ["go*/**"]  # after: the same paths, said explicitly
```

The change fails closed: nothing is admitted that was refused before. It applies to `generic` only; `deb`, `rpm`, `pacman` and `jetbrains` keep `*` crossing `/`.
:::

The path after `/generic/` maps 1:1 onto the configured upstream. Replace `<registry>` with your configured registry name:

```bash
REG="https://batlehub.example.com/proxy/<registry>/generic"

# nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz
#   → $REG/v24.18.0/node-v24.18.0-linux-x64.tar.gz
curl -fL -o node.tar.gz $REG/v24.18.0/node-v24.18.0-linux-x64.tar.gz
```

Most toolchains expose a mirror environment variable that you point at the registry root (`…/proxy/<registry>/generic`):

```sh
export NODEJS_ORG_MIRROR=https://batlehub.example.com/proxy/node-dist/generic
export RUSTUP_DIST_SERVER=https://batlehub.example.com/proxy/rust-dist/generic
```

Tools like `mise` read these automatically, and can also route direct downloads through a `[settings.url_replacements]` block (the `mise` `[settings.url_replacements]` use case). `batlehub-cli registry suggest` scans a project (including `mise.toml` / `mise.lock`) and prints both the registry config blocks and the matching client environment variables.

## Presets

`batlehub-cli registry suggest --mise` writes these registries when `mise.lock` records a URL on their host. Each `path_allow` starts with a literal product segment, so a preset never relays the whole host; `revalidate` lists the documents mise re-reads for news ([RFC 0047](/rfc/0047-mise-backends-coverage) §6.1).

| Name | Upstream | Serves |
| --- | --- | --- |
| `hashicorp-releases` | `https://releases.hashicorp.com` | boundary, consul, nomad, nomad-pack, packer, sentinel, terraform, terraform-ls, tfc-agent, vault, waypoint |
| `hashicorp-api` | `https://api.releases.hashicorp.com/v1/releases` | the release dates of nomad-pack, sentinel and tfc-agent — the first page of ten only, since the query string is not passed upstream |
| `k8s-dl` | `https://dl.k8s.io` | kubectl, kubectl-convert, `release/stable.txt` |
| `apache-archive` | `https://archive.apache.org/dist` | ant, groovy, maven, spark, tomcat |
| `openshift-mirror` | `https://mirror.openshift.com/pub/openshift-v4/clients` | oc, openshift-install |
| `julia-dist` | `https://julialang-s3.julialang.org/bin` | julia and its `versions.json` |
| `docker-dl` | `https://download.docker.com` | the static docker CLI for Linux, macOS and Windows |
| `android-dl` | `https://dl.google.com/android` | android-sdk's command-line tools, aapt2, android-cli; `dl.google.com/go/` is the [`godist`](/registries/godist) kind |
| `deno-dl`, `zig-dl`, `hex-builds`, `swift-dl`, `swift-api`, `pypy-dl` | — | the `core:` toolchains of [mise](/use/mise#core-toolchains) |
| `helm-bin`, `sonar-binaries` | `https://get.helm.sh`, `https://binaries.sonarsource.com` | helm; sonar-scanner-cli and sonarqube-cli |

A directory listing — a URL ending in `/`, such as the `ocp/` index oc lists versions from, or `ant/binaries/` — is refused with `400` by every `generic` registry, so an install that must list versions first needs `mise.lock`. On a bucket host (`storage.googleapis.com`, `s3.amazonaws.com`) without a preset, the suggested mirror admits the locked file's directory and the version list mise reads from the same bucket.

## Authentication

Add `-H "Authorization: Bearer $BATLEHUB_TOKEN"` when the registry requires auth. For tools driven by env vars, add a `~/.netrc` entry for the proxy host — mise and anything else built on libcurl read it automatically:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Embedding HTTP Basic credentials in the mirror URL works as a fallback, but the token then lives in an environment variable that leaks into shell history, CI logs, process listings, and `mise doctor`-style diagnostics — prefer `~/.netrc`.

## Notes

- A `generic` mirror of an Alpine CDN has the same problem, one tree over: the `.apk` file name carries a real name and version, and a `generic` registry has nowhere to put them. Use [`apk`](/registries/apk) to block, age-gate or count an Alpine package; `generic` stays right for an Alpine tree you only want cached.
- A `generic` mirror of `nodejs.org/dist` caches Node correctly and can enforce nothing on it: a path-addressed registry has one synthetic package and no version to block. For policy on a Node release — a block that reaches `nvm ls-remote`, an age gate on a release published yesterday — use [`nodedist`](/registries/nodedist) instead; `generic` stays the right answer for a tree you want cached without policy.
- A `generic` mirror of a Nix binary cache is the same story a third time, and the worst of the three: every path is one synthetic package, so a CVE in a store path is invisible to the console and `nix copy --to` has nowhere to go. A whole fleet — the compiler included — pulls its closure through that hole. Use [`nix`](/registries/nix) to block a store path by the package and version Nix itself parses out of it; `generic` stays right for `channels.nixos.org`, which is a different host and a different protocol.
- A request for a path outside the registry's `path_allow` allowlist returns `403`, not 404 — that is the allowlist rejecting it locally, before any upstream request is made. Widen the globs if `mise install` reports a 403.
- Mirrored archives are often large; set `max_artifact_size_bytes` on this registry for toolchain tarballs over the instance's 500 MiB default. A cold download streams to the client while it is cached, and a retry joins the running fetch instead of starting another ([Caching](/guide/caching#cold-fill)).
- A tree that is immutable except for a few documents — a `release/stable.txt`, an `index.json` listing the releases — lists those in `revalidate`. They are refetched on every read, and served stale if upstream is down; every other path stays cached forever ([Caching](/guide/caching#revalidated-paths)).
- Path-addressed registries pre-warm specific **paths** via `[registries.cache] warm_paths`, not `warm_packages`.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Helm](/registries/helm) — a chart repository's `index.yaml` mirrored here is cached but unblockable, and its absolute chart URLs bypass the cache; the `helm` kind rewrites and filters it
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
