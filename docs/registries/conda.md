# Conda

Proxy and cache a conda channel, or host private conda packages. BatleHub serves per-platform `repodata.json` plus package downloads, gated by RBAC and the release-age gate. In `hybrid` mode, locally published packages are merged into the upstream `repodata.json`.

## At a glance

| | |
|---|---|
| **Config type** | `conda` |
| **Default upstream** | `conda.anaconda.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `curl -X POST …/{platform}/` |
| **Air gap** | offline, each subdir's `repodata.json` is composed from the held packages, each entry its own `info/index.json` read at import; a subdir with nothing held answers empty, and `micromamba` resolves against it |

## Proxy setup

Point conda at your registry. Replace `<registry>` with your configured registry name:

```yaml
# ~/.condarc  (or .condarc in the project root)
channels:
  - https://batlehub.example.com/proxy/<registry>
  - nodefaults
```

An `environment.yml` uses the same channel:

```yaml
name: myenv
channels:
  - https://batlehub.example.com/proxy/<registry>
  - nodefaults
dependencies:
  - python=3.11
  - numpy
```

## Publishing (local / hybrid)

The registry must be in `local` or `hybrid` mode. Build with `conda build`, then POST the artifact to the target platform directory:

```bash
curl -X POST \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-pkg-1.0.0-py311h0_0.tar.bz2 \
  "https://batlehub.example.com/proxy/<registry>/linux-64/"
```

Both `.tar.bz2` and `.conda` formats are accepted. The name, version, build, and dependencies are read from `info/index.json` inside the archive, and the channel's `repodata.json` is updated immediately.

## Blocked versions

`repodata.json` and `current_repodata.json` drop a blocked package from both
generations — the `.tar.bz2` entry under `packages` and the `.conda` entry under
`packages.conda` — since a channel serves both for the same release and leaving
either would keep the version installable.

::: tip A conda block can take up to 30 seconds
`repodata.json` describes a whole channel and is fetched on every
`conda install`, so its blocked set is read from a snapshot refreshed every
**30 seconds** rather than queried per request. Conda is the only registry with
this delay, and it applies to the *listing* only — the `403` on the download
itself is immediate.
:::

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Conda reads credentials from `~/.netrc` automatically:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

## Notes

- Version listings are synthesised: BatleHub scans `repodata.json` across the standard platforms (`noarch`, `linux-64`, `osx-64`, `osx-arm64`, `win-64`) to assemble the set of available versions.
- The release-age gate keys off the package's `timestamp` field in `repodata.json`. Packages whose upstream metadata omits a timestamp cannot be age-gated, so the gate is skipped for them.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
