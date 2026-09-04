# batlehub-cli

`batlehub-cli` is the official command-line client for BatleHub. It provides both a traditional CLI for scripting and CI pipelines, and an interactive TUI for everyday browsing and management.

## 1. Installation

**via mise** (recommended — manages version automatically):

```bash
mise use "github:batleforc/batlehub[asset_pattern=batlehub-cli-*]"
```

**via cargo** (builds from source — requires Rust toolchain):

```bash
cargo install --git https://github.com/batleforc/batlehub batlehub-cli
```

**Pre-built binaries** — download from [GitHub Releases](https://github.com/batleforc/batlehub/releases/latest):

```bash
# Linux x86_64
curl -fSL https://github.com/batleforc/batlehub/releases/latest/download/batlehub-cli-linux-amd64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# Linux aarch64
curl -fSL https://github.com/batleforc/batlehub/releases/latest/download/batlehub-cli-linux-arm64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# macOS Apple Silicon (M1/M2/M3)
curl -fSL https://github.com/batleforc/batlehub/releases/latest/download/batlehub-cli-darwin-arm64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# macOS Intel
curl -fSL https://github.com/batleforc/batlehub/releases/latest/download/batlehub-cli-darwin-amd64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# Windows (PowerShell)
Invoke-WebRequest https://github.com/batleforc/batlehub/releases/latest/download/batlehub-cli-windows-amd64.zip -OutFile batlehub-cli.zip
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
batlehub-cli config init
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

---

## 3. Global flags

These flags are available on every command:

| Flag | Short | Description |
|------|-------|-------------|
| `--profile <name>` | `-P` | Use a named config profile |
| `--server <url>` | | Override the server URL |
| `--token <tok>` | | Override the auth token |
| `--registry <name>` | `-r` | Set a default registry |
| `--json` | | Emit machine-readable JSON instead of tables |

---

## 4. Commands — registry

```
batlehub-cli registry list
batlehub-cli registry info <name>
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

### `registry info <name>`

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
  `dl.google.com`, `get.helm.sh`, `dl.min.io`, `binaries.sonarsource.com`), the
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
batlehub-cli package list   [--registry <r>] [--search <q>] [--blocked-only] [--page N] [--per-page N]
batlehub-cli package versions <registry> <name>
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

Filter in scripts with `jq`:
```bash
# List only blocked packages
batlehub-cli --json package list | jq '[.[] | select(.status.status == "blocked")]'
```

### `package versions <registry> <name>`

List all cached versions of a package with their status and download count.

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

## 6. Commands — version {#commands-version}

```
batlehub-cli version yank   <registry> <name> <version>
batlehub-cli version unyank <registry> <name> <version>
batlehub-cli version delete <registry> <name> <version> [--yes]
batlehub-cli version pin    <registry> <name> <version>
batlehub-cli version unpin  <registry> <name> <version>
```

These commands require an admin token.

| Command | Effect |
|---------|--------|
| `yank` | Marks a version unavailable (kept in storage, download blocked) |
| `unyank` | Reverses a yank |
| `delete` | Drops the artifact **and spends the version number permanently** |
| `pin` | Exempts a version from retention — it is never reclaimed automatically |
| `unpin` | Releases the pin, so the registry's retention policy applies again |

> **Package name casing**: package names are normalized to lowercase when published (NuGet lowercases the package ID, cargo and npm use lowercase by convention). Use the lowercase form with `version yank/unyank/delete` to match the stored name — e.g. `serilog`, not `Serilog`.

`delete` prompts for confirmation unless `--yes` is passed:

```
$ batlehub-cli version delete internal serilog 2.0.0
Delete internal/serilog@2.0.0? The artifact is dropped and the version number is
spent permanently — 2.0.0 can never be published again. [y/N] y
Deleted internal/serilog@2.0.0
```

