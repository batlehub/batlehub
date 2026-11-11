# Using BatleHub

**For the person whose package manager talks to BatleHub.** Setting up your
local environment to pull through it, and publishing private packages when your
administrator has enabled `local` or `hybrid` mode.

<SectionGrid section="use" />

If you are the one *running* the server — installing it, configuring registries,
granting access — that is the [operator's guide](/guide/installation).

- **[Your ecosystem's setup page](/registries/)** — the snippet for npm, Cargo,
  Maven, PyPI and eighteen others. Start here if you just need it to work.
- **[Publishing](/use/publishing)** — prerequisites, tokens, and where each
  ecosystem's publish instructions live.
- **[Command-line client](/use/cli)** — `batlehub-cli`, including the TUI.
- **[Troubleshooting](/use/troubleshooting)** — when it does not work.

---

## Getting a token {#getting-a-token}

Most BatleHub endpoints require a Bearer token. Ask your administrator for a token or, if OIDC login is enabled, generate one yourself:

**Via the Web UI:** log in at `https://batlehub.example.com`, open Settings → Tokens, and click "New token".

**Via the API:**

```sh
# Exchange your OIDC session token for a long-lived API token
curl -X POST \
  -H "Authorization: Bearer <oidc-session-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-laptop", "expires_in_days": 90, "role": "user"}' \
  https://batlehub.example.com/api/v1/auth/tokens
```

The raw token value is shown **once** — save it to a password manager or environment variable.

```sh
export BATLEHUB_TOKEN=bh_xxxxxxxxxxxxxxxxxxxx
```

### Authenticating from GitHub / Forgejo Actions {#ci-actions-oidc}

If your administrator has configured an `actions-oidc` auth provider, GitHub and Forgejo workflow jobs can authenticate **without any long-lived secret**. The workflow requests a short-lived OIDC token from the runner and passes it directly as a Bearer token.

Enable OIDC token minting in your workflow:

```yaml
jobs:
  publish:
    permissions:
      id-token: write   # required — lets the runner mint an OIDC token
      contents: read
```

Then exchange the token at the start of any step that calls BatleHub:

```sh
# In a GitHub Actions "run:" step:
BATLEHUB_TOKEN=$(curl -s -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=batlehub" | jq -r '.value')

# Use it exactly like any other Bearer token
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  https://batlehub.example.com/api/v1/...
```

The token is valid for the duration of the job. It carries claims like `repository`, `ref`, `environment`, and `actor`, which the `actions-oidc` provider uses to assign you to one or more groups — for example `"github-actions/myorg-my-repo/main"` — so you automatically receive the right RBAC permissions without any manual user management.

Ask your administrator which groups are mapped and what permissions they carry.

---

### Creating tokens from the API {#tokens-api}

Users authenticated via OIDC can create personal long-lived API tokens without going through SSO each time. This is the recommended approach for CI/CD pipelines when Kubernetes service account auth is not available.

```sh
# Create a token (valid for 30 days, cannot exceed creator's role)
curl -X POST https://batlehub.example.com/api/v1/auth/tokens \
  -H "Authorization: Bearer <oidc-access-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "ci-token", "expires_in_days": 30}'

# List active tokens
curl https://batlehub.example.com/api/v1/auth/tokens \
  -H "Authorization: Bearer <oidc-access-token>"

# Revoke a token
curl -X DELETE https://batlehub.example.com/api/v1/auth/tokens/<token-id> \
  -H "Authorization: Bearer <oidc-access-token>"
```

Key properties:
- Token values are shown **once** at creation time; store them securely.
- A token's role cannot exceed the role of the user who created it.
- Token auth (`type = "token"`) in the config file and user-generated tokens are two separate mechanisms; user-generated tokens are always available to OIDC-authenticated users with no extra `[[auth]]` entry needed.

---

## Setup Guide UI

The built-in **Setup Guide** at `https://batlehub.example.com/setup` generates ready-to-paste config snippets for every registered tool. The snippets are pre-filled with your server's address and available registries — use them as a starting point for the manual steps below.

---

## Per-registry setup {#registries}

Every registry type has a dedicated page in the [Registries reference](/registries/),
covering its proxy setup, publishing (in `local`/`hybrid` mode), and authentication.
Pick your ecosystem there — source hosting, the language registries, editor
extensions, OS packages, and the binary mirrors are grouped on that one page.

The list itself lives there and only there. A second copy on this page is a second
copy to keep in step, and the one that fell behind would be the one a reader found
first.

---

## Security auditing {#security-audit}

Several ecosystems can run their vulnerability audit through BatleHub — the proxy forwards the request to the upstream advisory database, so no direct internet access is required.

### npm audit {#audit-npm}

`npm audit` works automatically once the registry is configured — both quick and bulk audit modes are proxied through BatleHub to the upstream advisory database.

```sh
npm audit
npm audit --fix
```

### Composer audit {#audit-composer}

`composer audit` works automatically once the repository is configured — BatleHub proxies the Packagist security advisory API transparently.

```sh
composer audit
```

### Go — govulncheck {#audit-go}

BatleHub proxies the [Go Vulnerability Database](https://vuln.go.dev) so `govulncheck` works without direct access to vuln.go.dev. Set `GOVULNDB` to the same base URL as `GOPROXY`:

```sh
export GOVULNDB="https://batlehub.example.com/proxy/go"
govulncheck ./...
```

With authentication (put the token in `~/.netrc`):

```sh
echo "machine batlehub.example.com login user password $BATLEHUB_TOKEN" >> ~/.netrc
chmod 600 ~/.netrc
```

`machine` is matched by hostname, so it must be the host in `GOVULNDB` above. On a
deployment with [host-based routing](../rfc/0001-subdomain-routing.md) that is the
registry's own subdomain (`go.batlehub.example.com`), not the main host — one
`machine` line per host you fetch from. The Setup Guide's **.netrc** tab lists
them all, already filled in.

The govulndb URL can be changed per-registry with `vuln_db_url` in the server config (default: `https://vuln.go.dev`). Setting it to `""` disables the endpoints.

### .NET — vulnerable packages {#audit-dotnet}

`dotnet list package --vulnerable` works automatically — BatleHub exposes a `VulnerabilitiesUrl` resource in the v3 service index and proxies the vulnerability catalogue from the upstream NuGet gallery.

```sh
dotnet list package --vulnerable
dotnet list package --vulnerable --include-transitive
```

---

## Team Namespace dashboard {#team-namespace}

If your administrator has assigned namespace claims to your group, the **Team Namespace** page at `/my-namespace` gives you a single place to view your ownership, browse published packages, manage visibility, and upload new packages without needing CLI access.

### Your groups {#ns-groups}

The top card lists every auth-provider group you belong to. These are the values your administrator uses when creating namespace claims. Spaces are stripped from group names because package prefixes cannot contain spaces — `"oidc:my team"` is shown and matched as `"oidc:myteam"`.

### Your namespaces {#ns-namespaces}

The **My namespaces** table shows every namespace prefix claimed for your groups, across all registries. Each row shows:

| Column | Description |
|--------|-------------|
| Registry | The registry this claim applies to |
| Prefix | Package name prefix your group owns |
| Group | The group identifier (spaces stripped) |

Click any row to load the packages published under that namespace.

### Browsing and managing packages {#ns-packages}

After clicking a namespace row, the **Packages** card shows all published versions under that prefix. Columns include package name, version, visibility, publisher, and publication date.

**Changing visibility inline:**

Click the visibility badge on any row (or the "Edit visibility" button) to open an inline dropdown. Choose the new level and click **Save**:

| Level | Who can download |
|-------|-----------------|
| `public` | Everyone, including unauthenticated |
| `internal` | Any authenticated user |
| `team` | Members of your group only |

Results are paginated (50 per page). Use the Previous / Next buttons to navigate.

### Uploading packages {#ns-upload}

The **Upload package** card lets you publish directly from the browser for registry types that accept binary file uploads. Only registries in `local` or `hybrid` mode appear in the selector.

#### File upload (browser)

| Registry type | Accepted file | Extra fields |
|--------------|---------------|--------------|
| RubyGems | `.gem` | None — name and version are read from the gem |
| Composer | `.zip` | None — name and version are read from `composer.json` inside the archive |
| OpenVSX / VS Code Marketplace | `.vsix` | Extension ID (`publisher.name`) and version |
| Go modules | `.zip` | Module path (e.g. `github.com/org/repo`) and version (e.g. `v1.0.0`) |
| PyPI | `.whl`, `.tar.gz`, `.zip` | None — name and version are parsed from the filename |
| Conda | `.tar.bz2`, `.conda` | Platform (e.g. `linux-64`) — name, version, and build are read from `info/index.json` |

Select the registry, fill in any extra fields, choose the file, and click **Upload**.

::: tip Go module zip format
The zip must follow the standard Go module layout — every entry must be prefixed with `{module}@{version}/`. Running `go mod zip` produces this layout automatically.
:::

#### CLI (npm, Cargo, Maven, Terraform, NuGet)

For registry types without a browser-friendly binary format, the **CLI instructions** tab shows ready-to-paste commands pre-filled with your registry name. See the [Registries reference](/registries/) for each ecosystem's complete setup steps.

---

## Permissions

| Permission | What it grants |
|-----------|----------------|
| `releases:read` | List versions, download release assets and metadata |
| `source:read` | Download source archives (tarballs, `.crate`, module `.zip`) |
| `*` | All permissions (admin) |

Role inheritance: `admin` ⊃ `user` ⊃ `anonymous`. Your administrator can assign additional permissions to OIDC groups or Kubernetes service account namespaces on top of your role.

---

## Troubleshooting

**`403 Forbidden` on download:** Your token is missing or your role doesn't have `releases:read` or `source:read` for this registry. Check with your administrator.

**`403 Forbidden` on publish — "registry is not in local or hybrid mode":** Publishing is disabled on this registry. Ask your administrator to enable `mode = "local"` or `mode = "hybrid"`.

**`409 Conflict` on publish:** The version already exists. Bump the version in your package manifest.

**`cargo publish` fails with "invalid token":** Verify the `index` URL in `.cargo/config.toml` ends with `/registry/`:
```
sparse+https://batlehub.example.com/proxy/internal/registry/
```

**Go: `disabled by GOPROXY=...off`:** The proxy can't reach the upstream or the module doesn't exist there. Remove `,off` from `GOPROXY` to allow direct fallback, or check that the upstream is reachable from the BatleHub server.

**`dotnet nuget push` returns 401:** BatleHub accepts the `--api-key` value as a Bearer token (the `X-NuGet-ApiKey` header is transparently normalised to `Authorization: Bearer`). Make sure the token has `releases:publish` or admin permissions on the registry.
