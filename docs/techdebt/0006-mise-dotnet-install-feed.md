# TD-0006 — mise's `core:dotnet` downloads the SDK past the mirror

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-11-11, while proving the NuGet read boundary with a real `dotnet restore` (`tests/heavy/authz.sh`, `phase_nuget`) |
| **Area** | Upstream: [jdx/mise](https://github.com/jdx/mise), `src/plugins/core/dotnet.rs` (mise 2026.11.3) |

## What was observed

mise installs a .NET SDK by running Microsoft's `dotnet-install` script as a
subprocess. mise fetches the script itself, so `url_replacements` can route
that request through an instance. The archive is a different matter: the
script downloads it with `curl`/`wget` from feeds it hard-codes
(`builds.dotnet.microsoft.com/dotnet`, `ci.dot.net/public`), and mise passes
it no `--azure-feed`. No mise setting reaches that download.

Every other core toolchain can be pointed at an instance (`node.mirror_url`,
`go.download_mirror`, `url_replacements`). `core:dotnet` cannot. In an
air-gapped network the install fails on a connect timeout. On a connected
network the SDK bypasses the instance, so nothing caches it, audits it or
gates it.

## What holds it today

Nothing in BatleHub. The version list
(`release-metadata/releases-index.json`) does go through mise's HTTP client
and so through `url_replacements`, but the bytes do not.

## What would retire it

1. Get a `dotnet.feed` setting into mise, passed to the script as
   `--azure-feed`. The draft issue is below. It has **not been filed**: that
   was decided on 2026-11-14 to wait.
2. Once mise ships it, give the instance a path that serves the
   `builds.dotnet.microsoft.com/dotnet` layout (a `generic` preset or its own
   kind), document the setting on the registry page, and drive a real
   `mise install dotnet` through it in a heavy phase.

## Draft upstream issue

**Title:** core:dotnet: add a `dotnet.feed` setting passed to dotnet-install as `--azure-feed`

**Body:**

### What happens today

`core:dotnet` installs an SDK or runtime by downloading Microsoft's install
script and running it as a subprocess (mise 2026.11.3):

- the script is fetched from `https://dot.net/v1/dotnet-install.sh`
  (`src/plugins/core/dotnet.rs:380-382`; `.ps1` on Windows, `:385-387`)
  through mise's HTTP client, so `url_replacements` applies to that one
  request;
- the script is then run with `--install-dir`, `--version`, `--no-path` and,
  for a runtime, `--runtime` (`install_cmd`, `src/plugins/core/dotnet.rs:389-405`),
  and nothing else.

The script downloads the archive itself, with `curl`/`wget`, from feeds it
hard-codes: `https://builds.dotnet.microsoft.com/dotnet` and
`https://ci.dot.net/public` (dotnet-install.sh, lines 1343-1345 of the copy
served on 2026-11-11). It overrides them only from its own `--azure-feed` and
`--uncached-feed` arguments (lines 1721-1728). Measured on 2026-11-11:
`https://dot.net/v1/dotnet-install.sh` answers `301` to
`https://builds.dotnet.microsoft.com/dotnet/scripts/v1/dotnet-install.sh`.

Because the archive download happens in a subprocess, mise's
`url_replacements` never sees it, and mise offers no setting that reaches the
script's feed.

### Why a setting is needed

Organisations that route tool downloads through an internal mirror or
caching proxy (air-gapped networks, egress-restricted CI, audit
requirements) can point every other core toolchain at it — through
`url_replacements` or a per-tool setting such as `node.mirror_url` or
`go.download_mirror`. For `core:dotnet` there is no equivalent: the script
is reachable, the bytes it fetches are not. In an air-gapped network the
install fails on a connect timeout; on a connected network the archive
bypasses the mirror.

### Proposal

A `dotnet.feed` setting (string, unset by default). When set, mise appends
`--azure-feed <value>` to the dotnet-install command line (and, if useful,
a matching `dotnet.uncached_feed` → `--uncached-feed`). Unset, the command
line is unchanged, so existing installs behave exactly as today.

On Windows, `dotnet-install.ps1` would need the equivalent parameter (not
checked for this issue).

### Notes

- The version list (`https://builds.dotnet.microsoft.com/dotnet/release-metadata/releases-index.json`,
  `src/plugins/core/dotnet.rs:434`,
  read by mise itself) already goes through mise's HTTP client and
  `url_replacements`; only the archive download needs the new setting.
- Happy to test a build against a mirror that serves the
  `builds.dotnet.microsoft.com/dotnet` layout.
