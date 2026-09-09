# JetBrains IDEs

Cache JetBrains IDE installer archives. The first download is streamed from `download.jetbrains.com` and cached; later downloads of the same file are served locally — ideal for CI/Docker builds and offline networks. Proxy-only — there is no private publish model for IDE archives.

## At a glance

| | |
|---|---|
| **Config type** | `jetbrains` |
| **Default upstream** | `download.jetbrains.com` |
| **Modes** | proxy-only |
| **Addressing** | path-addressed |
| **Private publish** | ❌ proxy-only |
| **Air gap** | no composed index offline; a held file is served by path |

## Proxy setup

The path after `/jetbrains/` maps 1:1 to `download.jetbrains.com/<path>`. Replace `<registry>` with your configured registry name:

```bash
REG="https://batlehub.example.com/proxy/<registry>/jetbrains"

# download.jetbrains.com/idea/idea-2026.1.3.tar.gz
#   → $REG/idea/idea-2026.1.3.tar.gz
curl -fL -o idea.tar.gz $REG/idea/idea-2026.1.3.tar.gz
```

Use the **canonical** `download.jetbrains.com` path. That host 302-redirects to a CDN (`download-cdn.jetbrains.com`); BatleHub follows the redirect automatically and caches the final bytes under the path you requested — you never put the CDN host in the URL. To proxy the CDN host directly instead, set `upstreams = ["https://download-cdn.jetbrains.com"]`.

Use the **real** archive names: `idea-<ver>` for the unified installer (2025.3+); the legacy `ideaIU-<ver>` (Ultimate) / `ideaIC-<ver>` (Community) names only exist for releases ≤ 2025.2. A wrong name returns the upstream's 404.

The `batlehub download` CLI fetches a file through the proxy (and caches it as a side effect):

```bash
# registry-relative (needs -r); writes ./idea-2026.1.3.tar.gz
batlehub -r <registry> download jetbrains/idea/idea-2026.1.3.tar.gz
```

## Authentication

Add `-H "Authorization: Bearer $BATLEHUB_TOKEN"` when the registry requires auth.

## Notes

- IDE archives are large (~1–1.7 GB). The proxy buffers the whole artifact in memory before caching and rejects anything over `limits.max_artifact_size_bytes` (default 500 MiB), so raise that limit — e.g. `2147483648` for 2 GiB — or downloads will fail.
- Path-addressed registries pre-warm specific **paths** (there is no version model). List them under `[registries.cache] warm_paths` to fetch on startup, or warm on demand with `batlehub admin cache warm <registry> --paths "idea/idea-2026.1.3.tar.gz"`.
- For the JetBrains **plugin** ecosystem (plugins.jetbrains.com), use the dedicated `jetbrains-marketplace` type instead — don't point this kind at that host.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
