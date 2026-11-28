# Publishing Packages to BatleHub

This guide walks through publishing packages to a BatleHub private registry for each supported registry type. Publishing requires the registry to be running in `local` or `hybrid` mode and a token with sufficient permissions.

## 1. Prerequisites

Publishing is only available when the registry is configured with `mode = "local"` or `mode = "hybrid"`. In `proxy` mode (the default), all write requests are rejected.

| Mode | Behaviour |
|------|-----------|
| `local` | BatleHub is the only source. No upstream needed. |
| `hybrid` | Local packages take priority; unknown packages fall back to upstream. |

See [`docs/guide/configuration.md` § Registry modes](/guide/configuration#registry-modes) for the full configuration reference.

---

## 2. Getting an API token

All publish requests require a `Bearer` token in the `Authorization` header.

### Static tokens (config.toml)

The simplest option for CI pipelines or single-user setups:

```toml
[[auth]]
type = "token"

[[auth.tokens]]
value   = "my-publish-token"
role    = "admin"
user_id = "ci"
```

### User-generated API tokens (OIDC sessions)

If you use OIDC login, you can generate short-lived tokens from the Web UI (Settings → Tokens) or via the API:

```sh
curl -s -X POST \
  -H "Authorization: Bearer <oidc-session-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "ci-publish", "expires_in_days": 30, "role": "user"}' \
  https://batlehub.example.com/api/v1/auth/tokens
```

The response contains the raw token value — save it, it is shown only once.

```json
{
  "id": "...",
  "name": "ci-publish",
  "token": "bh_xxxxxxxxxxxxxxxxxxxx",
  "expires_at": "2026-06-21T00:00:00Z"
}
```

---

## 3. Your ecosystem's instructions

Publishing is per-ecosystem, and each ecosystem has one page. That page is the
home for its server configuration, its client setup, the publish command, how to
verify it worked, and its endpoint reference — this page is only the part that is
the same whatever you are publishing.

| Category | Registry | Publishing instructions |
| --- | --- | --- |
| Language | npm | [/registries/npm](/registries/npm#publishing-local-hybrid) |
| Language | Cargo | [/registries/cargo](/registries/cargo#publishing-local-hybrid) |
| Language | Go Modules | [/registries/goproxy](/registries/goproxy#publishing-local-hybrid) |
| Language | Maven | [/registries/maven](/registries/maven#publishing-local-hybrid) |
| Language | PyPI | [/registries/pypi](/registries/pypi#publishing-local-hybrid) |
| Language | Conda | [/registries/conda](/registries/conda#publishing-local-hybrid) |
| Language | Composer | [/registries/composer](/registries/composer#publishing-local-hybrid) |
| Language | RubyGems | [/registries/rubygems](/registries/rubygems#publishing-local-hybrid) |
| Language | NuGet | [/registries/nuget](/registries/nuget#publishing-local-hybrid) |
| Language | Terraform | [/registries/terraform](/registries/terraform#publishing-local-hybrid) |
| Editor extensions | OpenVSX | [/registries/openvsx](/registries/openvsx#publishing-local-hybrid) |
| Editor extensions | VS Code Marketplace | [/registries/vscode-marketplace](/registries/vscode-marketplace#publishing-local-hybrid) |
| Editor extensions | JetBrains Marketplace | [/registries/jetbrains-marketplace](/registries/jetbrains-marketplace#publishing-local-hybrid) |
| OS packages | Debian / APT | [/registries/deb](/registries/deb#publishing-local-hybrid) |
| OS packages | RPM / YUM / DNF | [/registries/rpm](/registries/rpm#publishing-local-hybrid) |
| OS packages | Pacman / Arch | [/registries/pacman](/registries/pacman#publishing-local-hybrid) |

Source forges (GitHub, GitLab, Forgejo), the JetBrains IDE mirror and the
Generic mirror are proxy-only — there is nothing to publish to them.

---

## 4. Troubleshooting

### `403 Forbidden` on publish

- The token is missing, expired, or does not have the required role. Publish is restricted to `admin` role by default. Check the `[registries.rbac]` block — the role that should publish needs `"*"` (or at minimum write access).
- Pass the token explicitly: `-H "Authorization: Bearer <token>"`.

### `403 Forbidden` — "registry is not in local or hybrid mode"

The registry `mode` is set to `proxy` (the default). Change it to `"local"` or `"hybrid"` in `config.toml` and restart the server.

### `409 Conflict`

The version already exists in the registry. Bump the version in your package manifest and republish.

### `400 Bad Request` (Go)

The module zip structure is invalid. Every entry inside the zip must be prefixed with `{module}@{version}/`. Rebuild with `go mod zip` to get the correct layout.

### `400 Bad Request` (Cargo)

Cargo uses a binary wire format (length-prefixed metadata JSON followed by the `.crate` bytes). Only `cargo publish` produces this format — do not attempt to hand-craft the request.

### Token accepted but `cargo publish` fails with "invalid token"

Cargo expects the sparse index `config.json` to match the token endpoint. Verify the `index` URL in `.cargo/config.toml` ends with `/registry/`:

```
sparse+https://batlehub.example.com/proxy/internal/registry/
```

### `400 Bad Request` (Maven) — "POM missing groupId"

The uploaded `.pom` file is missing `<groupId>` or `<artifactId>`. These are required fields. Check that your `pom.xml` or Gradle `build.gradle.kts` sets `group` and `archivesName`/`rootProject.name` before publishing.

### `mvn deploy` succeeds but `maven-metadata.xml` is not updated

BatleHub generates `maven-metadata.xml` dynamically from the database. A successful `.pom` upload (HTTP 201) means the version was recorded. If GET returns 404, the `.pom` upload may have failed — check the response status for each uploaded file in verbose output (`mvn deploy -X`).

### Published, but every read answers `403` — "package allowlist"

The registry has a [`package_allowlist`](/guide/security-gates#package-allowlist)
and your package is not on it. Publishing is governed by your grants, so the
upload succeeds; reading is governed by the allowlist, so every read is
refused. Ask your administrator to list the package, usually by its scope
(`"@acme/*"`).

### Terraform `terraform init` fails — "registry does not have a provider"

Verify the `source` address in `terraform required_providers` matches the registry hostname and path exactly:
```
batlehub.example.com/proxy/{registry}/namespace/type
```
Ensure credentials for `batlehub.example.com` are set in `~/.terraformrc`.

### Terraform provider download fails — "no matching binary"

The provider manifest was uploaded without a binary for the requested platform. Upload the binary via:
```
PUT /proxy/{registry}/v1/providers/{ns}/{type}/{version}/artifact/{os}/{arch}
```
