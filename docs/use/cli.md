---
reference: true
---

# batlehub-cli

`batlehub-cli` is the official command-line client for BatleHub. It provides both a traditional CLI for scripting and CI pipelines, and an interactive TUI for everyday browsing and management.

## 1. Installation

**via mise** (recommended — manages version automatically):

```bash
mise use "github:batleforc/batlehub[asset_pattern=batlehub-cli-*]"
```

**via cargo** (builds from source — requires Rust toolchain):

```bash
cargo install --git https://github.com/batlehub/batlehub batlehub-cli
```

**Pre-built binaries** — download from [GitHub Releases](https://github.com/batlehub/batlehub/releases/latest):

```bash
# Linux x86_64
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-linux-amd64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# Linux aarch64
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-linux-arm64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# macOS Apple Silicon (M1/M2/M3)
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-darwin-arm64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# macOS Intel
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-darwin-amd64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# Windows (PowerShell)
Invoke-WebRequest https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-windows-amd64.zip -OutFile batlehub-cli.zip
Expand-Archive batlehub-cli.zip -DestinationPath .
Move-Item batlehub-cli.exe "$env:LOCALAPPDATA\Microsoft\WindowsApps\batlehub-cli.exe"
```

Or run directly without installing (inside the repository):

```bash
task cli -- registry list
task cli:tui
task cli:help
```

---

## 2. Configuration

`batlehub-cli` reads `~/.config/batlehub/config.toml`. Run the setup wizard to create it:

```bash
batlehub-cli profile init
```

The file uses TOML and supports named profiles:

```toml
[default]
server_url = "http://localhost:8080"
token      = "my-secret-token"
registry   = "my-registry"        # optional default registry

[profiles.prod]
server_url = "https://batlehub.example.com"
token      = "prod-secret-token"
```

### Environment variable overrides

Every connection setting can be overridden by environment variables — useful in CI without touching the config file:

| Variable            | Equivalent flag  |
|---------------------|------------------|
| `BATLEHUB_SERVER`   | `--server`       |
| `BATLEHUB_TOKEN`    | `--token`        |
| `BATLEHUB_REGISTRY` | `--registry`     |
| `BATLEHUB_PROFILE`  | `--profile`      |
| `BATLEHUB_FORMAT`   | `--format`       |

---

## 3. Global flags

These flags are available on every command:

| Flag | Short | Description |
|------|-------|-------------|
| `--profile <name>` | `-P` | Use a named config profile |
| `--server <url>` | | Override the server URL |
| `--token <tok>` | | Override the auth token |
| `--registry <name>` | `-r` | The default registry: for a bare package name, and for every command's registry filter. Falls back to `BATLEHUB_REGISTRY`, then the profile's `registry` |
| `--format <table\|json\|csv\|raw>` | | Output shape (below); `BATLEHUB_FORMAT` sets it once for a CI job |
| `--json` | | Same as `--format json` |
| `--yes` | `-y` | Answer yes to the confirmation a destructive command asks for |

### Naming a package {#coordinate}

A command that takes a package takes one coordinate, `registry/name[@version]`:
the registry is everything before the first `/`, the version everything after
the last `@` that is not the name's first character.

```text
npm-local/lodash@4.17.21
npm-local/@scope/pkg@1.2.3        # name @scope/pkg
go/github.com/x/y@v1.2.0          # the module path stays whole
maven/org.slf4j:slf4j-api@2.0.9   # Maven's group:artifact stays whole
```

A coordinate with no `/` is a bare name, put in the default registry
(`-r`, `BATLEHUB_REGISTRY`, the profile's `registry`); with none of them set the
command fails with exit 2. A first segment that is not a registry you can see is
read as part of the name when a default registry is set
(`-r go github.com/x/y`), and fails with exit 5 otherwise, listing the registries
you can see. The older `registry:name@version` spelling is still read, with a
`warning:`.

Every command that acts on one package takes a coordinate: `package`, `owner`,
`visibility`, `grant`, `sbom show`, `authz check`, `why`, `wait` and
`verdict pullers`. The lifecycle verbs take several at once.

### Output and errors {#output}

- **`table`** is the default, on a terminal and in a pipe alike.
- **`json`** prints what the API sent, field names unchanged: a listing as an
  array (or the API's page object), an action as its response, or `{"ok": true}`
  when there is none.
- **`csv`** works on every listing, with the JSON field names as headers. It is a
  usage error (exit 2) on a command that prints one object.
- **`raw`** is accepted only by `auth token`, which prints one value.

Data goes to stdout. Progress, warnings, hints and confirmations go to stderr, so
`batlehub-cli … --format json | jq` never reads a prompt.

An error is one block on stderr:

```text
error: version 4.17.21 of npm-local/lodash is held until 2026-11-04 14:00 UTC
  code: MIN_AGE_NOT_MET — https://batlehub.dev/use/refused-downloads#min-age-not-met
hint: `batlehub-cli wait npm-local/lodash@4.17.21` returns when it becomes pullable
```

`error:` is the server's own message, `code:` the reason code when the server
gave one, linked to [Why a download was refused](/use/refused-downloads). With
`--format json` the same error is also written to stdout as
`{"error": {"message", "code", "docs", "hint", "exit"}}`.

### Exit codes {#exit-codes}

| Code | Meaning | Typical source |
| --- | --- | --- |
| 0 | success | — |
| 1 | generic failure | network error, unexpected 5xx, I/O |
| 2 | usage | a flag clap refuses, a coordinate with no registry, an artifact written to a terminal, `--format csv` on one object, a lockfile over 16 MiB |
| 3 | not authenticated | HTTP 401 |
| 4 | refused | HTTP 403; `wait` when waiting cannot help; `check` on a ⛔ (or a ⏳ with `--strict`) |
| 5 | not found | HTTP 404, an unknown registry |
| 6 | conflict | HTTP 409; `setup` `conflict`/`ambiguous`/`orphaned` |
| 7 | partial failure | a bulk operation with failed items, `mise seed`/`import` with failures |
| 8 | timed out | `wait --timeout` |
| 9 | confirmation required | a destructive command without a terminal and without `--yes`, a "no" at the prompt |
| *n* | child's code | `java` passes its child's exit code through |

### Destructive commands {#confirmation}

A command that deletes, purges, erases, yanks, unlists, blocks, revokes or
reclaims prints what it will do on stderr and asks `Proceed? [y/N]`. Anything
other than `y`/`yes` exits 9. **Without a terminal** it asks nothing, does
nothing and exits 9, so a script that forgets `--yes` fails loudly instead of
hanging on a prompt or destroying silently:

```text
$ batlehub-cli package delete internal/serilog@2.0.0 </dev/null
this will delete:
  internal/serilog@2.0.0
the artifact is dropped and the version number is spent permanently — 2.0.0 can never be published again
error: refusing to delete without a terminal; pass --yes
$ echo $?
9
```

The commands that confirm: `package yank`, `delete` and `unlist`,
`cache delete`, `cache evict` and `coherence` (not with `--dry-run`),
`audit purge`, `user block`, `gdpr erase`, `retention apply`,
`notification delete`, `auth token delete`, and `download` over an existing
file. `setup apply` and
`setup remove` keep their own contract: a dry run by default, `--yes` writes.

### What each registry kind covers {#coverage}

For every kind, what the CLI does for six capabilities: **configure** the client
(`setup`), **publish**, **download** by coordinate, **consult** (`package
versions`/`readme`), and the version **lifecycle** (`package yank`, …). A cell is
✅ (implemented), **print** (the CLI prints what to run, because running it needs
root or belongs to the native client) or **n.a.** (with the reason). The table is
generated from `cli/src/coverage.rs`, and the build fails when a claimed cell has
no test.

<!-- cli-matrix:start (generated by `task docs:cli-matrix`) -->
| Kind | Configure | Publish | Download | Consult | Lifecycle | Check |
| --- | --- | --- | --- | --- | --- | --- |
| github | ✅ mise | n.a. — proxy only | n.a. — a release asset is addressed by its filename, which the page does not know | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank | ✅ `mise.lock` |
| forgejo | ✅ mise | n.a. — proxy only | n.a. — a release asset is addressed by its filename, which the page does not know | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank | ✅ `mise.lock` |
| gitlab | ✅ mise | n.a. — proxy only | n.a. — a release asset is addressed by its filename, which the page does not know | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank | n.a. — `mise plan` maps no GitLab download to a registry yet |
| cargo | ✅ `~/.cargo/config.toml` | ✅ `.crate` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `Cargo.lock` |
| npm | ✅ `~/.npmrc`, `~/.yarnrc.yml` | ✅ `.tgz` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` |
| openvsx | ✅ `setup ide` | ✅ `.vsix` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — editors keep no lockfile |
| goproxy | ✅ `go env` file | ✅ module directory → one `{version}.zip` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `go.sum` |
| pypi | ✅ `pip.conf`, `uv.toml` | ✅ wheel, sdist | n.a. — a PyPI version is a set of distribution files — an sdist and one wheel per interpreter and platform — each addressed by its own filename, so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `uv.lock`, `poetry.lock`, pinned `requirements*.txt` |
| conda | ✅ `~/.condarc` | ✅ `.conda`, `.tar.bz2` | n.a. — a conda artifact is addressed by channel platform and build string as well as version, so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `conda-lock.yml` |
| composer | ✅ composer `config.json` | ✅ `.zip` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `composer.lock` |
| vscode-marketplace | ✅ `setup ide` | ✅ `.vsix` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — editors keep no lockfile |
| maven | ✅ `~/.m2/settings.xml` | ✅ `.jar` + `--pom`, or `target/` | n.a. — a Maven version is a set of files — a jar, a pom, sources, javadoc — so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — Maven has no lockfile |
| terraform | ✅ `~/.terraformrc` | ✅ module `.tar.gz`; provider directory of `_<os>_<arch>.zip` | n.a. — a Terraform provider binary is addressed by OS and architecture as well as version, and a module is fetched by the client from a URL this instance rewrites rather than as one artifact | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `.terraform.lock.hcl` |
| rubygems | ✅ `~/.gemrc` | ✅ `.gem` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `Gemfile.lock` |
| nuget | ✅ `NuGet.Config` | ✅ `.nupkg` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `packages.lock.json` |
| deb | print — the sources file is root's | ✅ `.deb` + `--distribution`/`--component` | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — the system's package manager keeps no lockfile |
| rpm | print — the sources file is root's | ✅ `.rpm` | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — the system's package manager keeps no lockfile |
| pacman | print — the sources file is root's | ✅ `.pkg.tar.zst` | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — the system's package manager keeps no lockfile |
| apk | print — the sources file is root's | ✅ `.apk` | n.a. — a version needs a branch, a repo and an architecture as well, so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — the system's package manager keeps no lockfile |
| jetbrains | n.a. — serves IDE downloads by path; `setup ide` configures the plugin marketplace | n.a. — proxy only | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank | n.a. — editors keep no lockfile |
| jetbrains-marketplace | ✅ `setup ide` | ✅ plugin `.zip`/`.jar` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — editors keep no lockfile |
| generic | ✅ mise | n.a. — proxy only | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank | n.a. — a mirrored file has no version to ask about |
| nodedist | ✅ env + mise | n.a. — a vendor's releases, mirrored | n.a. — a Node release is a set of files — one per platform, plus headers, source and checksums — so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | n.a. — a vendor's releases: nothing published here to yank | ✅ `.nvmrc` |
| sdkman | ✅ env | n.a. — a vendor's releases, mirrored | n.a. — an SDKMAN artifact is addressed by platform as well as version — one archive per platform — so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | n.a. — a vendor's releases: nothing published here to yank | ✅ `.sdkmanrc` |
| rustup | ✅ env | n.a. — a vendor's releases, mirrored | n.a. — a Rust release is a manifest plus one tarball per component per target, so "fetch this version" needs a target and a profile | ✅ `package versions`, `package readme` | n.a. — a vendor's releases: nothing published here to yank | ✅ `rust-toolchain.toml` |
| galaxy | ✅ `~/.ansible.cfg` | ✅ collection `.tar.gz` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — `requirements.yml` pins no exact version |
| nix | ✅ user `nix.conf` | print — `nix copy --to` signs and uploads the closure | n.a. — a version is one or more store paths, each a separate build with its own 32-character hash, and the hash is not derivable from the version | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — `flake.lock` pins sources; the cache serves store paths by hash |
| devfile | print — Che's `CheCluster` is cluster configuration | ✅ `devfile.yaml` or its directory | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | n.a. — editors keep no lockfile |
| helm | print — `helm repo add`: helm rewrites its own repository file | ✅ chart `.tgz` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint | ✅ `Chart.lock` |
<!-- cli-matrix:end -->

---

## 4. Commands — registry

```
batlehub-cli registry list
batlehub-cli registry show <name>
batlehub-cli registry suggest [--dir <path>] [--depth N] [--client-env] [--mise [--mise-commented]] [--include-existing]
```

### `registry list`

List all registries visible to the current identity.

```
$ batlehub-cli registry list
+----------+---------+--------+
| Name     | Type    | Mode   |
+----------+---------+--------+
| cargo    | cargo   | proxy  |
| internal | nuget   | hybrid |
| pypi     | pypi    | local  |
+----------+---------+--------+
3 registry/registries
```

### `registry show <name>`

Show type and mode for a single registry.

### `registry suggest`

Work out which registries a project actually needs, and print the
`[[registries]]` blocks to paste into `config.toml`.

Two inputs, in decreasing order of precision:

- **`mise.lock`** — the best source available: it records the exact download URL
  of every tool, per platform. Each URL maps either onto a typed registry (a
  `github.com` release asset → `type = "github"`) or, for hosts that speak no
  package protocol at all, onto a `generic` mirror of that host.
- **`mise.toml`** and the usual project manifests (`Cargo.toml`, `go.mod`,
  `package.json`, `pyproject.toml`, `pom.xml`, `composer.json`, `*.gemspec`,
  `*.nuspec`, `*.csproj`, `*.tf`, `environment.yml`) — no URLs, so the mapping
  goes by backend prefix / tool name and is best-effort. When a lock file is
  present it takes precedence, since it names the same tools more precisely.

```console
$ batlehub-cli registry suggest --client-env
+------------------+---------+--------------------------------------+----------------------------+
| Name             | Type    | Upstream                             | Detected from              |
+------------------+---------+--------------------------------------+----------------------------+
| cargo            | cargo   | (adapter default)                    | manifest, mise.lock: …     |
| github           | github  | (adapter default)                    | mise.lock: gitleaks, …     |
| node-dist        | generic | https://nodejs.org/dist              | mise.lock: node            |
| rust-dist        | generic | https://static.rust-lang.org         | mise.lock: rust            |
| helm-bin         | generic | https://get.helm.sh                  | mise.lock: helm            |
+------------------+---------+--------------------------------------+----------------------------+

Add to config.toml:
…
Point clients at the proxy:

# node-dist (generic)
export NODEJS_ORG_MIRROR="https://batlehub.example.com/proxy/node-dist/generic"
```

| Flag | Description |
|------|-------------|
| `--dir <path>`, `-d` | Directory to scan (default: current working directory) |
| `--depth N` | Subdirectory levels to scan for manifests (default 0 = root only). Does not affect `mise.lock`, which is only read from the root. |
| `--client-env` | Also print the environment variables that point each toolchain at the proxy |
| `--mise` | Also print a mise `[settings.url_replacements]` block routing mise itself through the proxy |
| `--mise-commented` | Comment out every line of the `--mise` block, for committing into a shared `mise.toml` |
| `--include-existing` | Emit suggestions even when the server already has a registry of that type |

#### Routing mise itself (`--mise`)

`[settings.url_replacements]` rewrites the URLs **mise's own HTTP layer**
fetches, which covers the aqua/ubi/GitHub-release backends and every `generic`
mirror. Verified against `mise install`:

- aqua resolves *and downloads* assets through `api.github.com/repos/…/releases/assets/{id}`,
  so the `api.github.com` rule is the load-bearing one — not the
  `github.com/…/releases/download/…` rule.
- `core:node` fetches **both** the platform tarball and the `node-v<ver>.tar.gz`
  source tarball, which is why the suggested `path_allow` is `v*/**` rather than
  a platform-only glob.

Backends that shell out to another tool (`cargo:`, `pipx:`, `npm:`, `go:`) are
**not** covered — those processes read their own config, not mise's. The
generated block names them explicitly rather than leaving them silently absent;
use `--client-env` and the per-ecosystem config for those.

> The regex keys must reach the file with **doubled** backslashes
> (`\\.`). TOML treats a lone `\` as an invalid escape, and mise responds by
> logging one line and running on with the entire settings block dropped — a
> silent no-op. The generator handles this; hand-edits should keep it in mind.

`--mise-commented` prefixes every line, including the generator's own header
comments, so that stripping exactly one `#` (and its trailing space) per line
yields a valid file. This
repo's own `mise.toml` carries such a block as a worked example.

Every generated `generic` block carries the `upstreams` and `path_allow` fields
the server requires for that type, so the output is directly usable. Note the
difference in allowlist precision:

- For hosts with a **curated preset** (`nodejs.org`, `static.rust-lang.org`,
  `dl.google.com`, `get.helm.sh`, `binaries.sonarsource.com`), the
  allowlist is a version-agnostic glob and keeps working across version bumps.
- For any **other host**, the allowlist is the set of exact paths found in the
  lock — narrow and provably sufficient for the pinned versions, but needing a
  re-run (or a manual widening) when those versions change. The generated TOML
  says so in a comment above the block.

On object-storage hosts (`storage.googleapis.com`, `s3.amazonaws.com`) the
bucket segment is folded into `upstreams`, not left in the path — otherwise the
mirror would relay every other public bucket on the same host.

Scanning is entirely local: the server is contacted only to annotate which types
are already configured, and an unreachable server degrades that annotation
rather than failing the command. `--json` emits the structured suggestions plus
the rendered TOML under a `toml` key.

---

## 5. Commands — package

```
batlehub-cli package list   [-r <r>] [--search <q>] [--blocked] [--page N] [--per-page N]
batlehub-cli package versions <registry>/<name>
batlehub-cli package show     <registry>/<name>[@<version>]
batlehub-cli package readme   <registry>/<name>[@<version>] [--no-upstream]
```

### `package list`

List packages across all accessible registries (or just one with `--registry`).

```
$ batlehub-cli package list --registry internal --search serilog
+----------+----------+-------------------+-----------+---------+
| Registry | Name     | Version           | Status    | Accesses|
+----------+----------+-------------------+-----------+---------+
| internal | Serilog  | 3.1.1             | available | 1234    |
| internal | Serilog  | 3.0.0             | blocked:… | 89      |
+----------+----------+-------------------+-----------+---------+
```

Use `--json` to get the raw JSON array — useful in scripts:

```bash
batlehub-cli --json package list --registry internal | jq '.[].name' | sort -u
```

The JSON items use an internally-tagged `status` field:

```json
[
  { "registry": "internal", "name": "serilog", "version": "3.1.1",
    "status": {"status": "available"}, "access_count": 1234 },
  { "registry": "internal", "name": "serilog", "version": "3.0.0",
    "status": {"status": "blocked", "reason": "yanked"}, "access_count": 89 }
]
```

`--blocked` keeps only blocked versions. The server filters before it cuts the
page, so `--per-page` and the total count only include blocked rows:

```bash
batlehub-cli package list --blocked -r internal
```

### `package versions <registry>/<name>`

Every version of a package, including those only the upstream has, one page at a
time, with the server's own state for each (held, upstream-only, yanked,
blocked, …) and where it came from (`local`, `proxied`, `upstream`).

```
batlehub-cli package versions <registry>/<name> [--limit N] [--page N] [--all] [--no-upstream]
```

`--page` is 0-based. When more pages exist, `next: --page N` goes to stderr, so
`--format json | jq '.[].version'` keeps working. `--all` follows the pages, up to
10 000 rows. `--no-upstream` answers from what this instance holds only.

### `package show <registry>/<name>[@<version>]`

One version: its state (the same column as `package versions`), where it comes
from, its published date, licence and links, then the answer `why` gives for it.
Without a version, the one the console opens on: the newest stable version this
instance holds. An unknown version exits 5. `--format json` returns
`{registry, name, version, links, pullability}`, `version` being the
`package versions` row.

```
$ batlehub-cli package show npm-local/left-pad@1.3.9
version     1.3.9
state       cached
source      local
published   2026-10-02T09:14:00Z
license     MIT
repository  https://github.com/left-pad/left-pad
homepage    —

✅ npm-local/left-pad@1.3.9 — pullable now
  ✅ pass  access      you may read npm-local
```

### `package readme <registry>/<name>[@<version>]`

Print a version's README — the **source**, not a rendering. Markdown in a
terminal is readable, and turning it into ANSI is a separate concern.

```
$ batlehub-cli package readme internal/mylib@1.4.2
# mylib

Does a thing.
```

Without a version, the newest one that has a README answers. When the version
you asked for ships none, the newest that does answers instead — and says so:

```
$ batlehub-cli package readme internal/mylib@2.0.0-rc1 > README.md
note: showing 1.4.2's README; version 2.0.0-rc1 ships none
```

**Every qualification goes to stderr**, so redirecting stdout writes the document
and nothing else. The notes you may see: a fallback from another version, a
README that is the *package's* rather than this version's, one read from the
upstream's own answer because nothing of this version is held here, one
truncated at the registry's `max_bytes`, and one that is not markdown.

`--no-upstream` answers from what this instance holds, without asking the
registry's upstream about a version it holds nothing of — for a script, or for a
host with no route off site. See
[what leaves this instance](/operations/egress#the-console-s-discovery-read).

`--json` prints the whole response, so a script can read `is_fallback`, `stored`
and `truncated` rather than parsing the notes:

```bash
batlehub-cli --json package readme internal/mylib@1.4.2 | jq -r .source_text
```

Which registries carry a README at all, and where each one's comes from, is in
the [README support table](/registries/#readmes).

---

## 6. Commands — package lifecycle {#commands-version}

```
batlehub-cli package yank        <registry>/<name>@<version>...
batlehub-cli package unyank      <registry>/<name>@<version>...
batlehub-cli package delete      <registry>/<name>@<version>...
batlehub-cli package pin         <registry>/<name>@<version>...
batlehub-cli package unpin       <registry>/<name>@<version>...
batlehub-cli package deprecate   <registry>/<name>@<version>... [--message <text>]
batlehub-cli package undeprecate <registry>/<name>@<version>...
batlehub-cli package unlist      <registry>/<name>@<version>...
batlehub-cli package relist      <registry>/<name>@<version>...
batlehub-cli package block       <registry>/<name>@<version>... --reason <text>
batlehub-cli package unblock     <registry>/<name>@<version>...
```

Each verb takes one coordinate or several, across registries; with `-r`, a bare
`name@version` names a version of the default registry. These commands need the
instance-tier `releases:yank` (`releases:delete` for `delete`), with one
exception: on cargo, composer, rubygems, terraform and nuget registries, whose
own clients let an owner yank, an **explicit owner of every package named** may
`yank` and `unyank` them too. A request that names a package the caller does not
own is refused whole, and a package with no owner is nobody's. `block` and `unblock` are moderation and need `packages:block`; they send one
request for every registry named. `yank`, `delete`, `unlist` and `block` are
[destructive](#confirmation): they ask on a terminal and need `--yes` in a
script.

| Command | Effect |
|---------|--------|
| `yank` | Marks a version unavailable (kept in storage, download blocked) |
| `unyank` | Reverses a yank |
| `delete` | Drops the artifact **and spends the version number permanently** |
| `pin` | Exempts a version from retention — it is never reclaimed automatically |
| `unpin` | Releases the pin, so the registry's retention policy applies again |
| `deprecate` | Warns a consumer and keeps serving |
| `undeprecate` | Removes the deprecation |
| `unlist` | Hides the version from search and listings, and keeps serving it to anyone who asks by exact version |
| `relist` | Reverses an unlist |
| `block` | Refuses the version to every client, with the code [`BLOCK_LIST`](/use/refused-downloads#block-list); `why` shows the reason to whoever can see findings |
| `unblock` | Lifts a block |

> **Package name casing**: package names are normalized to lowercase when published (NuGet lowercases the package ID, cargo and npm use lowercase by convention). Use the lowercase form to match the stored name — e.g. `serilog`, not `Serilog`.

With several coordinates, every one is attempted: the output names each that
succeeded and each that failed (`FAILED <coordinate>: <error>`), and the command
exits 7 when some failed. `--format json` prints
`{"processed", "succeeded": [...], "failed": [{"coordinate", "error"}]}`.

`delete` prompts for confirmation unless `--yes` is passed:

```
$ batlehub-cli package delete internal/serilog@2.0.0
this will delete:
  internal/serilog@2.0.0
the artifact is dropped and the version number is spent permanently — it can never be published again
Proceed? [y/N] y
Deleted internal/serilog@2.0.0
```

A deleted version number is never reused. Publishing `2.0.0` again is refused
with `409`, whoever asks and however long afterwards, so "delete and re-upload to
fix it" is not a plan — publish `2.0.1`, or `yank` instead if you only need the
version to stop being installed. The reasoning, and what the deletion leaves
behind for an auditor, are in
[Deleting a published version](/guide/admin-policies#deleting-versions).

---

## 7. Commands — owner {#commands-owner}

```
batlehub-cli owner list   <registry>/<name>
batlehub-cli owner add    <registry>/<name> <principal> [--type user|group] [--role admin|maintainer]
batlehub-cli owner remove <registry>/<name> <principal> [--type user|group]
```

Ownership controls who can publish new versions to a local/hybrid registry. It
is per package, so a coordinate with a version is refused. Requires an admin
token.

```
$ batlehub-cli owner list internal/Serilog
+------+------------------+------------+------------+
| Type | Principal        | Role       | Granted By |
+------+------------------+------------+------------+
| user | alice@example.com| admin      | -          |
| group| nuget-maintainers| maintainer | alice      |
+------+------------------+------------+------------+

$ batlehub-cli owner add internal/Serilog bob --type user --role maintainer
Added user 'bob' as maintainer on internal/Serilog
```

---

## 8. Commands — publish

```
batlehub-cli publish <input> [-r <r>] [--name <n>] [--version <v>]
                             [--pom <pom.xml>] [--distribution <d>] [--component <c>] [--platform <p>]
```

Upload to a local or hybrid registry. **The registry decides the kind**: the
CLI asks the server what `-r` is and uses that kind's upload route, so a Helm
chart `.tgz` goes to Helm even though npm uses the same extension. The file name
is read only for a name and a version, and only when it reads as that kind's own
artifact.

| Kind | Input | Name and version from |
|------|-------|-----------------------|
| nuget | `.nupkg` | the server, from the embedded `.nuspec` |
| pypi | wheel or sdist | the file name |
| rubygems | `.gem` | the server |
| npm | `.tgz` | the file name (`name-version.tgz`, as `npm pack` writes it) |
| cargo | `.crate` | the file name (`name-version.crate`) |
| conda | `.conda`, `.tar.bz2` | the server, from `info/index.json` (`--platform` is only a fallback) |
| composer | `.zip` | the server, from `composer.json` (`--version` overrides it) |
| openvsx, vscode-marketplace | `.vsix` | the file name (`publisher.name-version.vsix`) |
| jetbrains-marketplace | plugin `.zip`/`.jar` | the server, from `plugin.xml` |
| galaxy | collection `.tar.gz` | the server, from `MANIFEST.json` |
| helm | chart `.tgz` | the server, from `Chart.yaml` |
| devfile | stack directory or `devfile.yaml` | the server, from `metadata` |
| deb | `.deb` | the server — needs `--distribution` and `--component` |
| rpm, pacman, apk | the package | the server |
| goproxy | a module directory (zipped here), or a module `.zip` with `--name <module>` | `go.mod`'s `module` line; `--version v1.2.3` is required |
| maven | a `.jar` with `--pom`, a `.pom` alone, or a `target/` directory | the pom's `groupId`/`artifactId`/`version` |
| terraform | a module `.tar.gz`, with `--name <namespace>/<name>/<provider> --version` | the flags |
| terraform | a provider directory of `terraform-provider-<type>_<version>_<os>_<arch>.zip`, with `--name <namespace>/<type>` | the archives' names |

When a name or version cannot be read and no flag gives it, `publish` exits 2
before sending anything.

- **goproxy** sends one `PUT` of `{version}.zip`; the server derives `.mod` and
  `.info` from it, so the publish is atomic. The zip carries no symlink, no
  `vendor/` or version-control directory and no nested module.
- **maven** sends the jar, the pom and their `.sha1`/`.md5` sidecars, the pom
  last. A run that stops halfway exits 7 and names what was not sent; **run it
  again**: a file the server already holds with the same bytes counts as sent.
- **terraform** providers send the version manifest, then one archive per
  platform. A re-run picks up from the manifest the same way, and a stopped run
  exits 7.
- **nix** is refused with the command to run instead: `nix copy --to` signs and
  uploads a closure, which is not one file. Proxy-only kinds (jetbrains, github,
  forgejo, gitlab, generic) and vendor mirrors (nodedist, sdkman, rustup) have
  nothing to publish to.

`-t/--type` names the registry's kind for a token that may publish to it but
not list it (a least-privilege CI token holding `releases:publish` alone).
Otherwise the registry's own kind decides, and a `-t` that disagrees with it
exits 2.

```bash
# NuGet
batlehub-cli publish Serilog.3.1.1.nupkg -r internal

# A Go module, from its directory
batlehub-cli publish ./mymod -r go-local --version v0.1.0

# Maven, after `mvn package`
batlehub-cli publish target/ -r maven-local

# A Terraform provider release directory
batlehub-cli publish dist/ -r tf-local --name acme/null

# Debian (distribution/component aren't in the file)
batlehub-cli publish hello_1.0-1_amd64.deb -r internal --distribution stable --component main
```

---

## 9. Commands — auth

```
batlehub-cli auth login [--provider <p>] [--profile <p>] [--no-browser]
batlehub-cli auth login --kubernetes-token-path <path>
batlehub-cli auth whoami
batlehub-cli auth token                      # print a credential (refreshing it first)
batlehub-cli auth token [--output raw|json] [--min-ttl <seconds>]
batlehub-cli auth token list
batlehub-cli auth token create --name <n> [--days <d>] [--role user|admin]
                               [--groups <g1,g2> | --all-groups]
batlehub-cli auth token delete <uuid>
batlehub-cli auth write-token-file [--path <p>] [--from-file <p>]
batlehub-cli auth status [--path <p>] [--json]
batlehub-cli proxy serve --registry-url <url> [--bind 127.0.0.1:0] [--contract <p>]
                         [--state-dir <d>] [--print-gallery-url]
```

### `auth login` {#auth-login}

Signs in through the server's OIDC provider and saves the token, with its
refresh token, to the `-P` profile. The CLI listens on `127.0.0.1` on a free
port and opens your browser at the server's sign-in. Once the provider has
signed you in, the server redirects the browser back to that port. The CLI
checks that the callback belongs to the sign-in it started, and then closes the
port. It waits up to five minutes.

The browser is `$BROWSER` when it is set, otherwise `xdg-open`, `open` or the
Windows URL handler. When none of them starts, the CLI prints the URL for you to
open. When the browser cannot reach `127.0.0.1` on the machine running the CLI
(an SSH session, a cloud workspace), use `--no-browser`. The CLI then prints the
URL, and you paste back the URL you land on.

The provider's configuration does not change: the server completes the exchange
as it does for the console, and accepts no `return_to` other than
`http://127.0.0.1:<port>/callback`. A server without loopback support ignores `return_to`,
so the login times out with a hint to use `--no-browser`. CI keeps
`BATLEHUB_TOKEN` or `--kubernetes-token-path`.

### `auth whoami`

Print the identity resolved from the current token:

```
$ batlehub-cli auth whoami
+----------+-----------------------+
| User ID  | alice@example.com     |
| Role     | admin                 |
| Provider | oidc                  |
| Groups   | nuget-maintainers, …  |
+----------+-----------------------+
```

### `me`: your own state {#me}

```
batlehub-cli me show                      # user, role, groups, as the server sees you
batlehub-cli me quota                     # your usage against each registry's quota
batlehub-cli me downloads [--denied] [--limit 50] [--days <n>]
batlehub-cli me advisories                # advisories on versions you published or pulled
batlehub-cli me namespaces                # the team namespaces you belong to
```

`me downloads --denied` lists only your refused downloads, each with its reason
code. Pass one to [`why`](#commands-security) for the full reason. Every `me`
command reads only your own rows.

### `auth token`

With no subcommand, print a credential for the configured server — the one
command whose job is to emit a secret, and what a broker shells out to. It
refreshes first when less than `--min-ttl` seconds are left, defaulting to the
same 120 seconds every other command already refreshes on, so there is no
second notion of freshness to keep in step.

```
$ batlehub-cli auth token --output json
{ "registry": "https://hub.example.dev", "token": "…", "kind": "oidc",
  "expires_at": "2026-09-04T21:40:00Z" }
```

Non-zero exit when there is no credential, so a caller substituting it into a
header does not send an empty bearer.

### `auth logout`

Discards the stored credential.

```
batlehub-cli auth logout [--profile <name>] [--keep-contract] [--path <file>]
```

It clears two stores, and only those: the profile's `token`,
`oidc_refresh_token` and `kubernetes_token_path` in
`~/.config/batlehub/config.toml`, and this server's entry in the credential
contract file. `server_url` and `registry` survive — they are settings, and a
logout that forgot which server you talk to would be a worse command.

**Local only.** There is no server-side session and no refresh-token revocation
endpoint, so a discarded OIDC refresh token stays valid at the identity provider
until it expires. Revoke it there if that matters; the command says so on every
run that cleared something.

Three details worth knowing:

- **It is per profile.** Each named profile is a separate credential, so
  `--profile ci` logs out of that one and leaves `default` alone.
- **It never deletes a file an entry points at.** A `from = "file"` contract
  entry names a path the CLI does not own — a projected Kubernetes token — so
  the entry goes and the file stays.
- **It does not need the server.** Every other command resolves a token first,
  which can mean a network refresh; this one is exempt, so logging out of a
  server that is down still works.

`--keep-contract` clears the profile and leaves the contract file alone, for
when the editor should keep working.

### `auth write-token-file` and `auth status` {#credential-contract}

The credential contract file ([RFC 0011](/rfc/0011-openvsx-login) §4.1) is how
a process that is *not* the CLI finds a credential — a patched editor, a
script, anything started by a desktop session that inherits neither your login
nor your environment.

```
$ batlehub-cli --server https://hub.example.dev auth write-token-file
https://hub.example.dev written to /home/you/.batlehub/state/vsx-token.json
```

`$BATLEHUB_HOME/state/vsx-token.json`, `0600`, written atomically. It is keyed
by origin and only this server's entry is touched, so one laptop pointed at
three BatleHubs keeps three credentials in one file — and unknown fields are
preserved, so a newer writer's additions survive an older CLI.

`--from-file <path>` records a **path to read** rather than the value: for a
projected Kubernetes token, or any secret something else keeps fresh. The
credential then never rests in the contract file at all.

```
$ batlehub-cli auth status
+-------------------------+------------+---------------------------+-------+---------+-----------+
| Registry                | Kind       | Token source              | State | Expires | Refresh   |
+-------------------------+------------+---------------------------+-------+---------+-----------+
| https://hub.example.dev | oidc       | inline (written by cli)   | ok    | 4m12s   | cli       |
| https://hub.k8s.dev     | kubernetes | file /var/run/…/token     | unset | —       | reresolve |
+-------------------------+------------+---------------------------+-------+---------+-----------+
  https://hub.k8s.dev: reading /var/run/…/token: No such file or directory
```

Every state is a resolution performed **now**, never a cached opinion: a stale
`ok` from before a token file rotated is the failure being debugged. `unset`
and a misconfigured source look identical from an editor and want opposite
fixes, which is why the reason is printed. No output path can emit a
credential — the row type has no field able to hold one.

### `auth token create`

Create a long-lived API token (requires an active OIDC session). The raw token is printed exactly once — store it immediately:

```
$ batlehub-cli auth token create --name ci-pipeline --days 90
Created token 'ci-pipeline' (role: user, expires: 2026-09-02)
Groups: none — this token sees only public and internal packages

Token (store this — it will not be shown again):
  bh_pat_XXXXXXXXXXXXXXXXXXXX
```

#### Groups on a token

A token carries **no groups by default**, so it sees `public` and `internal`
packages and nothing granted to a team. Name the groups it should carry:

```
$ batlehub-cli auth token create --name ci-pipeline --groups platform,release
Created token 'ci-pipeline' (role: user, expires: 2026-10-01)
Groups: platform, release
```

`--all-groups` is shorthand for every group you hold right now — it reads
`auth whoami` and sends that list:

```
$ batlehub-cli auth token create --name laptop --all-groups
```

Three things worth knowing before you use them:

- **You can only give a token groups you hold.** Naming one you do not is
  refused with a `403` that names it, not silently dropped — a token that is
  quietly narrower than asked for shows up later as a pipeline that cannot see a
  package, with nothing connecting the two.
- **Spell the group as the server resolves it.** `auth whoami` prints the
  resolved ids, and they are not always the ones the operator has in mind: a
  Kubernetes group reaches this model prefixed with its provider name
  (`k8s:system:serviceaccounts:digital`, not
  `system:serviceaccounts:digital`) unless a `role_mappings` entry renames it.
- **It is a snapshot, not a subscription.** The groups are taken once, at
  creation, and never re-resolved — a token has no session to re-resolve from.
  Leaving a team does not narrow a token that already carries it; the token's
  expiry (90 days at most) and revoking it are what bound that, which is why
  offboarding should revoke tokens. `auth token list` shows what each one
  carries.

```
$ batlehub-cli auth token list
+--------------------------------------+-------------+------+------------+---------------------+
| ID                                   | Name        | Role | Expires    | Groups              |
+--------------------------------------+-------------+------+------------+---------------------+
| 0c0f…                                | ci-pipeline | user | 2026-10-01 | platform, release   |
| 7a31…                                | laptop      | user | 2026-09-20 | -                   |
+--------------------------------------+-------------+------+------------+---------------------+
```

Use the resulting token as `BATLEHUB_TOKEN` in CI:

```yaml
# GitHub Actions example
- run: cargo publish --registry batlehub
  env:
    BATLEHUB_TOKEN: ${{ secrets.BATLEHUB_TOKEN }}
```

---

## 10. Commands — proxy {#gallery-proxy}

The local gallery proxy ([RFC 0011](/rfc/0011-openvsx-login) §4.4), for an
editor whose core cannot send a credential — stock VS Code and every build
of it that reads its gallery from `product.json`. It is a loopback server
your own CLI runs in front of one BatleHub VSX registry; the editor's
gallery points at it, and it attaches the credential from the contract
file, so the editor never holds one.

```
$ batlehub-cli proxy serve --registry-url https://hub.example.dev/proxy/vsx
gallery proxy for https://hub.example.dev/proxy/vsx on 127.0.0.1:41873
  extensionsGallery.serviceUrl = http://127.0.0.1:41873/9f2c…/vsx/vscode/gallery
  credential: /home/you/.batlehub/state/vsx-token.json
  state:      /home/you/.batlehub/state/gallery-proxy.json
  not signed in: a search shows the sign-in entry until you are
```

Three things to know:

- **The path is the secret, not the port.** Everything is served under a
  per-run random segment; anything outside it is a `404`. In a workspace
  pod loopback is shared by every container, so a proxy on a well-known
  port would hand your credential to any process in it. `--bind` accepts
  loopback addresses only. Inside the segment the path is still checked
  rather than trusted: a `.` or `..` in it is a `404` (encoded forms
  included), because the credential is attached to whatever the forwarded
  path resolves to, and a request that climbed out of the registry's own
  prefix would reach the rest of the API holding your token. Only `GET`,
  `HEAD` and `POST` are forwarded, which is everything the gallery protocol
  uses. `--print-gallery-url` prints the URL alone, for
  a startup script that writes it into the editor's `product.json`; the
  same URL is in `gallery-proxy.json`, mode `0600`.
- **Signing in is something the editor shows you, not an error it hides.**
  With no credential a search answers one entry, *Sign in to BatleHub*,
  whose details are the sign-in steps; a lookup by name answers nothing,
  so the editor's installed extensions are never marked unavailable. Run
  `auth login`, then `auth write-token-file`: the proxy re-reads the file
  on every request, and the next search is the registry's — press the
  view's Refresh, the view answers a repeated search from its own cache.
- **Every URL in a gallery answer is rewritten onto the proxy**, so the
  `.vsix` the editor downloads after you click Install goes through it
  too, with the credential. A URL on any other origin is left alone.

One thing the proxy cannot change: **a current VS Code installs only signed
packages from a gallery.** The Extensions view greys out Install on any
entry without a signature asset with *This extension is not signed by the
Extension Marketplace* — which is why a registry signs what it hosts
([`[registries.vsx_signing]`](/guide/configuration#vsx-signing)) and relays
the upstream's signature for what it proxies. That turns the button on. The
editor's own verifier still accepts the Microsoft marketplace's signature and
no other, so on a stock build the install itself needs
`extensions.verifySignature: false`; the builds that ship with a
non-Microsoft gallery (code-server, VSCodium, che-code) ship it off for that
reason. On a stock build set it yourself, in the editor's settings (for a
server build, in `<server-data-dir>/data/User/settings.json`). The sign-in
entry is deliberately not signed: it is a page to read, and its button stays
grey.

`tests/heavy/vsx_login.sh` drives the real VS Code core's CLI through the
proxy, with and without a credential; `tests/heavy/vsx_view.sh` opens the
same build's Extensions view in a browser and reads what it shows.

## 11. Commands — vsx {#vsx}

The client side of a registry's VSIX signature
([RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry)). `keygen`
prints a seed for `[registries.vsx_signing]` and the key id it derives, and
writes nothing:

```
$ batlehub-cli vsx keygen
seed_hex   = "9d61b19d…"
key_id     = "3f1e0a9c7b2d4e61"
public_key = "d75a9801…"   # the trusted_keys form
```

`verify` checks a downloaded `.vsix` against the archive the registry serves
as its `VsixSignature` asset and the key its `PublicKey` asset names — the
Ed25519 signature over the file's bytes, and the manifest against the file's
entries:

```
$ batlehub-cli vsx verify weebo-bridge-notify-0.5.0.vsix \
    --registry-url https://hub.example.dev/proxy/vsx --id batleforc.weebo-bridge-notify --version 0.5.0
ok: weebo-bridge-notify-0.5.0.vsix is signed by key 3f1e0a9c7b2d4e61… (24503 bytes, manifest matches)
```

`--signature <archive> --public-key <pem|hex|file>` verifies offline. It does
not run the editor's own verifier, which accepts the marketplace's signature
and no other.


## 12. Commands for administrators

These groups act on the server rather than on one package. A command marked
`[admin]` in its `--help` needs an instance-tier grant; the server decides, so a
command an operator has delegated works for whoever holds the grant. Groups take
the registry from `-r` (or `BATLEHUB_REGISTRY`, or the profile) wherever the
registry is a filter rather than the object.

### Quota

```
batlehub-cli quota list  [-r <registry>]
batlehub-cli quota reset -r <registry> <user>
```

### IP blocks

```
batlehub-cli ip-block list
batlehub-cli ip-block add    <ip> [--reason <text>]
batlehub-cli ip-block remove <ip>
```

### Server

```
batlehub-cli server config reload     # trigger hot reload on the server
batlehub-cli server config changes    # view change history
batlehub-cli server config show       # the active configuration, as TOML
batlehub-cli server config validate <file>
batlehub-cli server config from-file <file>
batlehub-cli server config pending    # the staged configuration, and what applying it changes
batlehub-cli server config apply      # apply it (destructive: asks first)
batlehub-cli server config discard    # drop it (destructive: asks first)
batlehub-cli server config warnings   # what the running configuration's validation warned about
batlehub-cli server stats             # cache hit rate, bytes served, aggregate counters
batlehub-cli server health            # per-registry and per-backend health
```

`stats` and `health` are read-only. `health` is the one to run when a client
reports failures against one registry and not the others.

### Cache

```
batlehub-cli cache warm      -r <registry> [--packages pkg1,pkg2] [--paths p1,p2]
batlehub-cli cache delete    -r <registry>
batlehub-cli cache evict     -r <registry> [--dry-run]
batlehub-cli cache coherence -r <registry> [--dry-run]
```

`delete`, and `evict` and `coherence` without `--dry-run`, are
[destructive](#confirmation).

### Import

Runs a registry's configured `[[release_imports]]` now, whatever their interval
says ([RFC 0021](/rfc/0021-forge-releases-into-registries)).

```
batlehub-cli import <registry> [--tag <tag>] [--repo <owner/name>]
```

`--tag` imports that tag instead of what the configuration selects, and is the
only way to reach a pre-release deliberately: `latest` will not choose one.
`--repo` runs one of several imports configured into the same registry.

```
$ batlehub-cli import vsx-local
Release import on vsx-local: imported 3, skipped 11, errors 1

+--------+-----------------------------+-------------------------------+
| tag    | asset                       | error                         |
+=======================================================================+
| v2.1.0 | weebo-bridge-2.1.0.vsix     | manifest names no publisher   |
+--------+-----------------------------+-------------------------------+
```

`skipped` counts assets whose version this registry already holds. It is not an
error — it is what makes a re-run free. Failures are listed rather than counted,
because a tag with a reason is something you can act on.

Every import is a **publish**, and it runs as the principal the configuration
names, not as you. Asking for one needs `cache:warm`; the publish itself needs
that principal's own `releases:publish`, so an import can be refused as a publish
under a subject that is not yours.

### Banner

```
batlehub-cli banner set    "Maintenance at 22:00 UTC" [--level info|warning|error]
batlehub-cli banner delete
```

### Notifications

Channels are declared in `config.toml` under `[notifications]` and are read-only
here; subscriptions are rows this command manages.

```
batlehub-cli notification channels        # the configured outbound channels
batlehub-cli notification list            # the subscriptions
batlehub-cli notification delete <id>
```

Neither listing prints a webhook URL or a signing secret. See
[`[notifications]`](/guide/configuration#_3-8d-notifications-optional) for the
configuration side.

### Grants

The package and version tiers of the authorization hierarchy — the two a config
file cannot enumerate. Registry- and namespace-tier grants stay in `config.toml`.

```
batlehub-cli grant list   <registry>/<name>[@<version>]
batlehub-cli grant set    <registry>/<name>[@<version>] --subject <s> --actions <a1,a2>
batlehub-cli grant remove <registry>/<name>[@<version>] --subject <s>
```

```
$ batlehub-cli grant set npm1/@acme/billing \
      --subject group:oidc1:eng --actions releases:read,releases:list
Granted releases:read, releases:list on npm1/@acme/billing to group:oidc1:eng

$ batlehub-cli grant list npm1/@acme/billing
+----------------------------------+------------------------------+---------------------------+-----------+
| Node                             | Subject                      | Actions                   | Source    |
+----------------------------------+------------------------------+---------------------------+-----------+
| package:@acme/billing            | group:oidc1:eng              | releases:read,            | root      |
|                                  |                              | releases:list             |           |
| package:@acme/billing            | user:alice                   | releases:publish,         | ownership |
|                                  |                              | owners:read, owners:write |           |
| version:@acme/billing@2.4.0-rc.1 | group:oidc1:release-managers | releases:read             | root      |
+----------------------------------+------------------------------+---------------------------+-----------+
```

- **A coordinate with a version addresses one version**, so
  `npm1/@acme/billing` is a package and `npm1/@acme/billing@2.4.0` is a version.
- **What `set` prints is what was stored.** `--actions releases:*` names one verb
  and stores several; the output is the expanded set. It also prints warnings for
  a grant that is legal but inert — one a broader tier already gives, or one on a
  yanked version.
- **`Source: ownership` rows are not editable here.** They follow the package's
  owner list; change them with [`owner`](#commands-owner). Editing one earns a `409`.
- Needs `grants:write` (`grants:read` for `list`), which `role:admin` holds.

### Exposure and flags

Who pulled a flagged version, and what the configured sources have flagged
([RFC 0002](/rfc/0002-vulnerability-flags-and-exposure)).

```
batlehub-cli exposure list [-r <registry>] [--package <p>] [--source <s>]
                           [--min-effect inform|warn|gate|hard_block]
                           [--when any|before-flag|after-flag]
                           [--from <t>] [--to <t>] [--after <cursor>] [--limit 100]

batlehub-cli flag list [-r <registry>] [--package <p>] [--source <s>]
                       [--effect inform|warn|gate|hard_block] [--include-dead]
                       [--page 0] [--per-page 50]
```

`exposure list` gives one row per consumer, newest pull first, with how many
pulls preceded the flag — the retroactive case, where the advisory landed after
the download. It is cursor-paged: `--after` takes the cursor the previous page
printed. `--min-effect` means "or stronger".

`flag list` shows only live flags unless `--include-dead` adds the revoked and
expired ones.

```
batlehub-cli flag create <registry>/<name>[@<version>] --source <s> --external-id <id>
                         --effect inform|warn|gate|hard_block --summary <text>
                         [--severity <s>] [--kind <k>] [--url <u>] [--expires-at <t>]
batlehub-cli flag delete <external-id> --source <s>
```

`flag create` and `flag delete` act as a `[[flag_sources]]` entry, not as you:
they sign each request with the source's shared secret, read from
`BATLEHUB_FLAG_SECRET` so it stays out of the shell history. Without a version,
the flag covers every version. Sending the same `--external-id` again updates
the flag. `flag delete` revokes it and is [destructive](#confirmation): the
versions it held are served again.

### Policy and exemptions {#policy}

```
batlehub-cli policy show   <registry>/<name>[@<version>]
batlehub-cli policy set    <registry>/<name>[@<version>] -f <policy.json | ->
batlehub-cli policy set    <registry>/<name>@<version> --gate <g> --until <t> --reason <text>
batlehub-cli policy delete <registry>/<name>[@<version>] [--gate <g>]

batlehub-cli exemption list   -r <registry>
batlehub-cli exemption add    <registry>/<name>@<version> --gate <g> --until <t> --reason <text>
batlehub-cli exemption remove <registry>/<name>@<version> --gate <g>
```

A coordinate without a version addresses the package's policy, and one with a
version addresses that version's policy. In the JSON file, a field left out
inherits from the tier above. `policy set --gate` and `exemption add` are the
same call: they exempt one version from one gate until `--until` (RFC 3339).
The server records whether you exempted a version you published yourself, and
`add` notes it. `policy delete` is [destructive](#confirmation).

### Upstream audit {#upstream}

```
batlehub-cli upstream status      <registry>/<name>
batlehub-cli upstream recheck     <registry>/<name>[@<version>]
batlehub-cli upstream disappeared [-r <registry>] [--state missing|disappeared|restored]
                                  [--page 0] [--per-page 50]
```

These show what the upstream no longer lists
([RFC 0014](/rfc/0014-upstream-disappearance)). `recheck` probes the upstream
now instead of waiting for the next sweep.

### Access check

```
batlehub-cli authz check <registry>/<name>@<version>
                         [--resource releases:read] [--user <id>]
                         [--role anonymous|user|admin] [--groups <g1,g2>]
```

Simulates one decision without making a request as that identity. See also
[`authz explain`](#commands-authz), which answers the broader question of what a
subject may do and names the tier that granted each verb.

### Visibility, namespaces and users

```
batlehub-cli visibility show <registry>/<name>
batlehub-cli visibility set  <registry>/<name> public|internal|team

batlehub-cli namespace list   -r <registry>
batlehub-cli namespace claim  -r <registry> <prefix> <group-id>
batlehub-cli namespace remove -r <registry> <prefix>
batlehub-cli namespace signing-key list   -r <registry> <namespace>
batlehub-cli namespace signing-key set    -r <registry> <namespace> --key-id <id> --armor <key.asc | ->
batlehub-cli namespace signing-key delete -r <registry> <namespace> <key-id>

batlehub-cli user list                      # the blocked users
batlehub-cli user block   <user-id> [--reason <text>]
batlehub-cli user unblock <user-id>
```

All three change who can do what, and take effect immediately.
`namespace remove` reopens a prefix to anyone who can publish in the registry,
which is the one here whose blast radius is larger than it looks.

### SBOM

```
batlehub-cli sbom show   <registry>/<name>@<version> [--sbom-format cyclonedx]
batlehub-cli sbom export [-r <registry>] [--from <t>] [--to <t>]
                         [--sbom-format cyclonedx] [-o <file>]
```

`show` always prints JSON. `export` writes to the named file, or stdout without
`-o`. See [`docs/guide/sbom.md`](/guide/sbom) for the formats and what they
contain.

### Audit log

```
batlehub-cli audit log    [-r <registry>] [--user <id>] [--from <date>] [--to <date>] [--denied-only]
batlehub-cli audit purge  --before <RFC 3339 instant>
batlehub-cli audit verify [--from <t>] [--to <t>] [--head <digest>]
```

`purge` is [destructive](#confirmation).

#### Export for compliance

```
batlehub-cli audit export [--from <t>] [--to <t>] [-r <registry>]
                          [--action delete,retention_reclaim]
                          [--format json|csv] [-o <file>]
```

Distinct from `audit log` above, which is the interactive paged query. This one
is the export an auditor keeps: a whole window in one document, written to `-o`
or to stdout. For the identity-scoped question — what did *this account* pull —
see [`audit pulls`](#commands-audit), which aggregates rather than listing every
event.

### Retention

```
batlehub-cli retention show  <registry> [--show-kept]
batlehub-cli retention apply <registry> [--show-kept] [--dry-run]
```

Reclaims locally published versions the registry's `[registries.retention]`
policy no longer keeps. `show` reports and changes nothing. `apply` is only half
the interlock: the registry also needs `dry_run = false`, and `apply` is
[destructive](#confirmation). Two decisions in two places, because a reclaimed
artifact may exist nowhere else.

`--show-kept` prints every surviving version and the condition that saved it,
which is how you check a policy against what it actually does before arming it.

Pinning a single version against retention is
[`package pin`](#commands-version).

### Air gap

```
batlehub-cli air-gap missing [-r <registry>] [--kind artifact|document|checksum|ref|unmirrored_host]
batlehub-cli bundle list
```

What a disconnected instance was asked for and did not hold, and what came
across the gap. See [the air-gap runbook](/operations/air-gap).

### Data-subject requests

```
batlehub-cli gdpr erase  --user <subject> [--ignore-open-decisions]
batlehub-cli gdpr export --user <subject>
```

`erase` is [destructive](#confirmation). `--ignore-open-decisions` erases even
while an open decision names the subject; it is not a way to skip the
confirmation, which still needs `--yes` in a script.

### Removed spellings {#removed-spellings}

The grammar of RFC 0040 replaced every spelling below. None of them runs any
more: each one is refused as a usage error (exit 2), so a script that still
uses one fails loudly instead of doing something else.

| Before | Now |
| --- | --- |
| `config init/show/set` | `profile init/show/set` |
| `registry info <name>` | `registry show <name>` |
| `version yank/unyank/delete/pin/unpin <reg> <name> <ver>` | `package yank/… <reg>/<name>@<ver>…` |
| `admin bulk yank/unyank/delete <reg> <name@ver>…` | `package yank/unyank/delete <reg>/<name>@<ver>…` |
| `admin deprecate/undeprecate/unlist/relist <reg> <name> <ver>` | `package deprecate/… <reg>/<name>@<ver>…` |
| `package versions <reg> <name>` | `package versions <reg>/<name>` |
| `owners list/add/remove <reg> <name>` | `owner list/add/remove <reg>/<name>` |
| `admin visibility get/set <reg> <name>` | `visibility show/set <reg>/<name>` |
| `admin grants list/set/rm <reg> <target>` | `grant list/set/remove <reg>/<target>` |
| `admin namespace list/claim/release <reg> …` | `namespace list/claim/remove -r <reg> …` |
| `admin users list-blocked/block/unblock` | `user list/block/unblock` |
| `admin quota list/reset` | `quota list/reset` |
| `admin ip-block …` | `ip-block …` |
| `admin config reload/changes/view/validate/from-file` | `server config reload/changes/show/validate/from-file` |
| `admin stats`, `admin health` | `server stats`, `server health` |
| `admin cache warm/clear/evict/coherence <reg>` | `cache warm/delete/evict/coherence -r <reg>` |
| `admin import <reg>` | `import <reg>` |
| `admin banner set/clear` | `banner set/delete` |
| `admin retention <reg> [--reclaim]` | `retention show <reg>` · `retention apply <reg>` |
| `admin exposure` | `exposure list` |
| `admin bundles` | `bundle list` |
| `admin air-gap-missing` | `air-gap missing` |
| `admin flags list` | `flag list` |
| `admin audit-log [--purge-before]` | `audit log` · `audit purge --before` |
| `admin export-audit-log` | `audit export` |
| `admin audit verify` | `audit verify` |
| `admin gdpr erase --force` | `gdpr erase --ignore-open-decisions` |
| `admin sbom get/export` | `sbom show/export` |
| `admin notifications …` | `notification …` |
| `admin access-check --registry --package --version` | `authz check <reg>/<name>@<ver>` |
| `verdicts …` | `verdict …` |
| `auth token revoke` | `auth token delete` |
| `--csv` | `--format csv` |
| `mise plan/export --out` | `--output` (`-o`) |

One alias stays: `proxy serve --registry` and `vsx verify --registry` (a URL)
are still accepted for `--registry-url`, because the editor extensions pass
them. The `registry:name@version` coordinate is accepted for good, with a
`warning:` naming the `registry/name@version` form.

Two spellings changed with no alias, because the global `--format` took their
name: `admin sbom get/export --format` is now `--sbom-format`, and
`admin export-audit-log --format` is now the global `--format`.
`auth token --output raw|json` still works; `--format raw|json` is the new
spelling.

---

## 13. Commands — mise {#commands-mise}

The air gap's four verbs ([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate)).
The first three run against a **connected** instance; the last runs against
the disconnected one.

```
batlehub-cli mise plan   [--lock mise.lock] [--platform <p>[,<p>]|all] [--include-mise] [-o plan.json]
batlehub-cli mise seed   [--plan plan.json] [--verify]
batlehub-cli mise export [--plan plan.json] --sign-key <file> [-o estate.bhub] [--bundle-id <id>]
batlehub-cli mise import <bundle>
```

`plan` turns a lock into the bill of materials, offline: it reads the lock and
the server's registry list and resolves nothing over the network. `--platform`
defaults to this machine's; `all` takes every platform the lock records.
`--include-mise` carries mise's own release, so the estate can upgrade the
tool that reads the next plan.

`seed` fetches every planned entry *through* BatleHub — fetching is warming —
and compares the digest of what the server served with the lock's. Non-zero
exit means the bundle would be incomplete or wrong, so it works as a CI gate.
`--verify` adds what the supply-chain layer said and fails on a denied
verdict.

`export` builds the signed, content-addressed bundle and prints the **public**
key it signed with, which is the value the disconnected instance needs in
`air_gap.bundle_trusted_keys`. The signing key is a file, not a flag: a key on
a command line is a key in the shell history.

`import` verifies the signature before reading a single blob, then writes each
blob, the metadata entry that finds it, and the verdict and ref resolution it
carried. Importing the same bundle twice writes once and says so.

---

## 14. Commands — profile {#commands-profile}

The local configuration file: which server, which credential, which default
registry. The server's own configuration is [`server config`](#server).

```
batlehub-cli profile init                # interactive first-run wizard
batlehub-cli profile list                # the profiles, `*` on the one in use
batlehub-cli profile show [<name>] [--key <key>]   # token masked
batlehub-cli profile set server_url https://batlehub.example.com
batlehub-cli -P prod profile set registry internal
batlehub-cli profile use prod            # the profile used when -P is not given
```

Valid keys for `profile set`: `server_url`, `token`, `registry`. `set` writes
the `-P` profile (or the one in use). `use` writes a top-level `current` key; an
older CLI ignores it and keeps reading `default`. `profile set token …` stores a
static token, which `auth login` would refresh and this never does.

---

## 15. Commands — setup

```
batlehub-cli setup detect [--dir <path>] [--depth <n>] [--offline] [--json]
batlehub-cli setup ide [--offline] [--json]
```

`setup detect` scans a directory for project manifests (`Cargo.toml`, `go.mod`,
`package.json`, `pyproject.toml`, `pom.xml`, `composer.json`, `*.gemspec`,
`*.nuspec`, `*.csproj`, `*.tf`, `environment.yml`) and prints the configuration
snippet for each package manager it finds. `setup ide` does the same for the
editor you are running in (VS Code / VSCodium → OpenVSX or the VS Code
Marketplace; JetBrains → the JetBrains Marketplace).

Both ask the server which registries exist, so the snippets carry the real
registry name and the URL that registry actually answers on — its own subdomain
when [host-based routing](../rfc/0001-subdomain-routing.md) advertises one,
`{server}/proxy/{name}` otherwise. Each run ends with the matching `~/.netrc`
stanzas, one per host: credentials are matched by hostname, so a host-routed
registry needs its own entry.

If the server cannot be reached the commands still work — they print `<registry>`
placeholders and say so on stderr. `--offline` skips the request entirely.

### Configure this machine: `setup apply`, `status`, `remove`, `env` {#setup-apply}

```
batlehub-cli setup apply  [--yes] [--only <client>,…] [--skip <client>,…]
                          [--pick <type>=<name>]… [--maven-mirror-of central|external|all]
                          [--shell-rc auto|none] [--insecure-http] [--allow-foreign-hosts] [--json]
batlehub-cli setup status [--only …] [--skip …] [--pick …] [--shell-rc auto|none]
                          [--insecure-http] [--allow-foreign-hosts] [--json]
batlehub-cli setup remove [--yes] [--only …] [--skip …] [--json]
batlehub-cli setup env    [--shell sh|fish] [--only …] [--skip …] [--pick …]
                          [--insecure-http] [--allow-foreign-hosts] [--refresh]
```

`detect` prints snippets for a project; `apply` writes them for a machine. It
asks the server which registries exist, finds which clients are on `PATH`, and
writes each one's **user-level** configuration
([RFC 0038](../rfc/0038-client-setup-apply.md)). The server URL is the only
thing to provision — `--server`, `BATLEHUB_SERVER` or the profile — so a dev
environment hook can be one line:

```sh
BATLEHUB_SERVER=https://pkg.example.com batlehub-cli setup apply --yes
```

`apply`, `status`, `remove` and `env` run on Linux and macOS; on Windows they
refuse, and `detect` prints the snippets instead.

| Client | File | What is written |
| --- | --- | --- |
| cargo | `$CARGO_HOME/config.toml` | `[source.crates-io] replace-with`, plus `[registries.batlehub]` for `cargo publish` |
| npm, pnpm, bun, yarn 1 | `~/.npmrc` | `registry=` |
| yarn 2+ | `~/.yarnrc.yml` | `npmRegistryServer:`, plus `unsafeHttpWhitelist` for a plain-HTTP server, which yarn otherwise refuses |
| pip | `~/.config/pip/pip.conf` | `[global] index-url` |
| uv | `~/.config/uv/uv.toml` | a default `[[index]]` |
| go | `$GOENV`, else `~/.config/go/env` | `GOPROXY`, with no `,direct` fallback |
| maven | `~/.m2/settings.xml` | the whole file, a `<mirror>` (only when absent; once you edit it, it is yours) |
| mise | `$MISE_CONFIG_DIR/conf.d/batlehub.toml` (`~/.config/mise`) | `[settings.url_replacements]` for github, forgejo, npm, cargo and openvsx registries that proxy the public host (`api.github.com`, `codeberg.org`, …; one in front of another forge gets none, and says so); for every generic registry, one rule per upstream it mirrors (`^<upstream>/(.+)` onto the registry), after the others; `node.mirror_url` for a nodedist one. Both kinds of rule need the upstreams, which the registry list only shows to a caller holding [`upstreams:read`](/guide/access-control#verbs); the list is read anonymously, so that grant is to `*` |
| rustup, node | `~/.config/batlehub/env.sh` | `RUSTUP_DIST_SERVER`, `NODEJS_ORG_MIRROR` and the nvm/fnm/n mirrors, sourced from the **top** of `~/.bashrc` (above its non-interactive `return`) or from `~/.zshenv` (`$ZDOTDIR/.zshenv` when `ZDOTDIR` comes from outside it; a `conf.d` file for fish). On macOS, bash gets the login file it reads — the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile`. The shell is `$SHELL`, else the account's in `/etc/passwd`. `bash -c` reads no rc file: use `BASH_ENV` or `setup env` there |
| conda, mamba, micromamba | `~/.condarc` | `channels:` with the registry, then `nodefaults` |
| composer | composer's `config.json` (`$COMPOSER_HOME`, else Composer's own XDG or `~/.composer` choice) | the whole file — a `"//"` key marks it — with the registry under `repositories` and packagist turned off; only when `setup` created the file. No credential is written |
| gem | `~/.gemrc` | `:sources:` |
| dotnet, nuget | `~/.nuget/NuGet/NuGet.Config` (under `$DOTNET_CLI_HOME` when set) | the whole file: `<clear/>` and one source; only when `setup` created it |
| terraform, tofu | `~/.terraformrc` | a `provider_installation` block with the registry as `network_mirror` (https only: terraform refuses a plain-HTTP mirror) |
| ansible-galaxy | `~/.ansible.cfg` | `[galaxy] server_list = batlehub` and `[galaxy_server.batlehub]` |
| nix | `~/.config/nix/nix.conf` | `extra-substituters`; your user must be in the daemon's `trusted-users`, which `setup` says rather than writes |
| sdkman | `~/.config/batlehub/env.sh` | `SDKMAN_CANDIDATES_API`, `SDKMAN_BROKER_API` |
| mise, for gitlab | the mise drop-in above | `url_replacements` for `gitlab.com`, as for github and forgejo |
| helm | nothing | helm rewrites `repositories.yaml` itself and drops comments, so no block would survive: `status` reads the file and reports `manual` with the `helm repo add` to run, until a repository there has the registry's URL |
| apt, dnf/yum, pacman, apk, devfile | nothing | `print`: the snippet, because the file is root's (or the cluster's, for a Che devfile registry) |

- **Without `--yes`, nothing is written**: the diff of every file is printed.
- **Only what is between the markers is ever touched.** A block starts with
  `# >>> batlehub-cli setup … >>>` and ends with `# <<< batlehub-cli setup <<<`; files
  managed whole (`settings.xml`, the drop-ins) carry the start marker on their
  first line. `remove` gives every file back byte for byte.
- **What you edit stops being managed.** The marker records a hash of what was
  written: a block or drop-in changed since (by hand, or `go env -w`) is
  `conflict` and left alone by `apply` and by `remove` alike — your
  `GOPROXY`, the `<servers>` you added to `settings.xml`. Delete the marker
  lines to own the setting, or the whole block (the whole drop-in) to have it
  rewritten.
  The exact lines `apply` would write, found without markers — pasted from
  the Setup Guide, or a block that `npm config set`, `npm login`,
  `pip config set` or `yarn config set` rewrote without comments — are
  `up-to-date` and stay yours: `apply` does not take them over, and `remove`
  only points such a file out (`--server` known) and edits nothing.
- **A setting you own is never edited.** If your part of the file already sets
  what the block would — a `registry=` line, a `[global]` section in
  `pip.conf`, your own `settings.xml` — or an environment variable overrides
  the file (`GOPROXY`, `NPM_CONFIG_REGISTRY`, `PIP_INDEX_URL`, a `PIP_CONFIG_FILE`
  that exists, pnpm's own global `rc`/`auth.ini`, …), the client is
  `conflict`: skipped, with the snippet to merge by hand. So is a second index
  the client asks besides the instance — `extra-index-url` or an
  `[install] index-url` in `pip.conf`, another `[[index]]` in `uv.toml`,
  `PIP_EXTRA_INDEX_URL`, `UV_EXTRA_INDEX_URL` — since a version the instance
  withholds would install from it; and another mise config file
  (`config.local.toml`, another `conf.d` file) that sets the same table, which
  mise keeps from one file only. A setting that agrees with the block is not
  a conflict: the URL it writes, an empty variable, a config-file variable
  naming the very file, cargo's `config` symlinked to `config.toml`. Variables
  are read from the environment `apply` runs in and from the rc file it
  manages; one exported from another dotfile is not seen. When `env.sh` is in
  conflict, no rc line or fish file is written to source it.
- **Only `proxy` and `hybrid` registries are used**, and only those the server
  says you can install from (`installable`): a `local` one cannot stand in for
  crates.io or npmjs, and one listed for its metadata alone would fail every
  download. Two candidates of one type are `ambiguous` until
  `--pick npm=<name>` picks one (not `--registry`, the global default-registry
  flag); the pick is recorded in the block, and later runs keep it. If the
  picked registry goes away, one remaining candidate is switched to with a
  note; none or several leave the block `orphaned`: kept on its dead URL, so
  the client fails instead of going back to the public registry, until
  `--pick` or `remove`. `--maven-mirror-of` is kept the
  same way.
- **`--only` / `--skip` select files, not lines**: a file shared by several
  clients (`~/.npmrc`, `env.sh`) is touched when one of them is selected and
  none is skipped, and is always written for all of them.
- **The list is read anonymously**, so a registry closed to anonymous reads is
  never written into a client that sends no credential; publishing still goes
  through `auth login` and `.netrc`.
- **An unreachable server writes nothing** (exit 1), unlike `detect`. So does a
  plain `http://` server outside loopback (`--insecure-http` to accept). A
  registry whose `public_url` is not on the server's host or a subdomain of it
  (`--allow-foreign-hosts`), or is plain HTTP on an HTTPS server
  (`--insecure-http`), stops only its own clients: they are `conflict`, every
  other file is written, and the run exits 6. A URL or registry name holding a
  character a config file could misread (a newline, a quote, credentials) is
  refused the same way.
- **One broken file is that file's `conflict`**, not every client's: a
  missing marker line or invalid TOML stops only that client, and `remove`
  still removes the rest. A file that is not UTF-8 (a latin-1 `~/.bashrc`)
  keeps every byte.
- **An empty registry list is refused** (exit 1): a half-started server, or
  one whose registries are all closed to anonymous reads, would otherwise
  have a hook's `apply --yes` take out every block — `remove` does that when
  asked. The list is fetched without following a redirect off the server's
  host or down to plain HTTP.
- **A symlinked dotfile** is written through to its target (created if the
  link dangles; `remove` deletes the target and keeps the link), unless the
  target belongs to another user than the link. A read-only file is left
  alone; a hard link is broken by the atomic rename; a bind-mounted file
  (Che's `subPath` automount) is written in place.
- **Run it as the user, not as root**: `apply --yes` and `remove --yes` refuse
  to run as root with a `HOME` root does not own — every path there is the
  user's to symlink into system files. In a root hook:
  `runuser -u <user> -- batlehub-cli setup apply --yes`.
- **Maven** reads `${user.home}/.m2`, and Java takes `user.home` from the
  password database, not `$HOME`: where the two differ (some containers),
  pass `-Duser.home` or set `HOME` to match.

`status` prints one row per file: `not-installed`, `no-registry`, `ambiguous`, `manual` (helm: a command to run), `print` (a snippet for a file `setup` never writes),
`absent`, `up-to-date`, `drifted`, `orphaned` (its registry is gone; the block
is kept — `--pick` another or `remove` it) or `conflict`. `env` prints the env-only variables for
`eval "$(batlehub-cli setup env)"`, for a shell `--shell-rc none` leaves alone
(`none` leaves rc files out entirely: it neither writes nor removes a block).
`env` runs at every shell start, so it prints the `env.sh` `apply` wrote
without asking the server when that file is unedited, for the same server
(or any, when no server is set in that shell) and not overridden by a
variable already set — a registry renamed since shows up at the next
`apply`, or with `--refresh` or `--pick`, which always ask. Otherwise it waits
2 s for the server, then prints the `env.sh` written last time when the
server cannot answer (unreachable, an error status, an unreadable or empty
list — a bad `--pick` is still an error) under the same rules: that server's
URLs only, nothing over a variable you set, and only the variables `apply`
writes, as plain URLs. It
prints nothing, exit 6, when `env.sh` is `conflict` or `ambiguous`: `eval`
would override the variable `apply` refused to. `status` takes the
`--shell-rc` given to `apply`.

Exit codes, for a hook or a CI step: `0` nothing left to do, `2` a usage
error, `1` an error
(a file that could not be written does not stop the others; each is
reported), `3` done but a client needs a look (`conflict`, `ambiguous`; for
`status`, anything not settled; for `remove`, a file left alone; for
`apply`, also an `orphaned` block, and env-only
variables no rc file sources — an unknown shell: pass `--shell-rc none` and
`eval "$(batlehub-cli setup env)"` yourself). `apply`
without `--yes` exits as the real run would, 0 even with files to write.

---

## 16. TUI mode

```
batlehub-cli tui
# or
task cli:tui
```

The TUI is a full-screen terminal interface built with [ratatui](https://ratatui.rs).

### Screens

```
╔ BatleHub — Registries ═══════════════════════════════╗
║ > cargo    (cargo  ) [proxy ]                         ║
║   internal (nuget  ) [hybrid]                         ║
║   pypi     (pypi   ) [local ]                         ║
╚══════════════════════════════════════════════════════╝
 q:quit  ↑↓:navigate  Enter:select  p:publish  ?:help
```

| Screen | How to reach |
|--------|--------------|
| Registry list | Launch / `Esc` from package list |
| Package list | `Enter` on a registry |
| Version detail | `Enter` on a package |
| Publish wizard | `p` from registry list |
| Help | `?` from any screen |

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| `q` / `Ctrl-C` | Quit |
| `Esc` | Go back one screen |
| `↑` / `k` | Move selection up |
| `↓` / `j` | Move selection down |
| `Enter` | Open selected item |
| `/` | Toggle package search filter |
| `p` | Open publish wizard (registry list) |
| `L` | Open the login screen (registry list) |
| `?` | Toggle help overlay |
| `Tab` / `Shift-Tab` | Cycle fields in publish wizard |

On the version detail screen, the keys act on the selected version, through
the same requests as [`package <verb>`](#commands-version) and `why`:

| Key | Action |
|-----|--------|
| `w` | **Why** pane: [`why`](#commands-security)'s answer for the version, then your own recent refusals of this package |
| `o` | The package's owners |
| `y` / `d` | Yank / delete: a modal names the version and asks first; any key but `y` cancels |
| `u` | Unyank |
| `p` / `P` | Pin / unpin against retention |
| `Esc` | Close the side pane, then go back |

The login screen writes the `-P` profile, as `auth login` does, and the TUI
uses the new credential at once. The publish wizard shows the name and version
it read from the file once you press `Enter` on the path, and runs `publish`
itself: the TUI steps aside while it prints, and comes back on `Enter`.

## 17. Commands — why, wait and check {#commands-security}

The two verbs of a supply-chain quarantine ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)). A registry behind `[registries.security]` refuses a version it has not judged yet, or has judged against; the refusal names the coordinate, the state, the reason codes and this command.

### `why <registry>/<name>@<version>`

Why a version is pullable, not pullable yet, or not pullable ([RFC 0040 §4.8](/rfc/0040-cli-grammar-and-coverage#_4-8-why-a-version-is-not-pullable)). It prints one headline, then every cause the download checks, in the order the server checks them (access, block, lifecycle, policy rules, the verdict, upstream). Every cause is reported, not only the first one that fails. The command asks `GET /api/v1/packages/access` and writes nothing on the server: it queues no scan and records no verdict. Any identity may ask. A package you may not read gets the same `404` as an unknown one.

```bash
batlehub-cli why npm/left-pad@1.4.0
batlehub-cli why npm/left-pad@1.4.0 --format json
batlehub-cli why npm/left-pad@1.4.0 --rescan     # queue a rescan too (needs gates:exempt)
batlehub-cli -r npm why left-pad@1.4.0           # a bare name, in the default registry
```

```text
⏳ npm/left-pad@1.4.0 — pullable from 2026-11-04 14:00 UTC

  ✅ pass  access      you may read npm
  ✅ pass  block       not blocked
  ⏳ wait  verdict     MIN_AGE_NOT_MET: quarantined — MIN_AGE_NOT_MET (until 2026-11-04 14:00 UTC)
                      https://batlehub.dev/use/refused-downloads#min-age-not-met
  ✅ pass  upstream    metadata resolved

next: `batlehub-cli wait npm/left-pad@1.4.0` returns when it becomes pullable
```

The headline is **✅ pullable now**, **⏳ pullable from \<time\>** (or **when a scanner answers**), or **⛔ not pullable**. Each failing cause carries its reason code, the code's section of [Why a download was refused](/use/refused-downloads), and a `next:` step. A token with `findings:read` also sees the scanners' findings under a held verdict. The exit code is **0** for ✅ and ⏳, since a hold that lifts on its own is still an answer, **4** for ⛔ and **5** for not found. Use `wait` when you want the exit code to wait.

### `why <registry>/<name>`

With no version, `why` explains **resolution**: which version a client asking for the newest one receives, and why that is not the newest version. Below that is a table of the ten newest versions and their state.

```text
npm/left-pad resolves to 1.3.9 — 1.4.0 is waiting (MIN_AGE_NOT_MET, until 2026-11-04 14:00 UTC)

VERSION  STATE        UNTIL                  CODE
1.4.0    quarantined  2026-11-04 14:00 UTC   MIN_AGE_NOT_MET
1.3.9    allowed      —                      —
1.3.8    yanked       —                      —
```

### `why --last [--limit 20]`

Your own refusals from the last 30 days, newest first: the coordinate, the time and the reason code. Use it when npm printed a `403` and you do not know which exact version it asked for. Each row is a coordinate you can pass back to `why`. It reads `GET /api/v1/me/downloads?outcome=denied`, which returns only the caller's rows. A refusal recorded before the upgrade shows `—` as its code.

### `wait <registry>/<name>@<version> [--timeout 1h] [--interval 30s]`

The CI contract. Polls the verdict and exits **0** when the version becomes servable, **4 when waiting cannot help** — a `denied` verdict, or a hold with no clock such as `TIMESTAMP_MISSING` — and **8** on timeout ([exit codes](#exit-codes)). Exit 4 comes back on the first poll with the reason, never after burning the timeout, so a pipeline fails fast on a decision and waits only on a clock. When the hold names an `available_at` the wait sleeps to it rather than polling.

```bash
batlehub-cli wait npm/left-pad@1.3.1 --timeout 2h && npm ci
```

A version this instance has never been asked for has no verdict to wait on: request the artifact once (the first request is what creates the hold and queues the scan), then wait.

### `check [--lock <file>…] [--strict]` {#check}

Answers whether the pinned dependencies of this project would install. Without
`--lock`, `check` reads every lockfile it knows in the current directory: the
**Check** column of [the coverage matrix](#coverage) lists them. Each pin is
asked about against the registry of its kind (`-r` when it is that kind,
otherwise the first one you can install from, a proxy before a local one). The
dependencies that are not ✅ are listed with their reason and the next step:

```
$ batlehub-cli check
+----+-----------------------------+-------------------+------------------------------------+-----------------------------------------+
|    | DEPENDENCY                  | LOCKFILE          | WHY                                | NEXT                                    |
+----+-----------------------------+-------------------+------------------------------------+-----------------------------------------+
| ⏳ | npm/left-pad@1.4.0          | package-lock.json | MIN_AGE_NOT_MET: released 2 days … | batlehub-cli wait npm/left-pad@1.4.0    |
+----+-----------------------------+-------------------+------------------------------------+-----------------------------------------+
212 dependencies: 211 pullable, 1 waiting, 0 refused
```

It exits **4** as soon as one dependency is ⛔, or one this instance does not
have or that you may not see, and with `--strict` on a ⏳ too. Otherwise it
exits 0. Run it in CI right before the install, so the job fails naming a
dependency rather than with a `403` in the package manager's log:

```bash
batlehub-cli check --strict && npm ci
```

`check` asks `POST /api/v1/packages/access/batch` 50 pins at a time. The server
answers from the metadata it already holds and never fetches upstream for a
`check`. A version it has not resolved yet is ⏳ "not yet resolved", and
`batlehub-cli why <coordinate>` resolves it. Each pin counts against your rate
limit like a download. A lockfile over 16 MiB is refused with exit 2 before
anything is sent.

---

## 18. Commands — download {#commands-download}

Fetch one file through the proxy, which caches it on the way past. This is how a
path-addressed registry (`deb`, `rpm`, `pacman`, `apk`, `jetbrains`, `generic`) is
warmed one file at a time, and how you check that a path the proxy is meant to
serve actually resolves.

```
batlehub-cli download <target> [-o <file>]
```

`<target>` is a coordinate or a path:

```sh
# a coordinate: the version's one artifact, checked against its metadata
batlehub-cli download npm-local/left-pad@1.3.0
batlehub-cli -r crates download serde@1.0.200

# a full URL
batlehub-cli download https://hub.example.dev/proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz

# a server path, against the configured server
batlehub-cli download /proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz

# registry-relative, which needs -r
batlehub-cli download -r jb jetbrains/idea/idea-2026.1.3.tar.gz
```

**By coordinate**, the kind must have one artifact per version: npm, cargo,
composer, rubygems, goproxy, nuget, openvsx, vscode-marketplace,
jetbrains-marketplace and galaxy. Any other kind exits 2 and says why (a PyPI
version is several files, a Maven version a jar and a pom); give its `/proxy/…`
path instead. The file is named from the coordinate (`left-pad-1.3.0.tgz`),
never from anything the server sends.

The bytes go to `<file>.part` and are hashed while they stream, against the
checksum the kind's own metadata advertises (npm's `dist.integrity`, cargo's
index `cksum`). Only a match is renamed into place; a mismatch, a refusal or a
Ctrl-C removes the `.part` and exits non-zero. A kind whose metadata carries no
checksum the CLI reads is written with a `warning:` that says it is unverified.

When the server refuses a coordinate (`403`, or `404`), `download` prints what
[`why`](#commands-security) prints for it on stderr, then exits 4 or 5.

`-o` names the output file, and an existing one is replaced only after a
[confirmation](#confirmation). `-o -` writes to stdout so the file can be piped;
there, a mismatch is reported after the last byte and the exit code (1) is the
signal.

---

## 19. Commands — authz {#commands-authz}

Ask the server what a subject may do, and read what shadow mode has let through
([RFC 0015](/rfc/0015-grants-on-the-resource-hierarchy) §4.8). Both answer from
the running configuration, not from a file you hand them.

```
batlehub-cli authz explain --subject <s> --action <verb> <registry> [--package <p>] [--version <v>]
batlehub-cli authz shadow [--limit <n>] [--detail]
```

`--subject` takes the grant spelling: `*`, `role:user`, `user:alice`,
`group:oidc1:eng`, or `group:*:eng` for that group under any provider. The reply
names **which tier granted each verb**, which is the difference between this and
reading the config yourself.

```sh
batlehub-cli authz explain --subject group:oidc1:eng --action releases:publish internal-npm
```

`--package` matters: the namespace and package tiers only match when a package is
given, so an `explain` without one is answering a narrower question than it looks.
`--version` reaches the version tier.

`authz shadow` reports what shadow mode served that enforcement would have
refused — the list to empty before turning enforcement on. It summarises per node
by default; `--detail` prints every entry.

---

## 20. Commands — verdict {#commands-verdicts}

The administrator's side of a supply-chain verdict
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)). Where `why` answers
*why is my install held*, these answer *what is held, and who already pulled it*.

```
batlehub-cli verdict pullers  <registry>/<name>@<version> [--since <window>] [--format csv]
batlehub-cli verdict list     -r <r> [--state <s>] [--limit <n>]
batlehub-cli verdict backfill -r <r>
batlehub-cli verdict rescan   -r <r> [--state <s>]
```

`pullers` is the incident question: who pulled this version inside a window,
from the same access-log query the flip alert carries. `--since` takes `30d`,
`12h`, `90m` or an RFC 3339 instant, and defaults to the registry's
`pullers_window_days`. `--format csv` prints the CSV the export endpoint renders, for
handing to someone who does not have the CLI.

```sh
batlehub-cli verdict pullers npm/left-pad@1.3.1 --since 30d --format csv
```

`list` shows a registry's verdicts by state — `allowed`, `warned`,
`quarantined`, `denied`, or every state when `--state` is omitted. `--limit` is
per state, defaults to 100 and caps at 1000.

`backfill` queues a low-priority scan of every cached version of a registry, for
the versions that were already there when scanning was turned on. `rescan`
requeues verdicts that already exist, optionally only one state — the two are
different jobs and the distinction matters on a large registry.

---

## 21. Commands — completion {#commands-completion}

Print a shell completion script to stdout.

```
batlehub-cli completion <bash|elvish|fish|powershell|zsh>
```

It writes nothing itself, so where the script goes is yours to decide:

```sh
# bash, for this user
batlehub-cli completion bash > ~/.local/share/bash-completion/completions/batlehub-cli

# zsh, on a directory already in $fpath
batlehub-cli completion zsh > ~/.zfunc/_batlehub-cli

# fish
batlehub-cli completion fish > ~/.config/fish/completions/batlehub-cli.fish
```

---

## 22. Commands — audit {#commands-audit}

What one identity pulled ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)
§4.2). The transpose of [`verdict pullers`](#commands-verdicts): that one pins a
version and asks who took it, this one pins an identity and asks what it took.

```
batlehub-cli audit pulls --identity <id> [-r <r>] [--package <p>] [--since <window>] [--format csv]
```

`--identity` is a user id, or `ip:<addr>` for an anonymous caller — often the
only handle an incident has on a runner that presents no credential. `--since`
takes `30d`, `12h`, `90m` or an RFC 3339 instant and defaults to 30 days.
`--format csv` prints the CSV the endpoint renders, byte for byte, for handing to
someone who does not have the CLI.

```
$ batlehub-cli audit pulls --identity ci-bot --since 7d
ci-bot pulled 2 coordinates since 2026-09-01T00:00:00Z
registry       package                      version         pulls  last
npm-local      demo-pkg                     1.0.0               3  2026-09-08T23:20:33Z
crates         serde                        1.0.219            11  2026-09-08T21:04:11Z
```

Both this and `verdict pullers` need `audit:read`, and both count **delivered
bytes only**: a refusal transferred nothing and is a different question. Rows are
grouped by coordinate, newest pull first.

::: tip
The `client_user_agent` and `source_ip` columns are filled on every delivered
download, whether it was proxied or served from a local or hybrid registry's own
storage. Rows recorded before that second half landed have neither, and nothing
backfills them — so a blank pair beside a non-zero `count` dates the row rather
than describing the caller.
:::


## 23. Commands — java {#commands-java}

The Java engine of the BatleHub VS Code extensions: the inspections, fixes,
generators and renames of their Java support, over a real Java language
server and with no editor — for a CI gate and for agents. Everything after
`java` is the engine's own, and so is the exit code:

```
$ batlehub-cli java inspect . --fail-on warning --format sarif > java.sarif
$ batlehub-cli java fix src/main/java/com/acme/Greeter.java --write
$ batlehub-cli java mcp          # the same verbs as MCP tools over stdio
$ batlehub-cli java --help       # every verb, flag and exit code
```

It needs a `node` ≥ 22 on `PATH` (`mise use node@24`) and a JDK ≥ 21 its
discovery can find. The engine is built into this binary and written under
the cache directory (`~/.cache/batlehub/java-engine/<hash>/`) on first use;
its first run also downloads the pinned Java language server once.
`BATLEHUB_JAVA_ENGINE=<path to batlehub-java.mjs>` runs another build. No
server, token or profile is read: `--profile` and `--json` after `java` are
the engine's flags, not the CLI's.

The verbs, the configuration they read (only the committed
`.batlehub/java/` files) and the CI snippet are documented with the engine,
in [the extensions' guide](https://github.com/batlehub/batlehub-vsx/blob/main/docs/guide/java/engine.md).