A deleted version number is never reused. Publishing `2.0.0` again is refused
with `409`, whoever asks and however long afterwards, so "delete and re-upload to
fix it" is not a plan — publish `2.0.1`, or `yank` instead if you only need the
version to stop being installed. The reasoning, and what the deletion leaves
behind for an auditor, are in
[Deleting a published version](/guide/admin-policies#deleting-versions).

---

## 7. Commands — owners

```
batlehub-cli owners list   <registry> <name>
batlehub-cli owners add    <registry> <name> <principal> [--type user|group] [--role admin|maintainer]
batlehub-cli owners remove <registry> <name> <principal> [--type user|group]
```

Ownership controls who can publish new versions to a local/hybrid registry. Requires an admin token.

```
$ batlehub-cli owners list internal Serilog
+------+------------------+------------+------------+
| Type | Principal        | Role       | Granted By |
+------+------------------+------------+------------+
| user | alice@example.com| admin      | -          |
| group| nuget-maintainers| maintainer | alice      |
+------+------------------+------------+------------+

$ batlehub-cli owners add internal Serilog bob --type user --role maintainer
Added user 'bob' as maintainer on internal/Serilog
```

---

## 8. Commands — publish

```
batlehub-cli publish <file> [--registry <r>] [--name <n>] [--version <v>] [--type <t>]
                             [--distribution <d>] [--component <c>] [--platform <p>]
```

Upload an artifact to a local or hybrid registry. The CLI auto-detects the registry type and package metadata from the file:

| Extension | Registry type | Metadata source |
|-----------|---------------|-----------------|
| `.nupkg` | nuget | embedded `.nuspec` |
| `.whl` | pypi | filename (`name-version-*.whl`) |
| `.gem` | rubygems | filename (`name-version.gem`) |
| `.pkg.tar.{zst,xz,gz}` | pacman | filename (`name-pkgver-pkgrel-arch.pkg.tar.*`) |
| `.tgz` | npm | filename (`name-version.tgz`, as produced by `npm pack`) |
| `.crate` | cargo | filename (`name-version.crate`, as produced by `cargo package`) |
| `.vsix` | openvsx | filename (`extension_id-version.vsix`) |
| `.deb` | deb | server-side, from the package's control file — requires `--distribution` and `--component` |
| `.rpm` | rpm | server-side, from the package's header |
| `.tar.bz2` / `.conda` | conda | server-side, from the package's own `info/index.json` (`--platform` is only a fallback) |

Composer ZIPs share the generic `.zip` extension with other formats and are not auto-detected — pass `--type composer` explicitly. Composer, like conda/deb/rpm, parses name/version server-side from `composer.json`, so no `--name`/`--version` is needed (an optional `--version` overrides the archive's own version).

Use `--type` to override auto-detection entirely — useful for ambiguous extensions or when a file doesn't follow the expected naming convention.

Maven (separate jar+pom+checksum files), Terraform (providers need shasums/signature files; modules need a packaging step), and Go modules (need an `.info`/`.mod`/`.zip` triad) don't fit this command's single-file model by design. Use your existing tooling (`mvn deploy`, Terraform registry publishing conventions, `go mod`) configured to point at the BatleHub endpoint — see [`docs/use/publishing.md`](publishing.md) for per-registry setup instructions.

```bash
# NuGet
batlehub-cli publish Serilog.3.1.1.nupkg --registry internal

# Override detected metadata
batlehub-cli publish dist/mylib-1.2.3.tar.gz --type pypi --name mylib --version 1.2.3

# Composer (ambiguous .zip extension — type must be explicit)
batlehub-cli publish acme-widget.zip --type composer --registry internal

# Debian (distribution/component aren't in the filename)
batlehub-cli publish hello_1.0-1_amd64.deb --registry internal --distribution stable --component main

# Conda (platform is only a fallback for packages with no embedded subdir)
batlehub-cli publish numpy-1.26.0-py311h0.conda --registry internal --platform linux-64
```

---

## 9. Commands — auth

```
batlehub-cli auth whoami
batlehub-cli auth token                      # print a credential (refreshing it first)
batlehub-cli auth token [--output raw|json] [--min-ttl <seconds>]
batlehub-cli auth token list
batlehub-cli auth token create --name <n> [--days <d>] [--role user|admin]
                               [--groups <g1,g2> | --all-groups]
batlehub-cli auth token revoke <uuid>
batlehub-cli auth write-token-file [--path <p>] [--from-file <p>]
batlehub-cli auth status [--path <p>] [--json]
```

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

## 10. Commands — admin

These commands require an admin token.

### Quota

```
batlehub-cli admin quota list   [--registry <r>]
batlehub-cli admin quota reset  <registry> <user>
```

### IP blocks

```
batlehub-cli admin ip-block list
batlehub-cli admin ip-block add    <ip> [--reason <text>]
batlehub-cli admin ip-block remove <ip>
```

### Config

```
batlehub-cli admin config reload    # trigger hot reload on the server
batlehub-cli admin config changes   # view change history
```

### Cache

```
batlehub-cli admin cache warm  <registry> [--packages pkg1,pkg2]
batlehub-cli admin cache clear <registry>
```

### Banner

```
batlehub-cli admin banner set   "Maintenance at 22:00 UTC" [--level info|warning|error]
batlehub-cli admin banner clear
```

### Grants

The package and version tiers of the authorization hierarchy — the two a config
file cannot enumerate. Registry- and namespace-tier grants stay in `config.toml`.

```
batlehub-cli admin grants list <registry> <name>[@<version>]
batlehub-cli admin grants set  <registry> <name>[@<version>] --subject <s> --actions <a1,a2>
batlehub-cli admin grants rm   <registry> <name>[@<version>] --subject <s>
```

```
$ batlehub-cli admin grants set npm1 @acme/billing \
      --subject group:oidc1:eng --actions releases:read,releases:list
Granted releases:read, releases:list on npm1/@acme/billing to group:oidc1:eng

$ batlehub-cli admin grants list npm1 @acme/billing
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

- **`name@version` addresses one version**, split on the *last* `@` — so
  `@acme/billing` is a package and `@acme/billing@2.4.0` is a version.
- **What `set` prints is what was stored.** `--actions releases:*` names one verb
  and stores several; the output is the expanded set. It also prints warnings for
  a grant that is legal but inert — one a broader tier already gives, or one on a
  yanked version.
- **`Source: ownership` rows are not editable here.** They follow the package's
  owner list; change them with `admin owner`. Editing one earns a `409`.
- Needs `grants:write` (`grants:read` for `list`), which `role:admin` holds.

### Audit log

```
batlehub-cli admin audit-log [--registry <r>] [--user <id>] [--from <date>] [--to <date>] [--denied-only]
```

### Retention

```
batlehub-cli admin retention <registry> [--show-kept] [--reclaim]
```

Reclaims locally published versions the registry's `[registries.retention]`
policy no longer keeps. **Reports by default** — `--reclaim` is only half the
interlock, and the registry also needs `dry_run = false`. Two decisions in two
places, because a reclaimed artifact may exist nowhere else.

`--show-kept` prints every surviving version and the condition that saved it,
which is how you check a policy against what it actually does before arming it.

Pinning a single version against retention is
[`version pin`](#commands-version).

### Air gap

```
batlehub-cli admin air-gap-missing [--registry <r>] [--kind artifact|document|checksum|ref|unmirrored_host]
batlehub-cli admin bundles
```

What a disconnected instance was asked for and did not hold, and what came
across the gap. See [the air-gap runbook](/operations/air-gap).

---

## 11. Commands — mise {#commands-mise}

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

## 12. Commands — config

```
batlehub-cli config init           # interactive first-run wizard
batlehub-cli config show           # print resolved config (token is masked)
batlehub-cli config set server_url https://batlehub.example.com
batlehub-cli config set token      my-token [--profile prod]
batlehub-cli config set registry   internal [--profile prod]
```

Valid keys for `config set`: `server_url`, `token`, `registry`.

---

## 13. Commands — setup

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

---

## 14. TUI mode

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
| `y` | Yank selected version (version detail screen) |
| `u` | Unyank selected version |
| `p` | Open publish wizard |
| `?` | Toggle help overlay |
| `Tab` / `Shift-Tab` | Cycle fields in publish wizard |

## 15. Commands — why and wait {#commands-security}

The two verbs of a supply-chain quarantine ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)). A registry behind `[registries.security]` refuses a version it has not judged yet, or has judged against; the refusal names the coordinate, the state, the reason codes and this command.

### `why <registry>:<name>@<version>`

The verdict behind a refusal: state, reason codes, when a hold lifts, which scanners answered, and — for a token with `findings:read` — the findings themselves. Needs `quarantine:read` on the registry (`user` and `admin` hold it by default; `anonymous` does not, so a public mirror answers a plain 404 unless the operator grants it).

```bash
batlehub why npm:left-pad@1.3.1
batlehub why npm:left-pad@1.3.1 --json
batlehub why npm:left-pad@1.3.1 --rescan     # queue a rescan too (needs gates:exempt)
```

```text
npm:left-pad@1.3.1
  state        quarantined
  reasons      MIN_AGE_NOT_MET
  available    2026-09-05T14:12:00Z
  policy       npm/default
  evaluated    2026-09-04T14:12:03Z
  scanned      2026-09-04T14:12:01Z
  scanners     osv
  findings     none

Held until 2026-09-05T14:12:00Z. `batlehub wait` waits for it.
```

### `wait <registry>:<name>@<version> [--timeout 1h] [--interval 30s]`

The CI contract. Polls the verdict and exits **0** when the version becomes servable, **1 when waiting cannot help** — a `denied` verdict, or a hold with no clock such as `TIMESTAMP_MISSING` — and **2** on timeout. Exit 1 comes back on the first poll with the reason, never after burning the timeout, so a pipeline fails fast on a decision and waits only on a clock. When the hold names an `available_at` the wait sleeps to it rather than polling.

```bash
batlehub wait npm:left-pad@1.3.1 --timeout 2h && npm ci
```

A version this instance has never been asked for has no verdict to wait on: request the artifact once (the first request is what creates the hold and queues the scan), then wait.

