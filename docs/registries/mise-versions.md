# mise versions host

Proxy mise's **versions host**, `mise-versions.jdx.dev`: the list of versions mise reads for a registry tool *before* any backend's own. Without it, `mise use jq@latest` picks its version from a list this instance never saw, and after every install mise reports the tool, version, OS and architecture to that host. With it, the list goes through the block list, the report stays on site, and an air-gapped host gets an answer instead of a timeout ([RFC 0043](/rfc/0043-mise-versions-host)).

There is nothing to install from this kind. It serves version **lists**; the bytes still come through the registry that serves the tool's backend — [GitHub](/registries/github) for `aqua:` and `github:` tools.

## At a glance

| Property | Value |
|---|---|
| **Config type** | `mise-versions` |
| **Default upstream** | `mise-versions.jdx.dev` |
| **Modes** | proxy-only |
| **Addressing** | one package per mise tool (`jq`), versions as mise spells them (`1.8.2`, not the forge tag `jq-1.8.2`) |
| **Private publish** | proxy-only |
| **Air gap** | a list is a `404` at once — mise's cue to list from the backend, which the air gap composes from what it holds |
| **Clients** | mise 2026.1.0 and later |

## Proxy setup

mise reaches this host through the same `[settings.url_replacements]` it uses for every download. One rule, placed **before** any rule that could match the same host, because mise applies the first match only:

```toml
[settings.url_replacements]
"regex:^https://mise-versions\\.jdx\\.dev/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-versions/$1"
```

`batlehub-cli registry suggest --mise` and `batlehub-cli setup apply --only mise` write it, first, as soon as the instance has a `mise-versions` registry.

mise asks this host only for a tool whose backend is the **registry default** — `jq`, `node`, `terraform` — never for an explicit backend such as `github:cli/cli`. Those are listed by their backend directly.

### Auto-wire: one rule for every host

With `[registries.auto_wire] enabled = true` on this registry
([RFC 0045](/rfc/0045-mise-auto-wire)), mise carries one rule for every host
the instance mirrors instead of one per host. The server prints it for your
account:

```bash
batlehub-cli registry suggest --mise --auto-wire
batlehub-cli registry suggest --mise --auto-wire --mise-catch-all
```

