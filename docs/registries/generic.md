# Generic mirror

A proxy-only, path-addressed mirror of any plain HTTP file tree — for upstreams that have no package protocol at all: toolchain tarballs (`nodejs.org/dist`, `static.rust-lang.org`, `dl.google.com/go`) and single-binary vendor CDNs (`get.helm.sh`, `dl.min.io`). Every request streams `{upstream}/{path}` and caches it on the first miss. There is no publish, index, or signing model.

## At a glance

| | |
|---|---|
| **Config type** | `generic` |
| **Default upstream** | none — set `upstreams` explicitly (mandatory) |
| **Modes** | proxy-only |
| **Addressing** | path-addressed |
| **Private publish** | ❌ proxy-only |
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

Tools like `mise` read these automatically, and can also route direct downloads through a `[settings.url_replacements]` block (the `mise` `[settings.url_replacements]` use case). `batlehub registry suggest` scans a project (including `mise.toml` / `mise.lock`) and prints both the registry config blocks and the matching client environment variables.

## Authentication

Add `-H "Authorization: Bearer $BATLEHUB_TOKEN"` when the registry requires auth. For tools driven by env vars, add a `~/.netrc` entry for the proxy host — mise and anything else built on libcurl read it automatically:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Embedding HTTP Basic credentials in the mirror URL works as a fallback, but the token then lives in an environment variable that leaks into shell history, CI logs, process listings, and `mise doctor`-style diagnostics — prefer `~/.netrc`.

## Notes

- A `generic` mirror of `nodejs.org/dist` caches Node correctly and can enforce nothing on it: a path-addressed registry has one synthetic package and no version to block. For policy on a Node release — a block that reaches `nvm ls-remote`, an age gate on a release published yesterday — use [`nodedist`](/registries/nodedist) instead; `generic` stays the right answer for a tree you want cached without policy.
- A request for a path outside the registry's `path_allow` allowlist returns `403`, not 404 — that is the allowlist rejecting it locally, before any upstream request is made. Widen the globs if `mise install` reports a 403.
- Mirrored archives are often large; the proxy buffers the whole artifact before caching, so raise `limits.max_artifact_size_bytes` (default 500 MiB) for toolchain tarballs.
- Path-addressed registries pre-warm specific **paths** via `[registries.cache] warm_paths`, not `warm_packages`.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
