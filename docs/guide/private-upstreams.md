# Private and self-hosted upstreams

Any registry can proxy a self-hosted or private upstream by combining `upstream_auth` and `tls` fields. Both are optional and independent of each other.

## Upstream authentication

Three schemes are available via `[registries.upstream_auth]`:

| `type` | Use case | Required fields |
|--------|----------|-----------------|
| `bearer` | Gitea, Forgejo, GitHub Enterprise, Artifactory API tokens | `token` |
| `basic` | Nexus, Artifactory (password), most HTTP-authenticated feeds | `username`, `password` |
| `header` | Any registry using a custom header (e.g. `X-API-Key`) | `name`, `value` |

Bearer tokens are sent as `Authorization: Bearer <token>`. Basic credentials are attached per-request as HTTP Basic auth. Custom headers are injected as default headers on every upstream request.

## Custom CA certificates

When the upstream serves a certificate signed by a private CA, add the CA certificate to the system trust store **or** point `tls.ca_cert_path` at a PEM file:

```toml
[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"
```

This setting is per-registry, so you can mix public registries (no TLS config needed) with private registries that use a corporate CA — all in the same `config.toml`.

## Using `upstream_auth` and `tls` together

Both fields can appear on the same registry block:

```toml
[[registries]]
type      = "npm"
name      = "npm-private"
upstreams = ["https://nexus.corp.example.com/repository/npm-proxy/"]

[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "my-api-key"

[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"
```

## Supported registry types

Every registry type supports `upstream_auth` and `tls`. They are read once, before the client is built, so a type added later gets them without anything here changing. For `cargo`, the sparse index proxy (the `index_url` endpoint) uses the same credentials and TLS settings.

## Mixing a private upstream with a public fallback

`upstream_auth` is per-registry block, not per-URL. When `upstreams` lists multiple URLs, the configured credentials are sent to **every** entry in that list. This causes problems when you want a private upstream as the primary source and a public registry as the unauthenticated fallback: credentials forwarded to the public registry may produce `401 Unauthorized` rather than `404 Not Found`, and the fanout only advances to the next upstream on `404` — so a `401` stops the chain immediately.

The recommended pattern is:

1. A **private registry block** pointing at the authenticated upstream, with `upstream_auth` configured and anonymous reads enabled so BatleHub can reach it without a client token.
2. A **fanout registry block** that clients actually configure, whose `upstreams` list points at BatleHub's own proxy URL for the private registry first, then the public registry second.

BatleHub handles the credentials internally when it fetches from itself, so the fanout block never needs its own `upstream_auth`.

```toml
# Step 1 — private Gitea registry with credentials.
# anonymous source:read is required so the fanout block below can reach it
# without forwarding a client token.
[[registries]]
type      = "cargo"
name      = "internal-cargo"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/cargo"]
index_url = "https://gitea.corp.example.com/api/packages/myorg/cargo/index"

[registries.upstream_auth]
type  = "bearer"
token = "npat-xxxx"

[registries.rbac]
anonymous = ["source:read"]
user      = ["source:read"]
admin     = ["*"]

# Step 2 — fanout registry: private first (via BatleHub self-proxy), public fallback.
# Clients only configure this one.
[[registries]]
type      = "cargo"
name      = "cargo"
upstreams = [
  "http://localhost:8080/proxy/internal-cargo",  # BatleHub proxies with stored credentials
  "https://static.crates.io/crates",             # public fallback — no auth needed
]
index_url = "https://index.crates.io"

[registries.rbac]
anonymous = ["source:read"]
user      = ["source:read"]
admin     = ["*"]
```

Clients configure only the fanout registry:

```toml
# ~/.cargo/config.toml
[registries.cargo]
index = "sparse+https://batlehub.example.com/proxy/cargo/registry/"
```

When BatleHub resolves a crate through the `cargo` registry it first fetches `http://localhost:8080/proxy/internal-cargo/…`; that self-request is served by the `internal-cargo` registry which injects the Gitea bearer token on the way out. If the crate is not found (404), BatleHub falls through to `crates.io` without any credentials. The client never knows the private registry exists.

## Secret management

Credential values (`token`, `password`, `value`) are stored in the TOML config file. In production:
- Use a secrets manager (Vault, AWS Secrets Manager, Kubernetes Secrets) to inject values at runtime.
- Many deployment tools (Helm, Kustomize, systemd `EnvironmentFile`) support substituting environment variable references into config files before the process starts.

See [Worked Example 6.5](/guide/configuration-examples#_6-5-self-hosted-private-registries) for a full multi-registry config.