The second form is the air-gap variant. [Pointing mise at BatleHub](/use/mise#auto-wire)
covers the overlay, troubleshooting and rollback.

## Authentication

mise sends a `~/.netrc` entry for the host a rule rewrote to, so an authenticated instance needs one stanza for the proxy host — the same one the forge registries use:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

mise refuses to send credentials over an HTTPS-to-HTTP rewrite.

Your administrator's registry block:

```toml
[[registries]]
name = "mise-versions"
type = "mise-versions"
mode = "proxy"                                  # the only mode: nobody publishes here
upstreams = ["https://mise-versions.jdx.dev"]   # the default

[registries.rbac]
# Every route of this kind is a listing (`releases:list`); the python index is
# read as bytes (`releases:read`).
anonymous = ["releases:read", "releases:list"]

  # Optional. Hides versions published less than a week ago from the list, so
  # `latest` lands on one the backend registry's own gate lets through.
  [[registries.rules]]
  kind = "release_age_gate"
  min_age_secs = 604800
```

## Blocked versions

A blocked version is one line less in `data/<tool>.toml`: every other line — order, `release_url`, `rolling`, `prerelease`, keys this instance does not know — is served as upstream wrote it. `mise ls-remote jq` no longer prints it and `mise use jq@latest` picks the newest version left.

**A block here is a statement about the list, not about the bytes.** `jq` / `1.8.2` on this kind and `jqlang/jq` / `jq-1.8.2` on the GitHub registry are two coordinates, and the second is the one that refuses the download. Block both — the tool name is mise's (`mise registry jq` prints its backend) and the version is the tag without its prefix:

```bash
batlehub-cli package block github/jqlang/jq@jq-1.8.2 --reason "CVE-2026-0000"
batlehub-cli package block mise-versions/jq@1.8.2 --reason "CVE-2026-0000"
```

Blocking every version still serves the document, with an empty table; mise then lists from the backend, which refuses the same versions.

See [blocking a package version](/guide/admin-policies#block-a-package-version) and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered).

## Notes

- **The age gate filters the list.** Every entry carries `created_at`, and this kind has no artifact for a gate to refuse, so a `release_age_gate` on the registry hides the versions younger than it — the one listing in BatleHub an age gate filters. It is re-evaluated on every request against the cached list, so a version appears on the day it comes of age.
- **The install beacon never leaves.** After an install mise reports the tool, version, OS, architecture and backend URL — `GET /t?…` from 2026.11, `POST /api/tools/{tool}` from 2026.5, `POST /api/track` before that. This kind answers `204` and forwards nothing; nothing from it is logged above `trace`. mise sends it fire-and-forget and often exits first, so the request may not appear at all.
- **The GitHub mirror is refused with `503`.** mise also asks this host for GitHub release metadata (`/api/github/…`) unless `api.github.com` is rewritten. A `503` is the answer mise retries on `api.github.com`; a `404` would read as "no such release" and fail the install. Keep the GitHub registry's rules in the same block, and mise skips the mirror altogether.
- **The python-build-standalone index** (`python-precompiled-<triple>.gz`, which mise 2026.9.5 asks for under `tools/` and later releases under `data/`) is cached as upstream sent it and filtered on the way out: every build of a version blocked as `python` on this registry is removed, and under a `release_age_gate` so is every build whose release date (the `+YYYYMMDD` in its file name) is younger than the gate. An older build of the same version stays. With nothing to remove, the bytes are upstream's.
- **Older mise.** mise 2026.1.0–2026.6.5 asks for `tools/<tool>.toml`, which is served from the same cache entry. mise before 2026.1 asked for `/<tool>` as plain text; that path is a `404` here, and that mise lists from its backend.
- **Air gap.** No bundle carries this list — `mise.lock` names assets, not listings — so an air-gapped instance answers `404` without recording a miss. mise then lists from the backend, and RFC 0008-bis composes that list from the assets the bundle carried.

## Endpoints

::: details Show every endpoint

<!-- BEGIN endpoints: proxy/mise-versions -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/mise-versions/_registry/latest.tar.zst` | The registry archive mise floats, with this instance's overlay tools |
| `GET` | `/proxy/{registry}/mise-versions/_wire-answer/{outcome}` | The wire's own answer: a bounce, `not_wired`, a malformed path, or the |
| `GET` | `/proxy/{registry}/mise-versions/api/github/{tail}` | The GitHub mirror's API family — refused with the `503` that makes mise ask |
| `POST` | `/proxy/{registry}/mise-versions/api/tools/{tool}` | The install beacon of mise 2026.5–2026.10 — answered here, sent nowhere. |
| `POST` | `/proxy/{registry}/mise-versions/api/track` | The install beacon of mise 2026.1–2026.4 — answered here, sent nowhere. |
| `GET` | `/proxy/{registry}/mise-versions/data/{file}` | A tool's version list, blocked and too-young versions removed — or the |
| `GET` | `/proxy/{registry}/mise-versions/data/github/{tail}` | The GitHub mirror's whole-list family — refused like the API family. |
| `GET` | `/proxy/{registry}/mise-versions/t` | The install beacon of mise 2026.11 and later — answered here, sent nowhere. |
| `GET` | `/proxy/{registry}/mise-versions/tools/{file}` | The same documents under the path mise 2026.1.0–2026.6.5 asks for its |
<!-- END endpoints -->

:::

## See also

- [mise](/use/mise) — pointing mise at BatleHub, connected and air-gapped
- [GitHub](/registries/github) — where the bytes of an `aqua:` or `github:` tool come from
- [Node distributions](/registries/nodedist) — the other toolchain list mise reads
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
