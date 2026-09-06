---
# The configuration reference: 11 955 words of field tables, because the
# TOML surface is large. `docs:structure` asks for this line above 4 000 words —
# not a cap, a declaration someone had to type (RFC 0005-bis §4.5).
reference: true
---

# Configuration Reference

batlehub is configured with a single TOML file. This document covers every option, how they interact, and includes copy-paste examples for common deployment scenarios.

## 1. Quick Start

Copy this into `config.toml`, start PostgreSQL, and run the server:

```toml
[server]
port = 8080

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value = "my-admin-token"
role = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "./cache"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user = ["releases:read", "source:read"]
admin = ["*"]
```

```sh
batlehub --config config.toml
```

Verify the server is running:

```sh
curl http://localhost:8080/api/openapi.json
```

Authenticated requests use a Bearer token:

```sh
curl -H "Authorization: Bearer my-admin-token" http://localhost:8080/...
```

---

## 2. How Configuration Works

### Loading order

1. The TOML file at the path given to `--config` is parsed (default: `config.toml` in the working directory).
2. Environment variables matching `PROXY_CACHE__<SECTION>__<FIELD>` are applied on top of the file values.
3. The config is validated: `config_version` (if set) must not exceed what this binary supports, registry names must not be empty, and registry types must be one of `github`, `npm`, `cargo`, `openvsx`, `vscode-marketplace`, `goproxy`, `maven`, `terraform`, `rubygems`, `composer`, `pypi`, `conda`.

### Auth evaluation order

The `[[auth]]` array is tried in declaration order. The first provider that recognises a credential wins and the request proceeds with that identity. If no provider matches, the request is treated as `anonymous`. Putting a token provider before OIDC means static tokens are checked first, which is slightly more efficient.

### Config versioning

A top-level, optional `config_version` field pins a config file to a schema version:

```toml
config_version = 1   # optional; absent means "current"
```

- **Absent is always accepted** and treated as the binary's current schema version, so every existing config file keeps working unchanged across upgrades.
- **An explicit value newer than what the running binary supports** fails validation at startup with an upgrade-path message, instead of silently ignoring fields it doesn't understand yet.
- **An explicit value older than current** is currently accepted (there is no migration engine yet) — this field exists so a future breaking change has somewhere to hang a version check, not to enable time-travel to old behavior today.

What requires bumping `CURRENT_CONFIG_VERSION` (in `crates/config/src/schema/mod.rs`) when it eventually happens: removing or renaming an existing field, or changing what an existing field's default means. What does **not** require a bump: adding a new optional field (the common case for this codebase's evolution so far — see `CHANGELOG.md` for what changed in each release).

---

## 3. Full Reference

### 3.1 `[server]`

Controls the HTTP listener and optional SPA serving.

```toml
[server]
host = "0.0.0.0"        # default
port = 8080             # default
# static_dir = "./ui/dist"  # optional: serve the built Vue SPA from this path
# trusted_proxies = ["10.42.0.0/16"]   # see "Proxy trust" below
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `host` | string | `"0.0.0.0"` | Bind address |
| `port` | u16 | `8080` | TCP port |
| `static_dir` | string | — | Path to the built SPA; when set, the server serves the frontend at `/` |
| `cors_allowed_origins` | string[] | *absent* → same-origin only | Origins allowed to read cross-origin responses. `["*"]` opts back in to any origin. See [CORS](#cors) |
| `cli_binary_path` | string | — | Path to `batlehub-cli`, served at `GET /api/v1/cli/download` |
| `trusted_proxies` | string[] | *absent* | CIDR ranges (or bare IPs) of reverse proxies whose `X-Forwarded-*` headers are believed |
| `signed_urls` | table | *absent* | Signing material for download URLs. See [`[server.signed_urls]`](#server-signed-urls) |
| `roles` | string[] | `["proxy", "worker"]` | What this process does: `proxy` serves requests and queues scan jobs, `worker` dequeues and scans them. The default is both (an embedded worker). `batlehub --roles worker` overrides it for a scan-only process. See [`[registries.security]`](#registries-security) and [`[worker]`](#scanners-and-worker). |

#### CORS

| `cors_allowed_origins` | Behaviour |
|---|---|
| absent or `[]` | Same-origin only — no CORS headers are emitted |
| `["*"]` | Any origin may read responses (explicit opt-out; raises a `cors.any-origin` config warning) |
| `["https://ui.example", …]` | Exactly those origins |

Most deployments need nothing here. The server hosts the SPA itself when
`static_dir` is set, and same-origin requests never consult CORS — so the UI
keeps working with the field unset. Set it only when the UI is served from a
different origin than the API.

> **Changed in 1.1.0 — breaking.** An empty or absent list used to allow *every*
> origin. Any website a visitor happened to open could then issue cross-origin
> requests to this server and read the responses. Credentials are never sent
> cross-origin, so this was not a route to stealing a token — but for a registry
> proxy inside a private network it meant a public page could enumerate internal
> package metadata using the visitor's browser as its network position.
>
> **Upgrading:** if your UI is served from the same origin as the API (the
> default, including every Helm-chart deployment), there is nothing to do. If it
> is served from a different origin, add that origin explicitly:
>
> ```toml
> [server]
> cors_allowed_origins = ["https://ui.example.com"]
> ```
>
> To keep the pre-1.1.0 behaviour verbatim, set `cors_allowed_origins = ["*"]`.
> The server will start and log a `cors.any-origin` warning, visible at
> `GET /api/v1/admin/config/warnings` and on the Config Reload admin page.

#### Proxy trust

Three headers from a reverse proxy shape what BatleHub does, and all three are
attacker-settable when the server is exposed directly:

| Header | Decides |
|---|---|
| `Forwarded` / `X-Forwarded-Host` | the host in every generated URL — NuGet service indexes, npm `dist.tarball`, PyPI simple pages, Composer `dist`, Terraform `download_url` — and, with [`[subdomain_routing]`](#39-subdomain_routing-optional), **which registry** serves the request |
| `X-Forwarded-Proto` | `http` vs `https` in those URLs |
| `X-Forwarded-For` | the client IP the [`[ip_blocking]`](#36-ip_blocking-optional) middleware counts violations against |

`trusted_proxies` states which peers may set them. It has three distinguishable
states:

| Value | Host + scheme | Client IP |
|---|---|---|
| absent | forwarded headers believed from **any** peer | TCP peer (`X-Forwarded-For` ignored) |
| `[]` | `Host` header and the connection only | TCP peer |
| `["10.42.0.0/16"]` | forwarded headers believed from peers inside the range, `Host` from everyone else | right-most `X-Forwarded-For` entry outside the range, from a peer in range; TCP peer otherwise |

**Use CIDR ranges, not exact IPs.** A Kubernetes ingress sits behind a pod CIDR
that changes on every rollout, so enumerating addresses is unmaintainable. A bare
address is accepted and treated as a `/32` (`/128` for IPv6).

**Absent is a hard error once host-based routing is configured** — routing on a
header the server has no stated policy about is not a state a deployment should
reach. For everyone else, absent keeps the pre-existing behaviour, because
tightening it by default would silently change the URLs existing deployments
advertise. The startup error contains the exact TOML to paste.

> **Deprecated:** `[ip_blocking].trusted_proxies` still works. When
> `[server].trusted_proxies` is absent it is used, and then governs the forwarded
> host and scheme as well as the client IP — including satisfying the host-routing
> requirement above, so an existing deployment can adopt host routing without
> touching its proxy-trust config. When both are set, `[server]` wins. Either way
> you get a config warning; see [`GET /api/v1/admin/config/warnings`](/guide/hot-reload#_9-2-api-endpoints).
>
> Unlike `[server].trusted_proxies`, an entry of the deprecated key that is
> neither an IP nor a CIDR range (a hostname, say) is **dropped with a warning**
> rather than refused at startup — that key predates the validator and used to
> discard such entries silently, so rejecting one now would break a config that
> never changed. The valid entries around it still apply.

#### `[server.signed_urls]` {#server-signed-urls}

Signing material for **signed download URLs**: a way to keep a registry closed
to anonymous callers even when the client fetches the artifact without
credentials. Terraform is the case this exists for — it authenticates the two
JSON documents of a provider install and then fetches the archive, its
`SHA256SUMS` and the `.sig` with no `Authorization` header and no mechanism to
send one.

Absent means the feature is unavailable. It is *global* rather than per-registry
because the key is a property of the instance; the switch that uses it is
[`signed_downloads`](#registry-signed-downloads) on each registry.

```toml
[server.signed_urls]
secret           = "${BATLEHUB_URL_SIGNING_SECRET}"
ttl_seconds      = 300
previous_secrets = ["${BATLEHUB_URL_SIGNING_SECRET_OLD}"]
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `secret` | string | *required* | HMAC-SHA256 signing key, **32 bytes minimum**. Interpolate it from the environment (see [Sensitive values](#env-inline)) — a signing key does not belong in a committed file |
| `ttl_seconds` | u64 | `300` | Lifetime of a minted URL. Hard-capped at `3600`; Terraform follows one within milliseconds, so the margin is for a slow runner rather than a human |
| `previous_secrets` | string[] | `[]` | Verified against but never minted with, so a secret can be rotated without a flag day. An entry that interpolates to empty is ignored — but the variable must still be **set** (to `""` is fine): `${VAR}` expansion runs before parsing and refuses an unset variable, so leaving this line in place after retiring the old secret fails the whole config load. Remove the line, or export the variable empty |

**Startup errors.** Each of these refuses to boot rather than degrading, because
every one of them produces a registry whose operator believes it is protected:

| Condition | Why it is fatal |
|---|---|
| A registry sets `signed_downloads = true` and this block is absent | The registry cannot serve the downloads it is closing off |
| `secret` is empty | Usually a `${VAR}` that is unset in this environment |
| `secret` is under 32 bytes | Too short for an HMAC-SHA256 key |
| `ttl_seconds` is `0` | Every minted URL would be born expired |
| `ttl_seconds` is above `3600` | A misconfiguration must not mint a month-long credential |
| An entry of `previous_secrets` is under 32 bytes | A short entry is a mistake, not a rotation in progress |

**Warnings** (non-fatal, served from `GET /api/v1/admin/config/warnings`):

| Code | Raised when |
|---|---|
| `signed-urls.unused` | A secret is configured and no registry sets `signed_downloads = true`, so nothing is signed |
| `signed-urls.anonymous-still-granted` | A signing registry still grants anonymous reads — legal, and usually a migration that stopped halfway, since signing exists so that grant can be removed |

::: warning A minted URL is a bearer capability, and it is in your logs
Until it expires, whoever holds the URL can fetch that one file as the user it
was minted for. BatleHub records the full request target — query string included
— in the `http.target` field of its request span at `INFO`, so log shipping,
OTLP export and any TLS-terminating proxy in front of BatleHub all capture it.

Bounded by the TTL, by the single coordinate, and by granting no permission the
signed-for user did not already have. If that is not enough for your estate:
lower `ttl_seconds`, and drop or rewrite `http.target` in your log pipeline. The
audit trail is unaffected — `access_events` records the package coordinate, never
the URL.
:::

---

### 3.2 `[database]`

batlehub uses PostgreSQL for storing registry metadata and user tokens.

```toml
[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"
max_connections = 10    # default
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `type` | string | — | Must be `"postgresql"` |
| `url` | string | — | Full PostgreSQL DSN including credentials |
| `max_connections` | u32 | `10` | Connection pool size |

The `url` field can be overridden at runtime via `PROXY_CACHE__DATABASE__URL` without touching the config file.

---

### 3.2a `[cache]`

Selects the storage backend for **metadata cache entries** and **rate-limit counters**. Both subsystems share this backend so a single configuration change affects them together.

```toml
# In-process memory (default — no extra infrastructure required)
[cache]
type = "memory"

# PostgreSQL — persistent across restarts, shared across replicas
[cache]
type = "postgres"

# Redis — persistent, shared, TTL-based eviction
[cache]
type = "redis"
url  = "redis://localhost:6379"
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `type` | string | `"memory"` | `"memory"`, `"postgres"`, or `"redis"` |
| `url` | string | — | Redis connection URL; required when `type = "redis"`. Format: `redis://[:<password>@]<host>[:<port>][/<db>]` or `rediss://…` for TLS. |

#### Backend comparison

| Backend | Persistence | Shared across replicas | Extra infra | Best for |
|---------|:-----------:|:---------------------:|:-----------:|---------|
| `memory` | No — resets on restart | No | None | Local dev, single-node |
| `postgres` | Yes | Yes | None (uses the existing `[database]`) | Production, multi-replica |
| `redis` | Yes | Yes | Redis cluster | High-throughput production |

> **`memory` is the default** and requires no config changes. Switch to `postgres` or `redis` when you run multiple server replicas or when you want rate-limit counters to survive server restarts.

> **Redis feature flag:** The `redis` backend is only compiled when the `cache-redis` feature is enabled. The official Docker image includes it. When building from source, pass `--features cache-redis` to `cargo build`.

#### How each backend is used

**Metadata cache:** Version lists and release metadata returned by upstream registries are stored with a TTL (`metadata_ttl_secs`). The cache backend is consulted on every proxy request before hitting the upstream.

**Rate-limit counters:** Each `increment` call atomically bumps a counter keyed by `rl:{registry}:user:{user_id}` (or `rl:{registry}:group:{group}`) and returns the new count plus the window-reset timestamp:
- `memory` — Mutex-protected HashMap; each process has its own counters.
- `postgres` — `INSERT … ON CONFLICT DO UPDATE … RETURNING count`; fully serialisable.
- `redis` — atomic `INCR` with a conditional `EXPIRE` on first write; TTL-based cleanup.

---

### 3.3 `[[auth]]`

An array of auth providers tried in declaration order. Three types are supported.

#### 3.3.1 Token auth (`type = "token"`)

Validates static bearer tokens defined in the config file. Useful for CI/CD pipelines and simple setups.

```toml
[[auth]]
type = "token"

[[auth.tokens]]
value = "my-ci-token"     # the bearer token value (plaintext or Argon2id PHC hash)
role = "user"             # "admin", "user", or "anonymous"
user_id = "ci-bot"        # optional: display name in logs

[[auth.tokens]]
value = "my-admin-token"
role = "admin"
user_id = "admin"
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `value` | string | yes | The Bearer token string — plaintext **or** an Argon2id PHC hash (see below) |
| `role` | string | yes | `"admin"`, `"user"`, or `"anonymous"` |
| `user_id` | string | no | Used in audit logs |

#### Argon2id hashed token values (recommended for production)

Instead of storing a raw token in the config file, store an **Argon2id PHC hash**. BatleHub ships a helper command that generates the hash from the raw token:

```sh
batlehub hash-token my-secret-token
# → $argon2id$v=19$m=65536,t=3,p=4$...
```

Copy the printed hash into the `value` field:

```toml
[[auth.tokens]]
value = "$argon2id$v=19$m=65536,t=3,p=4$..."
role  = "admin"
user_id = "admin"
```

BatleHub automatically detects PHC-format values (those starting with `$argon2`) and verifies incoming bearer tokens against the stored hash. Plaintext values continue to work without any change — the two formats can coexist in the same config file.

> **Why this matters:** If the config file leaks (e.g. committed to VCS by mistake, visible in a Kubernetes ConfigMap), hashed tokens cannot be used directly by an attacker. The raw token only ever needs to exist in your secrets manager or the developer's clipboard.

#### 3.3.2 OIDC auth (`type = "oidc"`)

Validates JWT Bearer tokens issued by any standards-compliant OIDC provider (Authentik, Keycloak, Dex, etc.). Optionally enables browser-based SSO login.

```toml
[[auth]]
type = "oidc"
# name = "oidc"           # default; must be unique when running multiple OIDC providers
issuer_url = "https://sso.example.com/application/o/batlehub/"
client_id = "batlehub"
# client_secret = "..."   # required for confidential clients
# redirect_uri = "https://batlehub.example.com/api/v1/auth/oidc/callback"
# frontend_url = ""       # default: same origin as the backend
scopes = ["openid", "profile", "email", "groups"]
user_id_claim = "preferred_username"   # default: "sub"
role_claim = "groups"                  # default: "role"

[auth.role_mappings]
"authentik Admins" = "admin"
"proxy-users"      = "user"
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | string | `"oidc"` | Provider name; becomes the group prefix (e.g. `"oidc:team-a"`). Must be unique across providers. |
| `required` | bool | `true` | Whether an identity provider that is unreachable at startup is fatal. Starting without it looks healthy and is not: every request that would have carried an identity becomes anonymous instead. Set `false` to warn and continue, which raises `batlehub_auth_provider_down`. |
| `issuer_url` | string | — | Base URL of the OIDC provider; `/.well-known/openid-configuration` is appended for endpoint discovery. Must be `https` (except on localhost), and the `issuer` the document declares must match it. |
| `client_id` | string | — | OAuth2 client identifier |
| `client_secret` | string | — | Required for confidential clients; optional for public clients |
| `redirect_uri` | string | — | When set, enables browser SSO at `/api/v1/auth/oidc/callback` (default provider) or `/api/v1/auth/oidc/{name}/callback` (named providers). Must be registered with the OIDC provider. |
| `frontend_url` | string | `""` | After a successful SSO callback the browser is redirected to `{frontend_url}/#oidc_access_token=...`. The tokens ride in the URL **fragment**, so they never reach the server hosting the SPA. Leave empty in production (same origin). Set to `http://localhost:5173` when running the Vite dev server separately. |
| `scopes` | string[] | `["openid","profile","email"]` | OAuth2 scopes to request |
| `audiences` | string[] | `[client_id]` | Values the token's `aud` claim is accepted for. Set explicitly when the provider issues tokens for a separate API audience (Auth0 `audience`, an Okta authorization server). Never unchecked. |
| `user_id_claim` | string | `"sub"` | JWT claim used as the user identifier. `"preferred_username"` gives human-readable names from Authentik/Keycloak. |
| `role_claim` | string | `"role"` | JWT claim inspected for role mapping. May be a string or array of strings; the highest matching role wins. |
| `role_mappings` | map | `{}` | Maps JWT claim values to proxy roles (`"admin"`, `"user"`, `"anonymous"`). Values not present default to `anonymous`. |

**Group namespacing:** Claim values that appear as keys in `role_mappings` are stored as-is in the identity's group list. Claim values not in `role_mappings` are prefixed with `{name}:` (e.g. `"oidc:team-a"`). This allows the RBAC `groups` table to use `"*:team-a"` as a cross-provider wildcard.

**Running multiple OIDC providers:** Set a unique `name` on each. Their callback URLs will be `/api/v1/auth/oidc/{name}/callback`.

**Token creation is scoped to these providers.** `POST /api/v1/auth/tokens` accepts a session from any provider declared here, whatever its `name` and whether or not it has a `redirect_uri`. No other credential can mint a personal access token: a static token, a Kubernetes service account, an Actions OIDC job or another PAT all get `403`, so a machine credential can never issue a longer-lived one. With no `type = "oidc"` provider configured, nobody can create a PAT.

**A personal access token carries a snapshot of its creator's groups.** The groups are chosen at creation, capped to the ones the creator holds — asking for one they do not is `403` — and never re-resolved afterwards, because a token has no session to re-resolve from. Two consequences for an operator:

- **A token's expiry is an access-control lifetime, not hygiene.** Group membership on a token does not follow the IDP, so a departed member keeps whatever the token carries until it expires or is revoked. Expiry is mandatory and capped at 90 days, and **offboarding should revoke tokens** rather than rely on the IDP change reaching them. For interactive users, an OIDC session re-resolves groups on every refresh and stays the recommended posture; tokens are for automation.
- **A token names the group the provider actually emits.** That is the prefixed form for anything not renamed by `role_mappings` above — `k8s:system:serviceaccounts:digital` from a provider named `k8s`, not `system:serviceaccounts:digital`. `batlehub auth whoami` prints them as resolved, which is the reliable way to spell one; the console offers them as buttons for the same reason.

Tokens minted before this existed carry no groups and are unchanged by it: they see `public` and `internal` and nothing granted to a team, exactly as they did. A user who needs a token to reach team packages creates a new one.

#### 3.3.3 Kubernetes auth (`type = "kubernetes"`)

Validates Kubernetes service account tokens via the Kubernetes TokenReview API. All fields default to the standard in-cluster mounted secrets and environment variables, so minimal configuration is needed when running inside a cluster.

```toml
[[auth]]
type = "kubernetes"
# name = "kubernetes"   # default

# All of the following default to in-cluster values:
# api_server   = "https://kubernetes.default.svc"
# ca_cert_path = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
# token_path   = "/var/run/secrets/kubernetes.io/serviceaccount/token"
# audiences    = ["batlehub"]
# issuers      = []     # any issuer; see below

[auth.role_mappings]
"system:serviceaccount:prod:ci-deployer" = "admin"
"system:serviceaccounts:staging"         = "user"
"system:serviceaccounts"                 = "anonymous"
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | string | `"kubernetes"` | Provider name; becomes the group prefix |
| `api_server` | string | from `KUBERNETES_SERVICE_HOST` / `KUBERNETES_SERVICE_PORT` env | Kubernetes API server URL. **Must be `https://`** — see below |
| `ca_cert_path` | string | `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt` | CA cert for API server TLS verification |
| `token_path` | string | `/var/run/secrets/kubernetes.io/serviceaccount/token` | batlehub's own service account token for TokenReview calls; re-read each request to handle automatic rotation |
| `audiences` | string[] | `["batlehub"]` | Audiences sent in the TokenReview request, **and required back in its response** — see below |
| `issuers` | string[] | `[]` (any) | Token issuers (`iss`) worth a TokenReview. Set it when this server sees tokens from more than one issuer — see below |
| `role_mappings` | map | `{}` | Maps Kubernetes usernames or group names to proxy roles |

**`api_server` must be `https://`, and the server refuses to start otherwise** (plain HTTP is accepted for `localhost` and `127.0.0.1` only, as it is for an OIDC `issuer_url`). The rule is stricter here than it looks: every TokenReview carries BatleHub's *own* service account token, and the reply decides the caller's identity. Anyone sitting on a cleartext path both learns that token and can answer `authenticated: true` with `system:serviceaccount:…`, which `role_mappings` will translate into whatever role that key names — up to `admin`. Leave `api_server` unset in-cluster and the default is `https://` by construction.

**Every `[[auth]]` `name` must be unique across all provider types**, and the server refuses to start on a collision. The name is not a label: it is what a session, a stored OIDC refresh token and an unmapped group (`"k8s-prod:team-a"`) are attributed to. Two providers sharing one are one provider as far as all of that is concerned — a `type = "kubernetes"` provider named `"corp"` would let a service account act on the sessions and personal access tokens of the OIDC provider named `"corp"`.

**Role mapping keys:** Kubernetes sets `username: "system:serviceaccount:<namespace>:<name>"` and `groups: ["system:serviceaccounts", "system:serviceaccounts:<namespace>", ...]`. When a token matches multiple keys, the highest role wins.

**Audience binding is enforced in both directions.** `audiences` is sent as `spec.audiences`, and the TokenReview response is only accepted when its `status.audiences` contains at least one of them. A token the API server authenticates but does not confirm as bound to one of these audiences is refused, and the rejection is logged at `warn` with both lists.

This matters because the default service account token mounted into every pod in the cluster is bound to the API server, not to BatleHub. Without the response-side check, an authenticator that ignores `spec.audiences` would let any pod in the cluster authenticate here.

So the workload must present a **projected** token minted for this audience, not the default mounted one:

```yaml
volumes:
  - name: batlehub-token
    projected:
      sources:
        - serviceAccountToken:
            path: token
            audience: batlehub        # must match one entry in `audiences`
            expirationSeconds: 3600
```

Point the client at `/var/run/secrets/batlehub/token` (`batlehub-cli auth login --kubernetes-token-path`). If authentication starts failing with `TokenReview authenticated a token the API server did not confirm is bound to a requested audience` in the logs, the workload is sending the default token and needs the projected volume above.

**Only credentials that could be ours are sent to the API server.** Three filters run before any TokenReview, in order:

- a bearer token that is not three dot-separated parts cannot be a service account token, so it is passed to the next provider untouched — this keeps personal access tokens out of the control plane's request logs;
- a JWT whose own `aud` claim shares nothing with `audiences` is refused locally. This is the same check `status.audiences` gets after the round trip (that field is the intersection of `spec.audiences` and the token's `aud`, so such a token could never come back confirmed), moved earlier. It matters because an OIDC ID token *is* JWT-shaped: with `type = "kubernetes"` listed before `type = "oidc"` — the natural order in a cluster — every browser request's ID token would otherwise be POSTed verbatim to the API server;
- when `issuers` is set, a JWT from any other issuer is refused locally too. Leave it empty unless this server sees tokens from more than one issuer with the same audience name (federated clusters, a cloud OIDC provider alongside the in-cluster one). Read it with `kubectl get --raw /.well-known/openid-configuration | jq -r .issuer`.

None of this grants anything: claims are read without verifying the signature, and can only make BatleHub decline to *ask*. The TokenReview verdict remains what authenticates.

**Verdicts are cached, both kinds.** A success is reused for 60 seconds, a rejection for 10 — keyed by the SHA-256 of the token, never by the token itself. Without the second, a client repeating a credential the cluster refuses (a misconfigured CI job, a stale token in a loop) put one TokenReview on the API server per proxied request with no ceiling. Ten seconds is also the longest a service account waits after its RoleBinding lands.

#### 3.3.4 Actions OIDC auth (`type = "actions-oidc"`)

Validates short-lived OIDC JWTs issued by GitHub Actions or Forgejo Actions to workflow jobs (requires `id-token: write` in the workflow permissions). Rather than mapping a single claim value to a role, it evaluates a list of **rules** — each rule matches on any combination of JWT claims and grants a group name and a role when it matches.

```toml
[[auth]]
type = "actions-oidc"
name = "forgejo-action"                    # default: "actions-oidc"
issuer_url = "https://forgejo.example.com" # GitHub: "https://token.actions.githubusercontent.com"
audience = "https://batlehub.example.com"  # REQUIRED — see below
# user_id_claim = "sub"                    # default

  # Static group: deployers on the main branch
  [[auth.rules]]
  group = "ci-deployers"
  role  = "admin"
  match = "all"              # all conditions must pass (default)
  [[auth.rules.conditions]]
  claim   = "repository_owner"
  pattern = "batleforc"
  [[auth.rules.conditions]]
  claim   = "ref"
  pattern = "refs/heads/main"

  # Dynamic group: every token gets an automatic per-repo/per-branch group
  # e.g. "forgejo-action/batleforc-batlehub/main"
  [[auth.rules]]
  group_template = "{name}/{repository}/{ref_name}"
  role           = "user"
  match          = "all"
  [[auth.rules.conditions]]
  claim   = "repository_owner"
  pattern = "batleforc"       # glob: exact match

  # Regex example: tag-based releases
  [[auth.rules]]
  group = "tag-releasers"
  role  = "user"
  match = "all"
  [[auth.rules.conditions]]
  claim      = "ref"
  pattern    = "^refs/tags/v[0-9]+"
  match_type = "regex"        # explicit; auto-detected from "^" anyway
```

**Provider fields:**

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | string | `"actions-oidc"` | Provider name. Appears in log output and in `Identity.auth_provider`. Must be unique across all `[[auth]]` entries. |
| `issuer_url` | string | — | OIDC issuer base URL. GitHub: `"https://token.actions.githubusercontent.com"`. Forgejo: your instance URL. Must be `https` (except on localhost). |
| `required` | bool | `false` | Whether an unreachable issuer at startup is fatal. Defaults to `false` here, unlike `type = "oidc"`: a CI provider being down stops publishing, not signing in. |
| `audience` | string | — | **Required.** Value the token's `aud` claim must equal. |
| `user_id_claim` | string | `"sub"` | JWT claim used as `user_id` in the resolved identity. |
| `rules` | array | `[]` | Ordered list of group rules evaluated against each JWT. All matching rules contribute — they are not exclusive. |

**`audience` is what makes this provider safe, and it has no default.** The issuer is shared: `https://token.actions.githubusercontent.com` signs a token for *any* workflow in *any* repository on GitHub, so validating `iss` proves only that the caller is a GitHub Actions job somewhere. `aud` is the one claim the calling workflow chooses, so it is what says "this token was minted for *this* deployment". Server startup fails if it is missing or blank.

Pick something specific to the deployment — its URL is the conventional choice — and have workflows request it:

```yaml
# GitHub Actions
- uses: actions/github-script@v7
  id: token
  with:
    script: return await core.getIDToken('https://batlehub.example.com')
```

The `rules` below still decide what the caller may *do*; `audience` decides whether it is heard at all. A deployment with loose rules and no audience check was reachable by any repository on the forge.

**Rule fields (`[[auth.rules]]`):**

| Field | Type | Default | Notes |
|---|---|---|---|
| `group` | string | — | Static group name granted when the rule matches. At least one of `group` or `group_template` is required. |
| `group_template` | string | — | Template for a dynamically-named group. See template variables below. |
| `role` | string | `"user"` | Role granted by this rule (`"admin"`, `"user"`, `"anonymous"`). The final role is the highest across all matching rules. |
| `match` | `"all"` \| `"any"` | `"all"` | Whether all conditions must pass (AND) or at least one (OR). |
| `conditions` | array | `[]` | Conditions evaluated against JWT claims. An empty list always matches. |

**Condition fields (`[[auth.rules.conditions]]`):**

| Field | Type | Default | Notes |
|---|---|---|---|
| `claim` | string | — | JWT claim key to test (e.g. `"repository"`, `"ref"`, `"environment"`, `"actor"`). |
| `pattern` | string | — | Pattern to match the claim value against. |
| `match_type` | `"auto"` \| `"glob"` \| `"regex"` | `"auto"` | Pattern type. `auto` treats the pattern as regex when it starts with `^`, ends with `$`, or contains `[`, `(`, `+`. Otherwise it is treated as a glob. |

**Pattern types:**

- **Glob** — shell-style wildcards: `myorg/*` matches `myorg/foo` but not `other/foo`. `*` matches any sequence of characters.
- **Regex** — full `regex` crate syntax: `^refs/tags/v[0-9]+` matches any tag starting with `v` followed by digits. Compilation errors abort provider startup.

**Group template variables:**

Templates are `{placeholder}` strings rendered per-request. Substituted values have `/` replaced with `-` (so group names stay path-safe); literal `/` in the template itself is preserved.

| Variable | Value |
|----------|-------|
| `{name}` | Provider's `name` field |
| `{ref_name}` | `ref` claim with `refs/heads/` or `refs/tags/` prefix stripped |
| `{<any claim key>}` | Value of that JWT claim, with `/` → `-` |

Example: with `name = "forgejo-action"`, `repository = "batleforc/batlehub"`, `ref = "refs/heads/main"`:

```
"{name}/{repository}/{ref_name}"  →  "forgejo-action/batleforc-batlehub/main"
```

**GitHub Actions OIDC token claims (representative subset):**

| Claim | Example value | Description |
|-------|---------------|-------------|
| `sub` | `repo:org/repo:ref:refs/heads/main` | Subject (unique token identifier) |
| `repository` | `org/my-repo` | Repository in `owner/name` form |
| `repository_owner` | `org` | Repository owner (user or org) |
| `ref` | `refs/heads/main` | Full Git ref |
| `ref_type` | `branch` or `tag` | Type of ref |
| `workflow` | `CI` | Workflow name |
| `environment` | `production` | Deployment environment (if set) |
| `actor` | `alice` | GitHub username who triggered the run |
| `event_name` | `push` | Triggering event |
| `sha` | `abc123…` | Commit SHA |

Forgejo issues tokens with the same claim structure; only the issuer URL differs.

**Granting access via RBAC:**

Dynamic groups enable wildcard grants. To allow all CI tokens from `batleforc`'s repos to read releases:

```toml
[registries.rbac.groups]
"forgejo-action/*" = ["releases:read"]

# Grant specific per-repo CI full publish access
"forgejo-action/batleforc-batlehub/*" = ["releases:read", "releases:publish"]
```

**GitHub Actions workflow snippet:**

```yaml
jobs:
  publish:
    permissions:
      id-token: write   # required to request an OIDC token
      contents: read
    steps:
      - name: Push artifact
        env:
          BATLEHUB_TOKEN: ${{ secrets.BATLEHUB_TOKEN }}
        run: |
          # BatleHub validates the OIDC token; no long-lived secret needed
          # when using actions-oidc — pass the ACTIONS_ID_TOKEN_REQUEST_URL
          # and ACTIONS_ID_TOKEN_REQUEST_TOKEN env vars to your publish tool
          cargo publish --registry batlehub
```

---

### 3.4 `[storage]`

Two formats are supported: single-backend (simpler, supports env-var overrides) and multi-backend (allows per-registry routing).

#### Single backend

```toml
# Filesystem
[storage]
type = "filesystem"
path = "./cache"

# S3 (or S3-compatible: MinIO, RustFS, etc.)
[storage]
type = "s3"
bucket = "my-artifacts"
region = "us-east-1"
prefix = "batlehub/"         # optional, default: none
endpoint_url = "http://minio:9000"  # optional: omit for real AWS
force_path_style = true         # optional: required for MinIO and RustFS
```

**Filesystem fields:**

| Field | Type | Required | Notes |
|---|---|---|---|
| `path` | string | yes | Directory for cached files; created if it does not exist |

**S3 fields:**

| Field | Type | Required | Notes |
|---|---|---|---|
| `bucket` | string | yes | S3 bucket name |
| `region` | string | yes | AWS region (e.g. `"us-east-1"`) |
| `prefix` | string | no | Key prefix for all stored objects |
| `endpoint_url` | string | no | Custom endpoint for S3-compatible stores |
| `force_path_style` | bool | no | Required for MinIO, RustFS, and other S3-compatible stores that use path-style URLs |

S3 credentials are sourced from the standard AWS SDK credential chain: `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` environment variables, `~/.aws/credentials`, EC2/ECS instance metadata, and so on.

#### Multi-backend

Use this when different registries should store artifacts in different backends.

```toml
[storage]
default = "primary"           # required: name of the fallback backend

[[storage.backends]]
name = "primary"
type = "filesystem"
path = "./cache"

[[storage.backends]]
name = "s3-artifacts"
type = "s3"
bucket = "release-artifacts"
region = "eu-west-1"
```

Then assign a registry to a specific backend with the `storage` field:

```toml
[[registries]]
type = "github"
name = "github"
storage = "s3-artifacts"    # this registry uses s3-artifacts; others use "primary"
```

> **Note:** Environment variable overrides for storage fields (`PROXY_CACHE__STORAGE__PATH`, etc.) only work with the single-backend form. Multi-backend configs must be changed in the file.

---

### 3.5 `[[registries]]`

An array of package registry proxies. Each entry configures one registry endpoint.

```toml
[[registries]]
type = "cargo"
name = "cargo"
# upstreams = ["https://crates.io"]   # default for cargo
# index_url = "https://index.crates.io"  # default; set for self-hosted registries
# storage = "backend-name"            # optional: use a named storage backend

[registries.cache]
metadata_ttl_secs = 300     # default: 300 (5 minutes)
# artifact_ttl_secs = 2592000  # optional: re-fetch artifacts older than 30 days

[registries.rbac]
anonymous = []
user = ["releases:read", "source:read"]
admin = ["*"]

[registries.rbac.groups]
"team-a" = ["releases:read", "source:read"]
"*:ops"  = ["*"]   # wildcard: any provider's "ops" group

[[registries.rules]]
kind = "release_age_gate"
min_age_secs = 3600              # default: 3600 (1 hour)
bypass_roles = ["admin"]
deny_missing_timestamp = false   # set true to block packages with no timestamp

# [[registries.rules]]
# kind = "require_signed_release"
# enabled = true
```

**Top-level fields:**

| Field | Type | Required | Notes |
|---|---|---|---|
| `type` | string | yes | `"github"`, `"forgejo"`, `"gitlab"`, `"npm"`, `"cargo"`, `"nuget"`, `"openvsx"`, `"vscode-marketplace"`, `"goproxy"`, `"maven"`, `"terraform"`, `"rubygems"`, `"composer"`, `"pypi"`, `"conda"`, `"deb"`, `"rpm"`, `"pacman"`, `"jetbrains"`, `"jetbrains-marketplace"`, `"generic"`, `"nodedist"`, `"sdkman"` |
| `name` | string | yes | Unique identifier; used in proxy URL paths |
| `mode` | string | no | `"proxy"` (default), `"local"`, or `"hybrid"`. Supported for `cargo`, `npm`, `openvsx`, `vscode-marketplace`, `goproxy`, `maven`, `terraform`, `rubygems`, `composer`, `pypi`, `conda`, and `jetbrains-marketplace`. See [registry modes](#registry-modes). |
| `upstreams` | string[] | no | Upstream URLs tried in order on cache miss; 404 from one falls through to the next. Defaults to the registry's built-in URL. Required for `hybrid` mode. |
| `index_url` | string | no | Cargo only: sparse crate index URL. Defaults to `https://index.crates.io`. Required for `hybrid` mode and self-hosted Gitea/Forgejo registries. |
| `broker_url` | string | no | **sdkman only.** The download broker, the second host of the one protocol. Defaults to `https://broker.sdkman.io`; `upstreams` is the candidates API (`https://api.sdkman.io/2`). An absolute http(s) URL; rejected on any other type ([RFC 0010](/rfc/0010-toolchain-managers) §4.5). |
| `storage` | string | no | Name of the storage backend. Must match a `[[storage.backends]]` name. Omit to use the default backend. |
| `path_allow` | string[] | no | Glob allowlist of upstream paths this registry may serve. Only valid for the path-addressed types (`deb`, `rpm`, `pacman`, `jetbrains`, `generic`) — using it elsewhere is a config error. **Required and non-empty for `generic`.** Use `["**"]` to allow everything deliberately. |
| `on_confirmed` | string | no | RFC 0014 §13 O6 — what the upstream audit does with a disappearance confirmed on *this* registry, `"audit"` or `"block"`. Overrides `[upstream_audit] on_confirmed` for this registry alone; absent, the estate's key applies. `"block"` needs the audit enabled and this registry audited, or the config is refused. |
| `vuln_db_url` | string | no | **goproxy only.** Upstream URL for the Go Vulnerability Database. Default: `https://vuln.go.dev`. Set to `""` to disable the `/v1/` endpoints. See [Vulnerability Proxy](/use/vulnerability-proxy#_1-go-—-govulncheck-go-vulnerability-database). |
| `sumdb_url` | string | no | **goproxy only.** Upstream URL for the Go checksum database. Default: `https://sum.golang.org`. Set to `""` to disable `/sumdb/{path}` — do that for a registry serving only private modules, where a lookup would leak private module paths to a public log. |
| `upstream_auth` | table | no | Credentials sent on every upstream request. See [upstream auth](#upstream_auth). |
| `signed_downloads` | bool | no | `false`. Mint and accept signed download URLs for this registry, so it can keep `anonymous = []` even though the client fetches artifacts without credentials. Requires [`[server.signed_urls]`](#server-signed-urls) — setting it without one is a startup error. See [signed downloads](#registry-signed-downloads). |
| `tls` | table | no | TLS settings for upstream connections. See [upstream TLS](#upstream_tls). |
| `proxy` | table | no | HTTP/SOCKS proxy for upstream connections. See [upstream proxy](#upstream_proxy). |

#### Registry modes {#registry-modes}

`cargo`, `npm`, `openvsx`, `vscode-marketplace`, `goproxy`, `maven`, `terraform`, `rubygems`, `composer`, `pypi`, `conda`, and `jetbrains-marketplace` registries support three operating modes, set via the `mode` field:

| Mode | Description |
|------|-------------|
| `proxy` | Default. BatleHub only forwards requests to upstream registries. Publishing is rejected. |
| `local` | BatleHub is the authoritative registry. No upstream needed. Clients publish directly to BatleHub. |
| `hybrid` | Local-first. Serves locally published packages directly; falls back to the configured upstream for anything not published locally. Requires `upstreams` (and `index_url` for Cargo). |

Publishing requires at least the `user` role. The `published_by` field is set from the authenticated user's `user_id`.

**Cargo** — `local`/`hybrid` modes expose the full publish API (`PUT /api/v1/crates/new`, yank, unyank, owners) and advertise the `api` URL in `config.json` so Cargo discovers it automatically.

**npm** — `local`/`hybrid` modes accept `npm publish` payloads (`PUT /proxy/{registry}/{name}`) and serve packuments and tarballs from local storage.

**openvsx / vscode-marketplace** — `local`/`hybrid` modes accept raw VSIX uploads (`PUT /proxy/{registry}/{extension_id}/{version}/vsix`) and serve them on download.

**goproxy** — `local`/`hybrid` modes accept Go module zip uploads (`PUT /proxy/{registry}/{module}/@v/{version}.zip`). `go.mod` is extracted automatically from the zip; `.info` is generated from the version and upload timestamp. Serves `@latest`, `@v/list`, `.info`, `.mod`, and `.zip` from local storage.

**maven** — `local`/`hybrid` modes accept `mvn deploy` artifact uploads (`PUT /proxy/{registry}/maven2/{path}`). Non-POM files (JARs, checksums) are stored immediately; the three-phase publish is triggered when the `.pom` file arrives. `maven-metadata.xml` is generated dynamically from the database and never cached client-side. See [Worked Example 6.12](/guide/configuration-examples#612-private-maven-registry-local--hybrid-mode).

**terraform** — `local`/`hybrid` modes accept module uploads (`POST /proxy/{registry}/v1/modules/{ns}/{name}/{provider}/{version}`), provider version manifests (`POST .../v1/providers/{ns}/{type}/versions`), and provider binary uploads (`PUT .../artifact/{os}/{arch}`). The `tf_module_download` endpoint returns a `204 + X-Terraform-Get` header pointing at the locally stored tarball. See [Worked Example 6.13](/guide/configuration-examples#613-private-terraform-registry-local--hybrid-mode).

**rubygems** — `local`/`hybrid` modes accept `gem push` uploads (`POST /proxy/{registry}/api/v1/gems`). Serves gem files, version index, and REST info from local storage.

**composer** — `local`/`hybrid` modes accept ZIP uploads (`POST /proxy/{registry}/api/upload`). `composer.json` (with `name` and `version` fields) is extracted automatically. Serves `packages.json`, `p2/` metadata, and `dist/` artifacts from local storage.

**pypi** — `local`/`hybrid` modes accept twine-compatible multipart uploads (`POST /proxy/{registry}/legacy/`). The name and version are parsed from the uploaded filename and multipart fields. In `local` mode the Simple API index (`GET /proxy/{registry}/simple/{package}/`) is generated from the database. In `hybrid` mode upstream and local entries are served together.

**conda** — `local`/`hybrid` modes accept raw conda package uploads (`POST /proxy/{registry}/{platform}/`). Metadata (`name`, `version`, `build`, `depends`) is extracted from `info/index.json` inside the `.tar.bz2` or `.conda` archive. In `local` mode `repodata.json` is generated from the database. In `hybrid` mode local entries are merged into the upstream `repodata.json`.

#### Registry-type notes

**`github`** — proxies the GitHub REST API (releases, assets, source tarballs, raw files). Requires `upstreams` to point at `https://api.github.com` (the default).

**`npm`** — proxies the full npm registry protocol: packuments, version metadata, and `.tgz` tarballs. Works with npm, yarn, pnpm, and any tool that speaks the npm registry protocol. Set `mode = "local"` or `mode = "hybrid"` to enable publishing. See [registry modes](#registry-modes) and [Worked Example 6.7](/guide/configuration-examples#67-private-npm-registry-local--hybrid-mode). Both `npm audit` modes (`quick` and `bulk`) are proxied automatically — see [Vulnerability Proxy](/use/vulnerability-proxy#_2-npm-—-npm-audit).

**`cargo`** — proxies the Cargo sparse index and `.crate` downloads. Set `index_url` for self-hosted Gitea/Forgejo registries. Set `mode = "local"` or `mode = "hybrid"` to enable publishing. See [registry modes](#registry-modes) and [Worked Example 6.6](/guide/configuration-examples#66-private-cargo-registry-local--hybrid-mode).

**`openvsx`** — proxies VS Code extension VSIX downloads from [open-vsx.org](https://open-vsx.org) or a compatible host. Extension IDs use the `{publisher}.{name}` convention. Set `mode = "local"` or `mode = "hybrid"` to enable publishing. See [Worked Example 6.8](/guide/configuration-examples#68-private-vs-code-extension-registry-local--hybrid-mode).

**`vscode-marketplace`** — proxies VS Code extension VSIX downloads from [marketplace.visualstudio.com](https://marketplace.visualstudio.com) using Microsoft's Gallery API. Extension IDs use the same `{publisher}.{name}` convention as OpenVSX. Metadata is resolved via a `POST /_apis/public/gallery/extensionquery` call; artifacts are fetched directly from `/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage`. Use this type when you need to cache extensions that are only available on the Microsoft marketplace and not mirrored on open-vsx.org. Supports `mode = "local"` and `mode = "hybrid"` for hosting private extensions — see [Worked Example 6.8](/guide/configuration-examples#68-private-vs-code-extension-registry-local--hybrid-mode).

```toml
[[registries]]
type = "vscode-marketplace"
name = "vscode"
# upstreams = ["https://marketplace.visualstudio.com"]  # default

[registries.rbac]
user = ["releases:read", "source:read"]
admin = ["*"]
```

Download a VSIX via the proxy:

```sh
# Latest version
curl -H "Authorization: Bearer <token>" \
  http://localhost:8080/proxy/vscode/ms-python.python/latest/vsix \
  -o ms-python.python.vsix

# Pinned version
curl -H "Authorization: Bearer <token>" \
  http://localhost:8080/proxy/vscode/ms-python.python/2024.2.1/vsix \
  -o ms-python.python-2024.2.1.vsix
```

**`jetbrains-marketplace`** — full [JetBrains Marketplace](https://plugins.jetbrains.com) emulation for the IDE plugin ecosystem (search, compatible updates, `meta.json` blobs, plugin downloads), distinct from the path-addressed `jetbrains` IDE-archive type. Point an IDE at the proxy either **fully** (Help → Edit Custom Properties… → `idea.plugins.host=https://your-host/proxy/{registry}`) or **additively** (Settings → Plugins → Manage Plugin Repositories… → `https://your-host/proxy/{registry}/updatePlugins.xml`). Supports `mode = "local"`/`"hybrid"` with a marketplace-compatible multipart publish (`POST /proxy/{registry}/api/updates/upload`), so JetBrains' `plugin-repository-rest-client` and the Gradle `publishPlugin` task work against it. Per-plugin metadata, artifacts, and forwarded query blobs are cached with stale fallback: anything seen once keeps resolving if plugins.jetbrains.com becomes unreachable.

```toml
[[registries]]
type = "jetbrains-marketplace"
name = "jbm"
mode = "hybrid"                                # or "proxy" / "local"
upstreams = ["https://plugins.jetbrains.com"]  # default

[registries.rbac]
user = ["releases:read"]
admin = ["*"]
```

Download and publish via the proxy:

```sh
# Download a plugin version
curl -H "Authorization: Bearer <token>" \
  "http://localhost:8080/proxy/jbm/plugin/download?pluginId=org.rust.lang&version=241.25026.107" \
  -o rust-plugin.zip

# Publish a plugin (local/hybrid mode)
curl -X POST -H "Authorization: Bearer <token>" \
  -F "xmlId=com.example.myplugin" \
  -F "file=@my-plugin.zip" \
  http://localhost:8080/proxy/jbm/api/updates/upload
```

**`goproxy`** — implements the [GOPROXY protocol](https://go.dev/ref/mod#goproxy-protocol) for Go module proxying. Set `mode = "local"` or `mode = "hybrid"` to host private modules — see [registry modes](#registry-modes) and [Worked Example 6.9](/guide/configuration-examples#69-private-go-module-proxy-local--hybrid-mode). Supports all five module proxy endpoints plus the Go Vulnerability Database (`govulndb`) protocol — see [Vulnerability Proxy](/use/vulnerability-proxy#_1-go-—-govulncheck-go-vulnerability-database).

| Endpoint | Description |
|----------|-------------|
| `/{module}/@latest` | Latest version metadata JSON |
| `/{module}/@v/list` | Newline-separated list of known versions |
| `/{module}/@v/{version}.info` | Version metadata JSON |
| `/{module}/@v/{version}.mod` | Raw `go.mod` file |
| `/{module}/@v/{version}.zip` | Module source zip archive |
| `/v1/index.json` | govulndb — all known vulnerability IDs |
| `/v1/ID/{id}.json` | govulndb — full OSV record for one vulnerability |
| `/v1/query` | govulndb — batch query by module/version |

Module paths may contain slashes (e.g. `golang.org/x/text`). Uppercase-encoded paths (`!{lowercase}` convention) are passed through to the upstream unchanged.

> **Caching note:** `@latest` and `@v/list` responses are cached permanently after the first request, just like other artifacts. They may become stale if new versions are published. Clear the proxy storage (or configure a shorter `metadata_ttl_secs`) to pick up new versions immediately.

The optional `vuln_db_url` field controls which govulndb upstream is used (default: `https://vuln.go.dev`). Set it to `""` to disable the `/v1/` endpoints entirely.

The optional `sumdb_url` field controls the **checksum database** proxy (default: `https://sum.golang.org`). This is the other half of the GOPROXY protocol: without it the go tool still opens a direct connection to `sum.golang.org` for every module it has not seen, so the proxy has moved the egress rather than removed it — and an air-gapped estate fails closed. Responses are cached, which is what makes the offline case work; caching is sound because the log is signed, so a cached record is exactly as trustworthy as a live one. Set it to `""` for a registry serving only private modules, where a lookup would publish private module paths to a public transparency log.

Configure the go toolchain to use the proxy:

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="http://batlehub.example.com/proxy/go,direct"
export GOVULNDB="http://batlehub.example.com/proxy/go"
```

---

**`maven`** — proxies Maven artifact repositories. Supports `GET` requests for POM files, JARs, source JARs, Javadoc JARs, SHA-1/MD5 checksums, and Maven metadata XML. Compatible with Maven, Gradle, and any tool that speaks the Maven repository protocol. Default upstream: `https://repo1.maven.org/maven2`. Set `mode = "local"` or `mode = "hybrid"` to enable private publishing — see [registry modes](#registry-modes) and [Worked Example 6.12](/guide/configuration-examples#612-private-maven-registry-local--hybrid-mode).

Configure Maven to use the proxy:

```xml
<!-- ~/.m2/settings.xml -->
<settings>
  <mirrors>
    <mirror>
      <id>batlehub</id>
      <mirrorOf>central</mirrorOf>
      <url>http://batlehub.example.com/proxy/maven/maven2/</url>
    </mirror>
  </mirrors>
</settings>
```

Configure Gradle to use the proxy:

```kotlin
// settings.gradle.kts
dependencyResolutionManagement {
    repositories {
        maven { url = uri("http://batlehub.example.com/proxy/maven/maven2/") }
    }
}
```

---

**`terraform`** — proxies the Terraform provider and module registry protocol. Supports provider version listing, provider download info (binary URL + checksums), module version listing, and module source download. Default upstream: `https://registry.terraform.io`. Set `mode = "local"` or `mode = "hybrid"` to enable private module and provider publishing — see [registry modes](#registry-modes) and [Worked Example 6.13](/guide/configuration-examples#613-private-terraform-registry-local--hybrid-mode).

| Endpoint | Method | Description |
|---|---|---|
| `/v1/providers/{namespace}/{type}/versions` | GET | Provider version list (JSON, cached) |
| `/v1/providers/{namespace}/{type}/{version}/download/{os}/{arch}` | GET | Provider download info JSON (cached; local: rewritten to `/artifact` URL) |
| `/v1/providers/{namespace}/{type}/versions` | POST | **Local/Hybrid:** publish provider version manifest |
| `/v1/providers/{namespace}/{type}/{version}/artifact/{os}/{arch}` | PUT | **Local/Hybrid:** upload provider binary zip |
| `/v1/providers/{namespace}/{type}/{version}/artifact/{os}/{arch}` | GET | **Local/Hybrid:** serve provider binary zip |
| `/v1/modules/{namespace}/{name}/{provider}/versions` | GET | Module version list (JSON, cached) |
| `/v1/modules/{namespace}/{name}/{provider}/{version}/download` | GET | Module source redirect (`204 + X-Terraform-Get`; local: points at `/artifact`) |
| `/v1/modules/{namespace}/{name}/{provider}/{version}` | POST | **Local/Hybrid:** upload module tar.gz |
| `/v1/modules/{namespace}/{name}/{provider}/{version}/artifact` | GET | **Local/Hybrid:** serve module tar.gz |

> **Module download in proxy mode:** passes through the upstream `204 + X-Terraform-Get` header without caching. In Local/Hybrid mode the header is rewritten to point at the local `/artifact` endpoint.

Configure the Terraform CLI to use the proxy for providers:

```hcl
# ~/.terraformrc  (or %APPDATA%/terraform.rc on Windows)
provider_installation {
  network_mirror {
    url = "http://batlehub.example.com/proxy/terraform/"
  }
}
```

---

**`nuget`** — implements the [NuGet v3 API](https://learn.microsoft.com/en-us/nuget/api/overview) for .NET package management. The v3 service index (`index.json`) is synthesised by BatleHub and points all resource URLs back at the proxy. Default upstream: `https://api.nuget.org`. Vulnerability data for `dotnet list package --vulnerable` is proxied automatically — see [Vulnerability Proxy](/use/vulnerability-proxy#_3-nuget-—-dotnet-list-package-vulnerable).

| Endpoint | Method | Description |
|---|---|---|
| `/proxy/{registry}/nuget/v3/index.json` | GET | NuGet v3 service index |
| `/proxy/{registry}/nuget/v3/registration5/{id}/index.json` | GET | Package registration (all versions + metadata) |
| `/proxy/{registry}/nuget/v3/flat/{id}/index.json` | GET | Flat container version list |
| `/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}` | GET | Package content download (`.nupkg`, `.nuspec`) |
| `/proxy/{registry}/nuget/v3/query` | GET | Package search |
| `/proxy/{registry}/nuget/v3/vulnerabilities/index.json` | GET | Vulnerability catalogue index |
| `/proxy/{registry}/nuget/v3/vulnerabilities/page/{page}` | GET | Vulnerability catalogue page |
| `/proxy/{registry}/nuget/api/v2/package` | PUT | Publish `.nupkg` |
| `/proxy/{registry}/nuget/v2/package/{id}/{version}` | DELETE | Yank a version |

Configure NuGet in `nuget.config`:

```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="batlehub" value="https://batlehub.example.com/proxy/nuget/nuget/v3/index.json" />
  </packageSources>
  <packageSourceCredentials>
    <batlehub>
      <add key="Username" value="user" />
      <add key="ClearTextPassword" value="<token>" />
    </batlehub>
  </packageSourceCredentials>
</configuration>
```

---

**`composer`** — implements the [Packagist v2 protocol](https://packagist.org/apidoc) for PHP Composer. Serves `packages.json` (repository root index), `p2/{vendor}/{package}.json` (metadata), and `dist/{vendor}/{package}/{version}` (ZIP artifact downloads). Default upstream: `https://packagist.org`. `composer audit` is proxied automatically — see [Vulnerability Proxy](/use/vulnerability-proxy#_4-composer-—-composer-audit). Set `mode = "local"` or `mode = "hybrid"` to enable private package publishing — see [registry modes](#registry-modes) and [Worked Example 6.15](/guide/configuration-examples#615-private-composer-registry-local--hybrid-mode).

| Endpoint | Method | Description |
|---|---|---|
| `/proxy/{registry}/packages.json` | GET | Repository root index (lists all known package names) |
| `/proxy/{registry}/p2/{vendor}/{package}.json` | GET | Package metadata (all versions, dist URLs) |
| `/proxy/{registry}/p2/{vendor}/{package}~dev.json` | GET | Dev-stability metadata variant |
| `/proxy/{registry}/dist/{vendor}/{package}/{version}` | GET | Download ZIP artifact |
| `/proxy/{registry}/api/security-advisories/` | GET | Security advisory query (`composer audit`) |
| `/proxy/{registry}/api/upload` | POST | **Local/Hybrid:** publish a package (multipart or raw ZIP body) |
| `/proxy/{registry}/api/packages/{vendor}/{package}/versions/{version}` | DELETE | **Local/Hybrid:** yank a version |

---

**`pypi`** — implements the [Python Simple Repository API (PEP 503 / PEP 691)](https://peps.python.org/pep-0503/) and [PyPI JSON API](https://docs.pypi.org/api/json/). Download URLs in Simple index pages are rewritten to route through the proxy cache. Default upstream: `https://pypi.org`. Set `mode = "local"` or `mode = "hybrid"` to enable private publishing via `twine upload` — see [registry modes](#registry-modes).

| Endpoint | Method | Description |
|---|---|---|
| `/proxy/{registry}/simple/` | GET | Root index (all project names) |
| `/proxy/{registry}/simple/{package}/` | GET | Per-package file listing (HTML or JSON via `Accept` header) |
| `/proxy/{registry}/packages/{filename}` | GET | Download wheel or sdist (cached) |
| `/proxy/{registry}/legacy/` | POST | **Local/Hybrid:** twine-compatible multipart publish |

Configure pip:

```ini
# ~/.pip/pip.conf
[global]
index-url = http://batlehub.example.com/proxy/my-pypi/simple/
```

---

**`conda`** — proxies a single conda channel (e.g. `conda-forge`) across all platforms. Caches `repodata.json` and package files per platform. In hybrid mode, locally published packages are merged into the upstream `repodata.json`. Default upstream: `https://conda.anaconda.org`. Set `mode = "local"` or `mode = "hybrid"` to enable private publishing — see [registry modes](#registry-modes).

| Endpoint | Method | Description |
|---|---|---|
| `/proxy/{registry}/{platform}/repodata.json` | GET | Channel index for a platform (e.g. `linux-64`, `noarch`) |
| `/proxy/{registry}/{platform}/current_repodata.json` | GET | Reduced index (proxy mode only) |
| `/proxy/{registry}/{platform}/{filename}` | GET | Download `.conda` or `.tar.bz2` package |
| `/proxy/{registry}/{platform}/` | POST | **Local/Hybrid:** publish a conda package |

Configure conda:

```yaml
# ~/.condarc
channels:
  - http://batlehub.example.com/proxy/my-conda
  - nodefaults
```

Configure Composer to use the proxy by adding a repository entry in `composer.json`:

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "http://batlehub.example.com/proxy/packagist/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer <your-token>"]
        }
      }
    }
  ]
}
```

Or store credentials in `auth.json` (never commit this file):

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "user",
      "password": "<your-token>"
    }
  }
}
```

---

**`deb`** — proxies and hosts Debian/Ubuntu APT repositories. In proxy mode the upstream `Release`/`InRelease` file and its existing signature are relayed unchanged; clients verify against the **upstream's** archive key. In local/hybrid mode BatleHub generates and (optionally) signs `Packages` and `Release` with an Ed25519 OpenPGP key (`repo_signing`). Default upstream: `https://deb.debian.org`.

| Endpoint | Method | Description |
|---|---|---|
| `/proxy/{registry}/deb/dists/{dist}/{component}/binary-{arch}/Packages` | GET | Package index (plain text) |
| `/proxy/{registry}/deb/dists/{dist}/{component}/binary-{arch}/Packages.gz` | GET | Package index (gzip) |
| `/proxy/{registry}/deb/dists/{dist}/Release` | GET | Release metadata |
| `/proxy/{registry}/deb/dists/{dist}/InRelease` | GET | Inline-signed release metadata |
| `/proxy/{registry}/deb/pool/{dist}/{component}/{filename}` | GET | Download `.deb` package |
| `/proxy/{registry}/deb/pool/{dist}/{component}/upload` | PUT | **Local/Hybrid:** publish a `.deb` |
| `/proxy/{registry}/deb/key.gpg` | GET | **Local/Hybrid:** signing public key (ASCII-armored) |

**Client setup — proxy mode** (relays upstream signature; trust the upstream archive key):

```sh
# Official Debian/Ubuntu mirrors — key is already in the keyring package
KEYRING=/usr/share/keyrings/debian-archive-keyring.gpg  # ubuntu: ubuntu-archive-keyring.gpg
echo "deb [signed-by=$KEYRING] http://batlehub.example.com/proxy/my-deb stable main" \
  | sudo tee /etc/apt/sources.list.d/my-deb.list

# Third-party upstream — import its key first:
# curl -fsSL <upstream-key-url> | gpg --dearmor \
#   | sudo tee /usr/share/keyrings/my-deb.gpg >/dev/null

sudo apt update
```

**Client setup — local/hybrid mode** (BatleHub signs `Release`; import BatleHub's key):

```sh
# Import BatleHub's signing key
curl -fsSL http://batlehub.example.com/proxy/my-deb/deb/key.gpg \
  | sudo tee /usr/share/keyrings/my-deb.asc >/dev/null

# Add the source (adjust suite/component to match your repo)
echo "deb [signed-by=/usr/share/keyrings/my-deb.asc] \
  http://batlehub.example.com/proxy/my-deb/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/my-deb.list

sudo apt update
```

For an unsigned local repo (no `repo_signing` key configured), replace `[signed-by=…]` with `[trusted=yes]`.

**Private registry authentication:**

APT reads credentials from `/etc/apt/auth.conf.d/` (Debian 9+ / Ubuntu 19.04+). The `sources.list` entry stays unchanged — credentials are kept in a separate file.

```sh
sudo tee /etc/apt/auth.conf.d/batlehub.conf > /dev/null <<'EOF'
machine batlehub.example.com
login <your-username>
password <your-token>
EOF
sudo chmod 0600 /etc/apt/auth.conf.d/batlehub.conf

sudo apt update
```

On older systems without `auth.conf.d` support, use `/etc/apt/auth.conf` with the same `machine / login / password` stanza.

Alternatively, embed the credentials directly in the URL (less secure — visible in `apt-cache policy` output):

```sh
echo "deb [signed-by=…] https://<user>:<token>@batlehub.example.com/proxy/my-deb/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/my-deb.list
```

**Publish a `.deb` (local/hybrid mode):**

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  --data-binary @hello_1.0_amd64.deb \
  http://batlehub.example.com/proxy/my-deb/deb/pool/stable/main/upload
```

---

**`rpm`** — proxies and hosts RPM repositories for DNF/YUM. In proxy mode the upstream `repomd.xml` (and any `repomd.xml.asc` signature) is relayed; clients verify against the **upstream's** GPG key. In local/hybrid mode BatleHub regenerates `repodata/` and optionally signs `repomd.xml` with an Ed25519 OpenPGP key (`repo_signing`). Default upstream: `https://dl.fedoraproject.org/pub/fedora/linux/releases`.

| Endpoint | Method | Description |
|---|---|---|
| `/proxy/{registry}/rpm/repodata/repomd.xml` | GET | Repository metadata index |
| `/proxy/{registry}/rpm/repodata/repomd.xml.asc` | GET | **Local/Hybrid (signed):** detached OpenPGP signature |
| `/proxy/{registry}/rpm/repodata/repomd.xml.key` | GET | **Local/Hybrid (signed):** signing public key (ASCII-armored) |
| `/proxy/{registry}/rpm/repodata/{filename}` | GET | Other repodata files (primary.xml.gz, filelists.xml.gz, …) |
| `/proxy/{registry}/rpm/{path}` | GET | Download `.rpm` package |
| `/proxy/{registry}/rpm/upload` | PUT | **Local/Hybrid:** publish an `.rpm` |

**Client setup — `.repo` file** (`/etc/yum.repos.d/<name>.repo`):

```ini
[my-rpm]
name=My RPM Registry
baseurl=http://batlehub.example.com/proxy/my-rpm/rpm
enabled=1
repo_gpgcheck=0   # set to 1 and add gpgkey= for a signed repo
gpgcheck=0
```

For a signed local/hybrid repo (BatleHub `repo_signing` key configured):

```ini
[my-rpm]
name=My RPM Registry
baseurl=http://batlehub.example.com/proxy/my-rpm/rpm
enabled=1
repo_gpgcheck=1
gpgcheck=0
gpgkey=http://batlehub.example.com/proxy/my-rpm/rpm/repodata/repomd.xml.key
```

For a proxy repo whose upstream signs metadata, point `gpgkey` at the **upstream** project's key.

**Private registry authentication:**

DNF/YUM reads `username` and `password` directly from the `.repo` file:

```ini
[my-rpm]
name=My RPM Registry
baseurl=http://batlehub.example.com/proxy/my-rpm/rpm
enabled=1
repo_gpgcheck=0
gpgcheck=0
username=<your-username>
password=<your-token>
```

Alternatively, use `~/.netrc` (DNF and libcurl honour it for HTTP Basic Auth):

```
machine batlehub.example.com
login <your-username>
password <your-token>
```

**Publish an `.rpm` (local/hybrid mode):**

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  --data-binary @hello-1.0-1.x86_64.rpm \
  http://batlehub.example.com/proxy/my-rpm/rpm/upload
```

---

**`generic`** — a path-addressed mirror of any plain HTTP file tree, for upstreams that have no package protocol at all: toolchain tarballs (`nodejs.org/dist`, `static.rust-lang.org`, `dl.google.com/go`) and single-binary vendor CDNs (`get.helm.sh`, `dl.min.io`, `binaries.sonarsource.com`). Proxy-only — there is no publish, index or signing model. A request to `/proxy/{registry}/generic/{path}` streams `{upstream}/{path}` and caches it on the first miss.

Two fields are **mandatory** for this type:

- `upstreams` — there is no default file tree to fall back to.
- `path_allow` — the request path is passed straight through to the upstream, so without an allowlist a registry pointed at a host that serves many unrelated tenants (a shared bucket, a multi-vendor CDN) would relay *every* path on that host. Patterns use [`glob`](https://docs.rs/glob) semantics, where `*` also crosses `/`. Paths outside the allowlist are rejected with `403` before any upstream request is made — which also means cache warming (`warm_paths`) cannot bypass it.

```toml
[[registries]]
type = "generic"
name = "node-dist"
mode = "proxy"
upstreams = ["https://nodejs.org/dist"]
# Verified against `mise install node`: mise fetches both the platform
# tarball and the `node-v<ver>.tar.gz` source tarball, so a platform-only
# glob 403s mid-install.
path_allow = ["v*/**"]

[registries.rbac]
anonymous = ["releases:read"]

# Pre-warm specific paths (path-addressed registries use `warm_paths`,
# not `warm_packages`).
[registries.cache]
warm_paths = ["v24.18.0/node-v24.18.0-linux-x64.tar.gz"]
```

Point the client at it with the toolchain's own mirror variable:

```sh
export NODEJS_ORG_MIRROR=https://batlehub.example.com/proxy/node-dist/generic
export RUSTUP_DIST_SERVER=https://batlehub.example.com/proxy/rust-dist/generic
```

`batlehub-cli registry suggest` scans a project (including `mise.toml` / `mise.lock`) and prints both the `[[registries]]` blocks and the matching client environment variables — see the [CLI Reference](/guide/server-cli).

**Artifact size:** mirrored archives are often large and the proxy buffers an artifact before caching it, so raise `limits.max_artifact_size_bytes` (default 500 MiB) when mirroring toolchains or IDE-sized downloads.

---

**`[registries.cache]` fields:**

| Field | Type | Default | Notes |
|---|---|---|---|
| `metadata_ttl_secs` | u64 | `300` | How long release metadata (version lists, release info) is cached in seconds |
| `serve_stale` | bool | `true` | When `true`, serve stale metadata if the upstream returns a transient error (5xx). Keeps the registry usable during upstream outages. |
| `artifact_ttl_secs` | u64? | — | Evict artifacts older than this many seconds. Omit to never expire by age. |
| `idle_days` | u64? | — | Evict artifacts not accessed for this many days. Omit to disable idle eviction. |
| `max_size_bytes` | u64? | — | Storage cap in bytes. When exceeded, the least-recently-used artifacts are removed until usage falls below the cap. Omit for no size limit. |
| `keep_latest_n` | usize? | — | Keep only the N most-recently-cached versions per package. Older versions are evicted when a new one is stored. Omit to keep all versions. |
| `warm_packages` | string[] | `[]` | Packages to pre-fetch at startup and via the admin warm endpoint. Each entry is a bare name (`"lodash"`) or a pinned version (`"lodash@4.17.21"`). |
| `warm_latest_n` | usize | `1` | Number of most-recent versions to warm per bare package name. Pinned-version entries always warm exactly one version. |
| `warm_concurrency` | usize | `2` | Maximum concurrent artifact downloads during a warming run. |

**Eviction example:**

```toml
[registries.cache]
metadata_ttl_secs = 600
artifact_ttl_secs = 2592000   # 30 days
idle_days         = 14
max_size_bytes    = 10737418240  # 10 GiB
keep_latest_n     = 5
```

**Cache warming example:**

```toml
[registries.cache]
warm_packages    = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n    = 3      # warm the 3 most recent versions of bare-name packages
warm_concurrency = 4      # up to 4 parallel downloads
```

At startup, BatleHub pre-fetches the listed packages so they are available with zero latency on first request. The same packages can be re-warmed at any time via the admin API:

```sh
# Warm all configured versions of lodash
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash"}'

# Override the version count for this call only
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash", "versions": 10}'
```

> **Registry support:** version enumeration (used by bare-name warming) is implemented for **npm**, **Cargo**, **OpenVSX**, and **Go** modules. For GitHub and VS Code Marketplace, pass a pinned version string (e.g. `"owner/repo@v1.2.3"`) to warm a specific version.

**JetBrains Marketplace:** plugins warm like any other package — an entry is the plugin `xmlId`, bare or pinned:

```toml
[[registries]]
name = "jetbrains-plugins"
type = "jetbrains-marketplace"

[registries.cache]
warm_packages = ["org.rust.lang", "com.intellij.ml.llm@2026.1.1"]
warm_latest_n = 2
```

Bare names enumerate versions through `/plugins/list`, which only lists the **Stable** channel — plugins on an EAP/nightly channel are not pre-fetched (their download path carries a `channel` parameter). The warmed archive is the one the IDE downloads from `plugin/download?pluginId=…&version=…`, so it is served from cache on the first request, including while plugins.jetbrains.com is unreachable.

**Cross-replica warm-up coordination (Redis):**

When `[cache] type = "redis"` is configured and the `cache-redis` feature is compiled in, BatleHub automatically coordinates cache warming across replicas. Before downloading an artifact, each replica attempts to acquire a short-lived Redis lock (`SET batlehub:warm:{key} 1 NX PX 600000`). Only the first replica to acquire the lock performs the upstream download; the others skip that artifact. This prevents thundering-herd downloads when multiple replicas restart simultaneously and discover the same cold-cache misses. No additional configuration is required — the coordination is enabled automatically whenever the Redis cache backend is selected. With non-Redis backends (`memory` or `postgres`), each replica warms independently (safe but redundant).

**Content-addressable deduplication:**

BatleHub stores physical artifact bytes at a content-addressed key (`blob/{sha256}`) and maps logical artifact keys to that blob via a reference count. When the same bytes are referenced by multiple logical keys (e.g. the same package mirrored across two registries, or a yanked-then-re-released version), only one copy of the data is stored on disk or in S3. The deduplication tables (`artifact_dedup_index`, `artifact_dedup_refs`) are created automatically by the database migration and require no configuration.

**`[registries.rbac]` fields:**

| Field | Type | Default | Notes |
|---|---|---|---|
| `anonymous` | string[] | `[]` | Permissions granted to unauthenticated requests |
| `user` | string[] | `[]` | Permissions granted to authenticated users (inherits anonymous perms) |
| `admin` | string[] | `[]` | Permissions granted to admins (inherits user and anonymous perms) |
| `groups` | map | `{}` | Dynamic group permissions (see [Section 4](#_4-permissions-reference)) |

**`[registries.refs]` — Git-forge ref resolution (`github`, `gitlab`, `forgejo` only; RFC 0019):**

| Field | Type | Default | Notes |
|---|---|---|---|
| `branch_ttl_secs` | u64 | `60` | How long a branch → commit resolution is trusted before the forge is asked again. Below `10` is a config error: re-resolving on every request is a rate-limit self-DoS. |
| `tag_ttl_secs` | u64 | `3600` | How long a tag → commit resolution is trusted. Also the latency with which a moved tag is noticed. |
| `mutable_refs` | string | `"warn"` | What following a branch does. `warn` serves it and says so; `deny` refuses every mutable coordinate, which is what a registry that must be reproducible wants. |
| `tag_moved` | string | `"deny"` | What a tag that now resolves to a different commit does — and a release asset whose digest changed. Denied by default: these are the two forge-native ways to swap bytes under a stable coordinate. `warn` serves and reports. |

> Every archive (`tarball/{ref}`, `zipball/{ref}`) and raw file is resolved to a commit before it is fetched, and cached under that commit — `main` today and `main` tomorrow are two entries. Every forge response carries `X-BatleHub-Ref-Kind` (`commit`, `tag` or `branch`), `X-BatleHub-Resolved-Commit`, and `X-BatleHub-Ref-Previous-Commit` when the ref moved. A forge registry with no `[registries.upstream_auth]` raises the `forge.anonymous-upstream` warning: anonymous GitHub allows 60 API requests an hour, and ref resolution spends one or two per new ref.
>
> With `[registries.security]` these three facts ride the version's verdict, so a warned branch answers `X-BatleHub-Verdict: warned` and `batlehub why` explains it. Without it there is no verdict to carry them: a `deny` is a plain `403` naming the code, and a `warn` is the headers above.

**`[registries.raw]` — Raw file serving (forge kinds only; RFC 0019):** {#registries-raw}

**Off unless written.** Raw content used to be served implicitly on all three
forges; a registry with no `[registries.raw]` block now refuses it, and the
refusal names this section. Every forge registry without it raises
`forge.raw-disabled-but-linked`, because the setup snippet the registry
generates rewrites the forge's raw host at a path that refuses.

```toml
[registries.raw]
enabled        = true
max_size_bytes = 10485760
repos          = ["cli/*"]
require_pinned = false
scripts        = "warn"
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Serve raw files at all. |
| `max_size_bytes` | u64 | `10485760` | Ceiling on one raw file; the stream stops there, so the file is refused rather than truncated. `0` with `enabled` is a config error, and a value above `[limits].max_artifact_size_bytes` is refused because the global ceiling would silently win. |
| `repos` | string[] | `[]` | `owner/repo` globs (`cli/*`). Empty allows any repository; the list narrows and never widens. A malformed entry is a config error. |
| `require_pinned` | bool | `false` | Refuse a branch ref (`PINNED_REF_REQUIRED`): raw content that changes under the same URL is what a pinned estate does not want. |
| `scripts` | string | *see below* | `warn`, `deny` or `ignore` for shell, PowerShell, Python and batch payloads (`RAW_SCRIPT`). **Absent means `deny` when the registry has `[registries.security]`** and `warn` otherwise. |

> `scripts` looks at the file's extension always, and under `deny` at its first
> bytes too — so an extensionless payload beginning with a shebang is refused
> as well. The default of `deny` under a security profile is RFC 0019 §11 q2:
> opting into a quarantine is opting into "nothing unscanned is served", and a
> single script file is the one artifact none of the scanners reads.

**`[registries.api_reads]` — Typed read-only JSON routes (forge kinds only; RFC 0019):** {#registries-api-reads}

```toml
[registries.api_reads]
families = ["tags", "commits", "branches"]
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `families` | string[] | `[]` | Any of `tags`, `commits`, `branches`. Anything else is a config error — `contents` and `git/blobs` are raw content by another door, and `[registries.raw]` is where that decision lives. |

> Each family adds one `GET` under `/proxy/<registry>/<owner>/<repo>/`, answering
> BatleHub's own shape rather than the forge's: no upstream URL to follow, no
> field that means something different per forge. A family the registry did not
> ask for answers `404`. Separately and always, the **release documents** —
> the listing and the release by tag, on all three forges — have their
> `tarball_url`, `zipball_url` and asset download URLs repointed at this proxy,
> so a client that reads the document instead of building a path stays behind
> the policy, the cache and the audit trail.

**`[registries.security]` — Quarantine and verdicts (optional):** {#registries-security}

Opts the registry into the supply-chain layer of RFC 0018. Every version this
registry serves then carries a **verdict** — `allowed`, `warned`,
`quarantined` or `denied` — computed from its age, the scanners' findings,
the operator's blocks and any SOC verdict. A version whose verdict is not
served is refused on the download path, with its reason codes, until the
verdict changes. A registry without the section is untouched.

```toml
[registries.security]
mode                   = "block"     # "block" | "warn"
min_age_secs           = 259200      # never served below this age; floor 3600
mature_age_secs        = 2592000     # served `warned` while a scan is pending above this age
hold_missing_timestamp = true        # hold a version the upstream did not date
scanners               = ["osv"]     # names from [scanners]; "osv" needs no declaration
required_scanners      = ["osv"]     # all must answer before the version is served
max_severity           = "high"      # findings at or above this deny (block) or warn
require_provenance     = false
deny_install_hooks     = "warn"      # "deny" | "warn" | "ignore"
scanner_error          = "quarantine" # "quarantine" | "warn" | "ignore"

pullers_window_days    = 30          # how far back a flip alert names who pulled the version

[registries.security.rescan]         # the rescan timer (RFC 0018 phase 4)
interval_secs = 0                    # > 0: every verdict older than this is scanned again
on_webhook    = true
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `mode` | string | `"block"` | `block` refuses a version whose verdict is `quarantined` or `denied`; `warn` serves it with the verdict visible. `BLOCK_LIST` and `SOC_VERDICT` deny in both modes. |
| `min_age_secs` | u64 | `86400` | Below this age a version is held (`MIN_AGE_NOT_MET`) whatever the scanners say. Below `3600` is a config error: an hour is the point of the quarantine. |
| `mature_age_secs` | u64 | `86400` | Above this age a version whose scan has not returned is served `warned` (`SCAN_PENDING`) and scanned behind the request. `0` never serves unscanned. Must be at least `min_age_secs`. With both at their defaults the scan-hold window is empty — the recommended production profile is 3 days / 30 days. |
| `hold_missing_timestamp` | bool | `true` | Hold a version the upstream did not date (`TIMESTAMP_MISSING`, open-ended: no `available_at`, and the maturity bypass does not reach it). `false` skips the age gate for it, as `release_age_gate` does by default. On the path-proxy kinds (`deb`, `rpm`, `pacman`, `generic`, `jetbrains`) no version is dated, so `true` holds everything and raises `security.timestamp-hold-unavailable`. |
| `scanners` | string[] | `["osv"]` | Which scanners the worker runs on this registry. Each name is a `[scanners.<name>]` entry; `osv` is implicit. Every type the RFC names is built: `osv`, `postmortem`, `guarddog`, `trivy`, `sigstore`, and the two external services `socket` (Socket.dev, one call per coordinate, needs `api_key`) and `mlab` (mlab.sh's CVE API, an *enrichment*: it attaches CVSS, EPSS and CISA KEV to the vulnerability findings the others produced and raises a KEV-listed CVE to `critical`; it never creates a finding, so listing it under `required_scanners` warns). A scanner answers under its config key, so a second `osv` pointed at another `api_url` is its own name. |
| `required_scanners` | string[] | `["osv"]` | Must all have answered before the version is served. Must be a subset of `scanners`. Empty with `mode = "warn"` raises `security.unprotected`: nothing can ever hold a version. |
| `max_severity` | string | `"high"` | `low`, `medium`, `high` or `critical`. A finding at or above it produces `denied` in `block` mode and `warned` in `warn` mode. |
| `require_provenance` | bool | `false` | A version without a provenance attestation is `PROVENANCE_MISSING` (a finding at `high`). Only meaningful with a scanner that checks provenance (`sigstore`, phase 3). |
| `deny_install_hooks` | string | `"warn"` | What an install hook (npm `preinstall`, a Python `setup.py`) is: `deny`, `warn` or `ignore`. Read by the archive scanners of phase 3. |
| `scanner_error` | string | `"quarantine"` | A scanner that cannot answer after `[worker].max_attempts`: `quarantine` holds the version (`SCANNER_ERROR`, time-bound), `warn` serves it warned, `ignore` drops the finding. |
| `pullers_window_days` | u32 | `30` | When a rescan moves a *served* version to `denied`, the `verdict_changed` notification names every identity that pulled it inside this window, read from the access log (RFC 0018 decision 23); it is also the default window of `GET /api/v1/verdicts/{registry}/{name}/{version}/pullers` and `batlehub verdicts pullers`. Anonymous pulls are kept under `ip:<addr>`. |
| `rescan.interval_secs` | u64 | `0` | `> 0`: the rescan scheduler — one per estate, elected with a PostgreSQL advisory lock — queues a `Rescan` (below `FirstSeen` and `Webhook`, above `Backfill`) for every verdict of this registry whose last scan is older than the interval. A verdict that flips from served to `denied` raises the alert above; a hold that lifts raises `artifact_released` to the identities that were refused it. `0` never rescans on a clock; `POST …/rescan` and the `security.rescan` webhook still do. |

> **Where the rules go.** `min_age_secs` *replaces* a `release_age_gate` rule
> on this registry — declaring both is a config error. The `cve_gate`,
> `license_gate`, `require_signed_release` and `trusted_publisher` rules, and
> the administrator's block list, are no longer run as rules on this registry:
> they run as internal scanners whose denial becomes a finding
> (`VULNERABILITY`, `LICENSE_DENIED`, `SIGNATURE_MISSING`,
> `UNTRUSTED_PUBLISHER`, `BLOCK_LIST`), so there is one decision per version
> and no rule that can fail open beside it. `deny_latest` and `version_gate`
> stay in the chain; they judge the request, not the artifact.
>
> **Who sees why.** A refused download is a plain `403` for everyone; the
> reason codes and the `batlehub why` hint in the body need `quarantine:read`
> (granted to `user` and `admin` by default) and the findings behind them
> need `findings:read` (`admin`). An operator override is a `GateExemption`
> on the gate `security_verdict` (`gates:exempt`): it turns a hold into
> `warned`, never into `allowed`, so it stays visible.
>
> **What must also be true.** Every `[[notifications.inbound]]` webhook must
> carry a `secret` once any registry has this section — a `security.*` event
> on an unsigned webhook would let anyone on the network deny packages. And
> a process with `worker` in its roles must exist somewhere: a proxy that
> queues jobs nobody dequeues holds every new version until
> `mature_age_secs`, and logs a warning at startup when it is alone.

**`[[registries.rules]]` — Release age gate:**

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"release_age_gate"` |
| `min_age_secs` | u64 | `3600` | Releases younger than this are blocked. |
| `bypass_roles` | string[] | `[]` | Roles that skip the gate entirely, including the missing-timestamp check (e.g. `["admin"]`). |
| `deny_missing_timestamp` | bool | `false` | When `true`, deny downloads for packages whose upstream provides no publish timestamp, instead of skipping the check and allowing the download. Useful for registries like conda where the timestamp field is optional — setting this to `true` ensures every package carries a verifiable age. |

> **Timestamp support by registry type:** The gate is only enforced when the upstream provides a publish timestamp.
> - **npm**, **Cargo**, **OpenVSX**, **VS Code Marketplace**, **Go**, **PyPI** — timestamp always populated; gate is fully enforced.
> - **GitHub** — timestamp populated only for specific-tag release requests (asset downloads). Raw files, source tarballs, and release listings return no timestamp; the gate is skipped for those requests.
> - **Conda** — timestamp is the `timestamp` field (milliseconds since epoch) in `repodata.json`. Most packages carry it, but older or third-party packages may omit it. Use `deny_missing_timestamp = true` to reject packages without a verifiable build date.
> - **Terraform providers** — timestamp populated by `registry.terraform.io` but not mandated by the official spec; other Terraform registries may omit it.
> - **Node distributions (`nodedist`)** — the release date is read from `index.tab`, so current releases carry a timestamp; a release the index no longer lists reaches the gate with none. On this kind `deny_missing_timestamp` is **mandatory**: a `release_age_gate` rule without it is a config error, because the field decides the gate for every de-listed release and neither answer is a default this server picks for you (RFC 0010 §6.7). `true` refuses de-listed releases, `false` serves them.
> - **SDKMAN (`sdkman`)** — the protocol publishes no dates at all, so every artifact reaches the gate with none and `deny_missing_timestamp` *is* the rule: `true` refuses every download on the registry, `false` makes the gate inert. Mandatory here for the same reason, and a `[registries.security]` block holds on a missing timestamp by default instead.

**`[[registries.rules]]` — Require signed release:**

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"require_signed_release"` |
| `enabled` | bool | `false` | When true, blocks releases with no signature signal (subject to `deny_missing_signature` below) |
| `bypass_roles` | string[] | `[]` | Roles that skip the gate entirely (e.g. `["admin"]`). |
| `deny_missing_signature` | bool | `false` | When `true`, deny releases from registries that report no signature signal at all, instead of skipping the check and allowing the download. |

> This checks `PackageMetadata.is_signed`, a best-effort signal populated per registry adapter — not full cryptographic signature verification. GitHub, Forgejo, GitLab, OpenVSX, and VS Code Marketplace populate it (presence of a `.asc`/`.sig` release asset or an extension signature blob); registries with no signing concept in their ecosystem (npm, PyPI, crates.io, Maven, RubyGems, Conda, Composer, Go, Terraform, NuGet, deb/rpm/pacman) report `None` and are allowed through unless `deny_missing_signature = true`.
>
> **On a `local` or `hybrid` registry, pair this with `[registries.signing] required = true`.** The paragraph above describes *proxied* artifacts. A **locally published** version is different: it is recorded as unsigned — not unknown — whenever the publish request carried no `X-Artifact-Signature` header, and this rule denies unsigned outright. `deny_missing_signature` does not soften that; it governs only the unknown case. So enabling this rule to gate the proxied half of a hybrid registry also makes every unsigned local publish fail **at download time**, with the `403` reaching the consumer rather than the publisher who could have fixed it. `signing.required = true` refuses the same artifact at the publish request instead. Configuring one without the other raises `require-signed-release.unsigned-publishes` on the **Config Reload** admin page and at `GET /api/v1/admin/config/warnings`.

**`[[registries.rules]]` — Deny latest:**

Rejects any request that uses `"latest"` as the version tag, forcing consumers to pin explicit versions (supply-chain hygiene).

```toml
[[registries.rules]]
kind = "deny_latest"
bypass_roles = ["admin"]   # omit or leave empty for a hard block
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"deny_latest"` |
| `bypass_roles` | string[] | `[]` | Roles that may still request `"latest"` (e.g. `["admin"]`). When multiple roles are listed the least-privileged one sets the access floor. When empty, the block applies to all roles. |

> This rule applies to all registry types. `"latest"` is the literal version string sent by the client — for npm it maps to the `latest` dist-tag, for Cargo and Go it triggers upstream `@latest` resolution, and for OpenVSX and VS Code Marketplace it fetches the current published version.

**`[[registries.rules]]` — Version gate:**

Gates downloads by version using an optional approved-version allowlist plus a blocklist of specific versions with known issues. The resolved version is matched against both lists: a `block` match is always rejected, and when `allow` is non-empty a version matching **none** of its entries is also rejected. `block` takes precedence over `allow`.

```toml
[[registries.rules]]
kind = "version_gate"
allow = [">=1.2.0, <2.0.0"]   # optional: when set, only matching versions are served
block = ["1.4.7", "1.5.0"]    # specific versions with known issues
bypass_roles = ["admin"]
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"version_gate"` |
| `allow` | string[] | `[]` | Approved-version allowlist. When non-empty, a version matching none of these entries is rejected. When empty, all versions are allowed (subject to `block`). |
| `block` | string[] | `[]` | Blocklist of specific versions (or ranges) with known issues. A match is always rejected. |
| `bypass_roles` | string[] | `[]` | Roles that may bypass the gate (e.g. `["admin"]`). When multiple are listed the least-privileged one sets the access floor. When empty, the gate applies to all roles. |

> **Matching:** each entry is treated as a semver range when it contains a range operator (`<`, `>`, `=`, `^`, `~`, `*`, `,`) and parses as a valid [`VersionReq`](https://docs.rs/semver/) (e.g. `">=1.2.0, <2.0.0"`); otherwise it is matched by **exact string equality**. This keeps a bare `"1.2.3"` exact (rather than the caret semantics `^1.2.3` semver would otherwise infer) and lets non-semver version strings (git hashes, dates) be listed verbatim.

**`[[registries.rules]]` — CVE gate:**

Denies downloads of versions with a recorded vulnerability finding at or above a severity threshold. Requires a configured vulnerability scanner — see [Vulnerability scanning](security-scanning.md) for the full setup.

```toml
[[registries.rules]]
kind         = "cve_gate"
min_severity = "high"        # one of: low, medium, high, critical (default: high)
bypass_roles = ["admin"]
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"cve_gate"` |
| `min_severity` | string | `"high"` | Minimum severity that triggers a block: `"low"`, `"medium"`, `"high"`, or `"critical"`. |
| `bypass_roles` | string[] | `[]` | Roles exempt from the gate. |

**`[[registries.rules]]` — Licence gate:**

Denies downloads by the licence the package's **own manifest** declares. The licence is read out of the archive by the SBOM extractor when the artifact is cached or published, so this rule needs no external feed and no extra upstream call.

```toml
[[registries.rules]]
kind          = "license_gate"
allow         = ["MIT", "Apache-2.0", "BSD-3-Clause"]  # optional allowlist
deny          = ["AGPL-3.0", "SSPL-1.0"]               # always refused
allow_unknown = true                                    # default
block         = true                                    # default false = warn-only
bypass_roles  = ["admin"]
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"license_gate"` |
| `allow` | string[] | `[]` | Approved licences. When non-empty, a declared licence matching none of these is refused. Empty means "no allowlist", not "allow nothing". |
| `deny` | string[] | `[]` | Refused licences. Checked before `allow`, so a licence in both lists is refused. |
| `allow_unknown` | bool | `true` | How to treat a version whose licence is not known. `true` lets it through; `false` refuses it. |
| `block` | bool | `false` | `false` = warn-only: the licence is shown in the console and nothing is ever refused. |
| `bypass_roles` | string[] | `[]` | Roles exempt from the gate. |

> **This rule requires `[registries.sbom]` with `enabled = true` on the same registry.** The licence is read out of the archive as part of SBOM generation, so with SBOM off nothing is ever extracted and the gate sees an unknown licence for every version — however good the parser for that registry type is. Configuring it without SBOM raises `license-gate.sbom-disabled`.
>
> **The first request for an uncached package cannot be gated.** The licence lives inside the archive, so it is recorded on the way *through* the proxy — after the first fetch, not before it. With the default `allow_unknown = true` the first download proceeds and every later one is gated; with `allow_unknown = false` nothing unknown is served, which costs one refused request per new package. This is the same trade `integrity.require_metadata` makes for checksums.
>
> Licence extraction currently covers **cargo, npm, maven, pypi and nuget**. Every other registry type reports an unknown licence permanently, so `allow_unknown = false` on one of those refuses everything.
>
> Configuring `license_gate` on a registry type with no parser raises a config warning — `license-gate.no-extractor` when the rule is merely inert, or `license-gate.denies-everything` when `block = true` and `allow_unknown = false` combine to refuse every download. Both appear on the **Config Reload** admin page and at `GET /api/v1/admin/config/warnings`, because neither state produces a runtime error: the config is valid, the rule is loaded, and it simply cannot see what it claims to govern.

Comparison is case-insensitive and ignores surrounding whitespace. It is otherwise literal: `allow = ["MIT"]` does **not** match a package declaring `MIT OR Apache-2.0`, because a compound expression is a different declaration — accepting it would let any package opt out of the gate by adding an alternative.

The rule gates the package's own licence, not its dependencies': BatleHub does not resolve a dependency graph, so it cannot answer a question about transitive licences and does not pretend to.

**`[[registries.rules]]` — Trusted publisher:**

Restricts downloads to packages published by an allowed org, user, or scope. The publisher is derived from metadata already resolved during the proxy fetch — no extra upstream calls.

```toml
[[registries.rules]]
kind = "trusted_publisher"
allow = ["my-org", "trusted-user"]
bypass_roles = ["admin"]
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `kind` | string | — | Must be `"trusted_publisher"` |
| `allow` | string[] | `[]` | Allowed publisher identifiers. When non-empty, a package whose derived publisher matches none of these is rejected. When empty, the rule allows everything. |
| `bypass_roles` | string[] | `[]` | Roles that may bypass the gate (e.g. `["admin"]`). |

> **Publisher support by registry type:** matching is case-insensitive.
> - **GitHub**, **GitLab**, **Forgejo** — the top-level owner/group segment of the package path (`"owner/repo"` or `"group/subgroup/project"` → `"owner"` / `"group"`).
> - **npm** — the scope for scoped packages (`"@scope/name"` → `"scope"`); otherwise the user who published that version.
> - **OpenVSX**, **VS Code Marketplace** — the publisher segment of the extension id (`"publisher.extension"` → `"publisher"`).
> - **Not yet supported: Cargo** (crate ownership isn't in the sparse index and would need a separate crates.io API call) and any other registry type. Configuring this rule on an unsupported registry **denies every request** — this is a fail-closed supply-chain gate, not a fail-open one.

#### `signed_downloads` {#registry-signed-downloads}

Some clients authenticate the *documents* of an install and then fetch the
*artifact* named by one with no credentials, because their protocol has no way
to send any. Terraform is the measured case: against 1.8.5, every protocol
document is authenticated and every artifact fetch is not — the provider
archive, its `SHA256SUMS` and the detached signature over that, on any host,
including the one it authenticated a request earlier.

Without help the only lever is `anonymous = ["releases:read", "source:read"]`,
which is per *registry*: opening the last step of one provider install opens
every version listing on it, and in hybrid mode everything published locally.

`signed_downloads = true` mints a short-lived, single-coordinate signature into
the document the client *did* authenticate, and accepts it on the fetches that
carry no header. The registry can then set `anonymous = []`.

```toml
[server.signed_urls]
secret = "${BATLEHUB_URL_SIGNING_SECRET}"

[[registries]]
type             = "terraform"
name             = "internal-tf"
signed_downloads = true

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
```

The signature **authenticates a request and authorises nothing**. It stands in
for the `Authorization` header and changes nothing else: the same rule chain,
block list, release-age gate, licence gate, visibility check, quota and audit
run against the identity it carries. A version blocked after a URL was minted
stays blocked, because the block is evaluated at redemption rather than at
minting.

Currently implemented for `terraform`. Setting it on another registry type is
accepted and does nothing, since no other adapter has a route that mints.

See [`[server.signed_urls]`](#server-signed-urls) for the key, its rotation, and
the note about the token appearing in logs.

#### `[registries.integrity]` {#integrity}

Per-registry artifact integrity verification. On the proxy fetch-and-cache path, buffered upstream bytes are hashed and compared against the checksum advertised in the registry metadata (Cargo SHA-256, npm SRI/`shasum`, PyPI SHA-256). Registries that advertise no checksum (NuGet, Maven, GitHub, Go, …) fall through to the "missing" path. Does **not** apply to `firewall_only` registries, which stream straight through without buffering.

```toml
[registries.integrity]
enabled = true            # verify when a checksum is advertised
block_on_mismatch = true  # fail the download on a hash mismatch (never bypassable)
require_metadata = false  # block downloads with no advertised checksum
bypass_roles = ["admin"]  # roles exempt from the require_metadata gate
verify_on_serve = false   # re-hash stored bytes on every serve, not just on first fetch
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. When `false`, no verification is performed. |
| `block_on_mismatch` | bool | `true` | Fail the download (and skip caching) when the computed digest does not match the advertised one. A mismatch is never bypassable. |
| `require_metadata` | bool | `false` | Block downloads for which the upstream advertises no usable checksum, unless the caller holds one of `bypass_roles`. Defaults to warn-only. |
| `bypass_roles` | string[] | `[]` | Roles allowed to bypass the `require_metadata` gate. |
| `verify_on_serve` | bool | `false` | Re-verify cached/stored bytes against a **self-computed** SHA-256 (recorded when the bytes are first cached) on every serve — cache hits on the proxy path and local-registry reads — not just on first fetch. Catches storage corruption or tampering of already-cached artifacts. A mismatch fails the download (`502`) and evicts the bad entry so a later request re-fetches clean bytes. Off by default because it reads and hashes the bytes on each serve (the proxy path streams them through the hash so memory stays bounded, then re-opens the entry to serve it). Pre-existing cache rows have no stored checksum and are treated as "skip re-verify" until next refreshed. |

#### `[registries.signing]` {#signing}

Per-registry artifact signing. At publish time, a client supplies a detached signature via the `X-Artifact-Signature` and `X-Signature-Type` headers, stored alongside the artifact. **The two headers go together**: either one without the other is rejected with `400`, because a signature that names no algorithm can never be verified and a type that names no signature describes nothing. The `required`/`allowed_types` fields gate signature **presence and type** at publish; `verify_on_download`/`trusted_keys` re-check a stored `ed25519` signature on **download**.

```toml
[registries.signing]
required = false                 # reject publishes with no X-Artifact-Signature header
allowed_types = ["ed25519"]      # accepted signature types; empty = any (or none)
verify_on_download = false       # re-verify a stored ed25519 signature on every download
trusted_keys = ["<hex pubkey>"]  # hex-encoded 32-byte Ed25519 public keys trusted to sign
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `required` | bool | `false` | Reject publish requests that do not include an `X-Artifact-Signature` header. A signature is always a pair, so a publish carrying one of the two headers is refused whatever this is set to. |
| `allowed_types` | string[] | `[]` | Accepted signature types (e.g. `["pgp", "ed25519"]`). When empty, any type is accepted. An *unsigned* publish is governed by `required` alone — this list never makes a signature mandatory. |
| `verify_on_download` | bool | `false` | Verify a stored `ed25519` detached signature against `trusted_keys` on every download (local-registry reads). A stored signature that fails to verify — or was signed by an untrusted key, or is of a type this cannot verify, or names **no** type at all — fails the download with `502`: with this on, an artifact that carries a signature and is not verifiable is refused rather than served. An artifact with no stored signature is not verified here; its presence is governed by `required` at publish time. |
| `trusted_keys` | string[] | `[]` | Hex-encoded 32-byte Ed25519 public keys trusted to sign artifacts in this registry. A download verifies against each in turn; any match passes. |

> **Why Ed25519 only?** RSA-based crypto (the `rsa` crate, and therefore PGP / x509 / the default Sigstore paths) is hard-banned from the dependency tree by `deny.toml` (RUSTSEC-2023-0071). Ed25519 detached-signature verification keeps the tree RSA-free; Sigstore / npm provenance verification is left as a future item for that reason.

#### `[registries.vsx_signing]` {#vsx-signing}

The registry's own signature on every VSIX it publishes ([RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry)), for `vscode-marketplace` and `openvsx` registries. A current VS Code's Extensions view greys out Install on any gallery entry without a signature asset — *This extension is not signed by the Extension Marketplace* — and a registry that holds a key serves one for everything it hosts, in the archive shape Open VSX uses: an Ed25519 signature over the whole `.vsix`, a manifest of its entries, an empty `.signature.p7s`. What it proxies from an upstream that signs (the Microsoft marketplace, an Open VSX instance) is relayed with the upstream's own signature whether or not a key is configured, and never re-signed.

```toml
[registries.vsx_signing]
seed_hex = "${VSX_SIGNING_SEED}"   # 32-byte Ed25519 seed, hex — `batlehub-cli vsx keygen` prints one
key_id   = "2026-09"               # optional; default: the first 16 hex characters of SHA-256(public key)
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `seed_hex` | string | — | The seed, 64 hex characters. A secret of the same class as `repo_signing.seed_hex`: keep it out of the file with `${VAR}`. Rejected at load when it is not 32 bytes of hex. |
| `key_id` | string | derived | The id the public key is served under, `GET /proxy/{registry}/api/-/public-key/{key_id}` (PEM, anonymous, cached a day). A URL path segment: `[A-Za-z0-9._-]`. Must change when the key does; the default derives it from the key, so it does. |

**What it does.** At publish the registry writes the signature archive beside the artifact and the gallery advertises `Microsoft.VisualStudio.Services.VsixSignature` and `…PublicKey` for the version; the Open VSX document carries `files.signature` and `files.publicKey`. A version published before the key existed is signed on the first request for its archive; a rotated key re-signs the same way, and the served key always verifies the served archive. On a registry in `proxy` mode the key signs nothing (nothing is published there) and a warning says so. A version whose signature archive was **provided** — an upstream's, attached after the publish with `PUT …/{extension_id}/{version}/vsix/signature` (see the [Open VSX page](/registries/openvsx#signatures)) — is never signed over: the registry serves that archive as-is and advertises no key for it.

**What it does not do.** Make a stock VS Code's own verifier pass: that one accepts the marketplace's signature and no other, so on a stock build the view's Install button turns on and the install needs `extensions.verifySignature: false` — the setting code-server, VSCodium and che-code ship off. The [CLI page](/use/cli#gallery-proxy) says where it goes; `batlehub-cli vsx verify` is the check that replaces it.

#### `[registries.upstream_auth]` {#upstream_auth}

Credentials to send on every upstream request for this registry. Three schemes are supported; choose one.

**Bearer token** — adds `Authorization: Bearer <token>`. Accepted by Gitea, Forgejo, Nexus (npm token), JFrog Artifactory, and GitHub Enterprise.

```toml
[registries.upstream_auth]
type  = "bearer"
token = "npat-xxxx"
```

**Basic auth** — standard HTTP Basic authentication.

```toml
[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "s3cr3t"
```

**Custom header** — sends an arbitrary header on every request. Useful for registries that use `X-API-Key` or similar schemes.

```toml
[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "my-api-key"
```

| Field | Type | Schemes | Notes |
|---|---|---|---|
| `type` | string | all | `"bearer"`, `"basic"`, or `"header"` |
| `token` | string | bearer | Bearer token value |
| `username` | string | basic | HTTP Basic username |
| `password` | string | basic | HTTP Basic password |
| `name` | string | header | HTTP header name (e.g. `"X-API-Key"`) |
| `value` | string | header | HTTP header value |

> **Security:** Never commit credentials to version control. Use `${VAR_NAME}` placeholders in the config file to pull secrets from environment variables at startup — see [§5 Environment Variable Overrides](#_5-environment-variable-overrides) for details.

#### `[registries.tls]` {#upstream_tls}

TLS settings for upstream connections. Use this when the upstream registry serves a certificate signed by a private or self-hosted CA that is not in the system trust store.

```toml
[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `ca_cert_path` | string | no | Path to a PEM-encoded CA certificate to add as a trusted root for this registry's upstream connections |

> The certificate is loaded once at startup. To rotate a CA certificate, restart the server.

---

#### `[registries.proxy]` {#upstream_proxy}

Route all outgoing upstream registry requests through an HTTP, HTTPS, or SOCKS5 proxy. Use this in corporate or air-gapped environments where direct Internet access is restricted.

```toml
[registries.proxy]
url = "http://proxy.corp.example.com:3128"

# Optional: proxy credentials (alternative to embedding in the URL)
# username = "proxyuser"
# password = "${PROXY_PASSWORD}"

# Optional: bypass the proxy for specific hosts/domains (comma-separated).
# Equivalent to the NO_PROXY environment variable.
# no_proxy = "localhost,10.0.0.0/8,internal.example.com"
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `url` | string | yes | Proxy URL. Supports `http://`, `https://`, and `socks5://` schemes. Credentials can be embedded directly: `http://user:pass@proxy:3128`. |
| `username` | string | no | Proxy Basic-auth username. Overrides any credentials embedded in `url`. Use `${VAR}` to inject from an environment variable. |
| `password` | string | no | Proxy Basic-auth password. Overrides any credentials embedded in `url`. Use `${VAR}` to inject from an environment variable. |
| `no_proxy` | string | no | Comma-separated list of hosts, domains, or CIDR ranges to bypass the proxy for (e.g. `"localhost,10.0.0.0/8,corp.example.com"`). Equivalent to the standard `NO_PROXY` environment variable. |

> **Scope:** The proxy applies only to upstream registry requests for the registry it is configured on. When absent, the global `[proxy]` section (if set) is used as a fallback — so you can set a single global proxy and override it per-registry where needed.

> **Security:** Avoid committing proxy credentials to version control. Use `${VAR_NAME}` placeholders — see [§5 Environment Variable Overrides](#_5-environment-variable-overrides).

> **`HTTP_PROXY` / `HTTPS_PROXY` environment variables:** When no `[registries.proxy]` (and no global `[proxy]`) is configured for a registry, the underlying HTTP client automatically reads the standard `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` env vars. As soon as any proxy is configured via the config file, env-var proxy reading is disabled for that registry's client — the config value fully replaces the env var.

#### Forwarding `HTTP_PROXY` into the config

If you want to keep using the standard `HTTP_PROXY` env var while still being able to set `no_proxy` or credentials in the config file, forward the variable through the `${VAR}` substitution mechanism:

```toml
# Shell: export HTTP_PROXY=http://proxy.corp.example.com:3128

[registries.proxy]
url      = "${HTTP_PROXY}"
no_proxy = "localhost,10.0.0.0/8"
```

The same pattern works for the global section:

```toml
[proxy]
url      = "${HTTP_PROXY}"
no_proxy = "${NO_PROXY}"   # forward the standard NO_PROXY list too
```

---

#### `[registries.rate_limit]` {#rate_limit}

Per-registry rate limiting using a **fixed-window counter** algorithm. Limits are tracked per authenticated user (by `user_id`) or per client IP for anonymous requests.

Counters are stored in the **cache backend** selected by `[cache]`:
- `type = "memory"` (default) — counters are per-process; they reset on restart and are **not** shared across multiple server replicas.
- `type = "postgres"` or `type = "redis"` — counters survive restarts and are shared across all replicas, making the limit consistent across a load-balanced cluster.

```toml
[registries.rate_limit]
requests_per_window = 100
window_secs         = 60
enforcement         = "block"   # "block" (default) or "warn"
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `requests_per_window` | u32 | — | Maximum number of requests allowed within `window_secs` |
| `window_secs` | u32 | — | Length of the sliding window in seconds |
| `enforcement` | string | `"block"` | `"block"` returns HTTP 429; `"warn"` allows the request but adds `X-RateLimit-Warning` |

**Response headers:**

| Header | When added | Description |
|---|---|---|
| `X-RateLimit-Limit` | Every proxied response (when configured) | The effective limit that bound this request |
| `Retry-After` | 429 responses (block mode) | Seconds until the bucket refills |
| `X-RateLimit-Reset` | 429 responses (block mode) | Unix timestamp when the bucket refills |
| `X-RateLimit-Warning: rate-limit-exceeded` | Over-limit responses (warn mode) | Signals the limit was exceeded but the request was allowed |

#### Per-group rate limits {#per_group_rate_limits}

All members of a named group share a single request pool. Group names are matched against the strings in the authenticated identity's `groups` list, which are namespaced by auth provider: `"oidc:<group>"`, `"kubernetes:<group>"`, etc.

```toml
[registries.rate_limit]
requests_per_window = 100
window_secs         = 60
enforcement         = "block"

# CI bots share a single 5000 req/min pool across all members:
[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 5000
window_secs         = 60
# enforcement = "block"   # optional; inherits parent enforcement when omitted

# Free-tier users share a more restrictive 200 req/min pool:
[[registries.rate_limit.groups]]
name                = "oidc:free-tier"
requests_per_window = 200
window_secs         = 60
```

`[[registries.rate_limit.groups]]` fields:

| Field | Type | Required | Notes |
|---|---|---|---|
| `name` | string | yes | Exact match against an entry in `Identity.groups` (e.g. `"oidc:ci-bots"`) |
| `requests_per_window` | u32 | yes | Shared pool size for **all** members of this group combined |
| `window_secs` | u32 | yes | Window length in seconds |
| `enforcement` | string | no | Overrides the parent `enforcement` for this group only; defaults to the parent value when omitted |

**Multi-limiter semantics:** both the per-user bucket and every applicable group bucket must have tokens for a request to proceed. If any bucket is exhausted:
- In `block` mode: the request is rejected with HTTP 429. The `Retry-After` and `X-RateLimit-Reset` headers reflect the longest wait among all exhausted buckets.
- In `warn` mode: the request is allowed and `X-RateLimit-Warning` is added to the response.
- If different buckets have different enforcement modes, `block` takes precedence over `warn`.

> **Multi-instance deployments:** Set `[cache] type = "postgres"` or `type = "redis"` to share rate-limit counters across all server replicas. With the default `type = "memory"`, each replica maintains its own independent counters and the effective per-user limit is `requests_per_window × replica_count`.

> **Fail-open behaviour:** If the cache backend is unreachable when a counter needs to be incremented, the request is **allowed** rather than rejected. A `WARN` log entry (`rate-limit store unavailable … failing open`) is emitted for each affected bucket. Monitor for these warnings to detect backend outages.

---

#### `[registries.beta_channel]`

Restricts pre-release versions (semver versions with a non-empty pre-release component, e.g. `1.0.0-beta.1`) so that only members of the registry's beta channel can see and download them. Non-members receive stable versions only and get HTTP 404 on direct pre-release artifact requests.

Applies to registries in `local` or `hybrid` mode. Members are managed via the back-office API.

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.beta_channel]
enabled = true
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Enable beta-channel access gating for this registry |

**Member management API (admin only):**
- `GET    /api/v1/admin/registries/{registry}/beta-channel` — list members
- `POST   /api/v1/admin/registries/{registry}/beta-channel` — body: `{ "principal_type": "user"|"group", "principal_id": "...", "granted_by": "..." }`
- `DELETE /api/v1/admin/registries/{registry}/beta-channel/{principal_type}/{principal_id}` — remove member

#### `[registries.retention]`

Retention for what this registry holds **locally**. Absent — the default — keeps
everything forever, which is what every instance does without this block.

Not to be confused with the eviction keys on [`[registries.cache]`](/guide/caching),
which govern the *proxy cache*: an evicted cache entry is re-fetchable from
upstream, a local version is frequently the only copy in existence. A
`[registries.retention]` block on a `proxy`-mode registry is a **startup error**,
because it would govern an empty set.

The block governs two different objects: **published versions**, which retention
reclaims, and the **tombstones** their deletion leaves, whose detail compaction
strips. Deleting a version already leaves a permanent tombstone with no
configuration at all; nothing here changes that, and the coordinate claim is
never removed by any setting. See
[Deleting a published version](/guide/admin-policies#deleting-versions).

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.retention]
keep_versions       = 10
keep_if_pulled_days = 90    # the veto that makes this safe to switch on
keep_for_days       = 365
dry_run             = false
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `keep_versions` | int | *(unset)* | Keep the newest N versions of every package, by publish date |
| `keep_for_days` | int | *(unset)* | Keep anything **published** within this window |
| `keep_if_pulled_days` | int | *(unset)* | Keep anything **downloaded** within this window |
| `keep_yanked` | bool | `true` | Keep yanked versions |
| `download_signal_floor_days` | int | *(built-in)* | Before this point, "no download record" proves nothing. Defaults to 2026-08-27 |
| `reclaim_delay_ms` | int | `0` | Pause between reclamations, bounding a first live run |
| `tombstone_detail_for_days` | int | *(unset)* | Strip a tombstone's detail this many days after the deletion. Unset keeps it forever. Minimum 30 |
| `dry_run` | bool | `true` | Report and change nothing |

### Keep conditions are a union of vetoes

**A version survives if *any* configured condition matches.** There is no
expression to write and no ordering to get wrong: the only way to reclaim a
version is for every configured condition to decline to keep it. Wrong
configuration therefore fails toward keeping, which is the direction that is
recoverable.

A block with **no** keep condition is rejected at startup, because it is the one
that reclaims every version on its first live run. `keep_yanked` does not count:
it defaults to `true` and only ever vetoes, so a block containing nothing else
would still destroy every unyanked version. `0` is rejected for every window — a
keep condition set to zero keeps nothing, which is not what it looks like it
means.

### `keep_if_pulled_days` is the one that matters

`keep_versions = 10` alone throws away the version half the estate is pinned to,
because it happens to be eleventh by date. `keep_if_pulled_days` is the rule that
makes retention safe to switch on: *whatever anyone is actually using stays,
regardless of age or count.*

Configuring reclamation without it raises `retention.no-pull-veto` on every
reload. Live reclamation raises `retention.reclamation-live` as well, and live
compaction `retention.compaction-live` — all three on every reload, because
unlike cache eviction these destroy the only copy.

Reading the download signal means reading its gaps too. The Maven and NuGet local
artifact paths recorded no download event at all before 2026-08-26, so
`download_signal_floor_days` marks the point before which an *absent* record
proves nothing, and a version whose only evidence predates it is kept. Set it
explicitly if this instance's audit history begins later — after a restore, or an
`audit_purge`.

A `keep_if_pulled_days` policy on a deployment with no package repository
**refuses to run** rather than treating "no signal" as "no downloads".

### What this does not have

Retention here is **registry tier**. The namespace and package tiers RFC 0016
§4.1 describes need RFC 0015's namespace blocks and its `policy` table, neither
of which exists yet. The version-tier pin does not need them and is available:
`POST …/retention-pin` sets `retention_keep` on a version, and a pinned version
is never reclaimed whatever the policy says.

**API (admin only):**
- `POST /api/v1/admin/registries/{registry}/retention` — run retention; optional `?dry_run=true` to preview. `409` when no keep condition is configured
- `POST /api/v1/admin/registries/{registry}/retention-pin` — body `{name, version, keep}`; pin or release a version
- `GET  /api/v1/admin/registries/{registry}/tombstones` — deleted coordinates, newest first; optional `?name=`
- `POST /api/v1/admin/registries/{registry}/tombstones/compact` — run compaction; optional `?dry_run=true` to preview. `409` when no window is configured

`?dry_run=` can only ever make a run *safer*: passing `false` does not override a
configured `dry_run = true`.

---

### 3.6 `[ip_blocking]` (optional)

Automatically blocks IP addresses that trigger too many violation events within a rolling time window — similar to fail2ban. Blocked IPs receive HTTP 403 with an `X-Block-Expires` header until the ban expires.

```toml
[ip_blocking]
enabled               = true
violation_threshold   = 10      # violations before auto-block
violation_window_secs = 300     # counting window in seconds (5 min)
ban_duration_secs     = 3600    # how long to block the IP (1 hour)
trigger_on_status     = [429, 401]   # HTTP response codes that count as violations
trusted_proxies       = ["10.0.0.1"] # IPs whose X-Forwarded-For header is trusted
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Enable/disable the middleware |
| `violation_threshold` | int | `10` | Number of violations before auto-block |
| `violation_window_secs` | int | `300` | Window length for counting violations |
| `ban_duration_secs` | int | `3600` | How long the auto-block lasts |
| `trigger_on_status` | int[] | `[429, 401]` | Response status codes that count as violations |
| `trusted_proxies` | string[] | `[]` | Upstream proxy IPs allowed to set `X-Forwarded-For` |

**Backends:** Block state is stored in the same backend as the cache (`memory`, `postgres`, or `redis`). Use `postgres` or `redis` for multi-instance deployments.

**Manual management:** Admins can manage blocks via the back-office API:
- `GET    /api/v1/admin/ip-blocks` — list currently blocked IPs
- `POST   /api/v1/admin/ip-blocks` — body: `{ "ip": "1.2.3.4", "reason": "...", "duration_secs": 3600 }`
- `DELETE /api/v1/admin/ip-blocks/{ip}` — unblock an IP

**Trusted proxies:** When a request arrives through a known reverse proxy, batlehub reads the real client IP from `X-Forwarded-For` only if the TCP peer address appears in `trusted_proxies`. Without this configuration, `X-Forwarded-For` is ignored to prevent header-spoofing attacks.

---

### 3.7 `[otel]` (optional)

Enables OpenTelemetry distributed tracing via OTLP gRPC.

```toml
[otel]
endpoint = "http://localhost:4317"
service_name = "batlehub"   # default
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `endpoint` | string | — | OTLP gRPC endpoint |
| `service_name` | string | `"batlehub"` | Service name reported in traces |

The entire section can be enabled without a config file change by setting `PROXY_CACHE__OTEL__ENDPOINT` — the section is created automatically if the env var is present.

---

### 3.8 `[proxy]` (optional)

A **global** HTTP/SOCKS proxy that applies to all upstream registry requests. Individual registries that define their own `[registries.proxy]` section override this global setting for that registry only.

```toml
[proxy]
url      = "http://proxy.corp.example.com:3128"
# username = "proxyuser"   # optional
# password = "${PROXY_PASSWORD}"   # optional
# no_proxy = "localhost,10.0.0.0/8,internal.example.com"  # optional
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `url` | string | yes | Proxy URL (`http://`, `https://`, or `socks5://`). Credentials can be embedded: `http://user:pass@proxy:3128`. |
| `username` | string | no | Proxy Basic-auth username. |
| `password` | string | no | Proxy Basic-auth password. Use `${VAR}` to keep secrets out of the file. |
| `no_proxy` | string | no | Comma-separated hosts/domains/CIDRs to bypass the proxy for. |

The entire section can be set without touching the config file via environment variables:

```sh
export PROXY_CACHE__PROXY__URL="http://proxy.corp.example.com:3128"
export PROXY_CACHE__PROXY__USERNAME="proxyuser"
export PROXY_CACHE__PROXY__PASSWORD="s3cr3t"
export PROXY_CACHE__PROXY__NO_PROXY="localhost,10.0.0.0/8"
```

`PROXY_CACHE__PROXY__URL` creates the `[proxy]` section automatically if it is not present in the TOML file, so a minimal deployment only needs the single env var set.

> **Precedence:** per-registry `[registries.proxy]` > global `[proxy]`. When neither is set, the underlying HTTP client reads the standard `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` env vars automatically. Configuring any proxy via the config file disables env-var proxy reading for that registry's client — to forward those env vars in, see [Forwarding `HTTP_PROXY` into the config](#forwarding-http-proxy-into-the-config) above.

---

### 3.8a `[stats]` (optional)

What numbers this instance keeps, and what it publishes. Two flags, one block,
because "do I want this instance keeping numbers" is one operator question —
even though the halves differ: `metrics_enabled` is about **exposure**,
`history_*` is about **storage**.

```toml
[stats]
history_enabled        = true   # default
history_retention_days = 30     # default; 0 disables pruning, not history
metrics_enabled        = true   # default: pre-RFC-0004 behaviour
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `history_enabled` | bool | `true` | Record the hourly cache rollup behind the admin dashboard's trend. `false` restores the pre-RFC-0004 dashboard, which shows only counters since the current process started |
| `history_retention_days` | u32 | `30` | Delete rollup rows older than this. `0` keeps every row — it disables *pruning*, not history |
| `metrics_enabled` | bool | `true` | Install the Prometheus recorder and serve `/metrics`. `false` makes `/metrics` answer `503 metrics not configured` |

**`metrics_enabled` is a security control, not a preference.** `/metrics` is
unauthenticated and, before this block existed, unconditional: it publishes
cache hit rates, per-registry pull volumes and upstream latencies to anyone who
can reach the port. That is a defensible default behind an ingress that does not
route it, and indefensible for a self-hoster who had no way to close it. It
defaults to `true` so no existing scrape breaks on upgrade.

**Why the rollup rather than the access log.** The access log already holds every
download, so a 30-day hit rate could in principle be derived from it. It is not,
deliberately: that table is an *audit* trail with its own retention and purge
semantics, and deriving an operational chart from it would let an audit purge
silently rewrite a dashboard. A hit/miss ratio is also a counter question, and
scanning an audit table per dashboard load is fine at ten thousand rows and a
problem at ten million.

The interval is fixed at one hour and is not configurable: it is the resolution
the data is *kept* at, daily figures can always be aggregated on read but never
recovered, and two instances with different intervals would have incomparable
histories. One row per registry per hour is under 9 000 rows a year.

The table holds no principal and no coordinate — registry, window, counters — so
its retention is an operational choice rather than a privacy one.

---

### 3.8b `[cache_coherence]` (optional)

Periodic collection of storage blobs nothing references.

An artifact is cached in two steps — the bytes are stored, then the row that
points at them is recorded. A process killed between the two leaves a blob that
no request can reach and that **no eviction strategy will ever consider**,
because every strategy reads the table the row is missing from. Deleting a row
by hand leaves the same thing. This is the sweep that collects them.

```toml
[cache_coherence]
enabled       = true
interval_secs = 86400   # default: daily
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Run the sweep on a timer. Absent block means the same as `false` |
| `interval_secs` | u64 | `86400` | Seconds between sweeps, across every registry |

**Off by default**, like every other policy here that deletes: it runs with
nobody watching. A deployment that would rather look first has the same sweep on
demand, and a preview of it —
[see the admin guide](/guide/admin-policies#cache-coherence).

**The interval is also the grace window.** A blob is deleted only if the
*previous* sweep saw it orphaned too, because a cache write in flight — bytes
stored, row not yet recorded — looks exactly like an orphan. The gap between two
sweeps is the whole margin that write gets, so a short interval trades the one
safety property this sweep has for faster collection of bytes nobody can reach.
Below 300s the server emits a config warning (`cache-coherence.interval-too-short`)
rather than refusing: a small estate on fast storage can reasonably want a tighter
loop.

Each run also lists **every cached object of every registry** — a paginated
`ListObjectsV2` against S3, or a full directory walk on a filesystem backend.
That is a second reason the default is daily rather than hourly.

The first sweep of a process runs one interval after startup, not at second
zero: a restart is exactly when half-finished cache writes exist, and a fresh
process has an empty grace set.

Scheduled sweeps are audited as `cache_coherence_run` with `user_id = "system"`
— the field that separates them from an operator who ran one by hand.

---

### 3.8c `[upstream_audit]` (optional) {#upstream-audit}

A periodic sweep that asks each proxy or hybrid upstream whether the artifacts
cached from it still exist, confirms a disappearance across several sweeps
before believing it, and **holds a confirmed artifact back from eviction** so
the last copy in the estate is not garbage-collected precisely because
upstream stopped refreshing it (RFC 0014). Off unless asked for: it sends
scheduled requests to third-party registries.

```toml
[upstream_audit]
enabled              = true
interval_secs        = 21600    # 6 h between sweeps; floor 300
confirm_after        = 3        # consecutive sweeps a miss must survive
confirm_min_age_secs = 86400    # …and at least this long since the first miss
outage_ratio         = 0.25     # above this fraction missing, the sweep is void
on_confirmed         = "audit"  # or "block": refuse a confirmed disappearance on the wire
retain_disappeared   = true     # hold confirmed artifacts back from eviction
skip_recently_seen   = true     # real traffic counts as a successful probe
registries           = []       # empty = every proxy/hybrid registry
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Nothing sweeps unless asked. |
| `interval_secs` | u64 | `21600` | Seconds between sweeps. Below `300` is a config error: a faster loop is a denial of service against someone else's registry, from a typo. |
| `confirm_after` | u32 | `3` | Consecutive misses, each in a valid sweep, before a disappearance is believed. `0` is refused. |
| `confirm_min_age_secs` | u64 | `86400` | The other floor: at least this long since the first miss. Both must clear, so with the defaults the fastest confirmation is 24 h. Lowering only `interval_secs` buys more probes and the same answer. |
| `outage_ratio` | f64 | `0.25` | A sweep in which more than this fraction of a registry's probed packages came back missing is **void**: nothing recorded, nothing confirmed. An outage affects nearly everything; an unpublish affects one thing. Must be in `(0.0, 1.0]`. Below ten probed packages the ratio is skipped and the two floors carry the decision alone. |
| `on_confirmed` | string | `"audit"` | What a confirmation does beyond recording, holding and notifying, for every audited registry that does not say otherwise. `"block"` also blocks every held version of the name through the admin block list (`blocked_by = system:upstream-audit`), and lifts *its own* block when the package reappears — never an admin's. Any other value is a config error rather than a fallback. A registry overrides this for itself with its own `on_confirmed` (RFC 0014 §13 O6; deepest wins) — auto-block a public upstream, audit-only an internal mirror. See the paragraph below before choosing `"block"`, and the [operations page](/operations/upstream-disappearance) for what it looks like from the console. |
| `retain_disappeared` | bool | `true` | Hold a confirmed artifact back from the TTL, idle and keep-latest-N eviction passes, and re-pin its cached metadata each sweep. **Not** from the LRU size cap: that exists to stop the disk filling, so held artifacts sort last there instead of being exempt. |
| `skip_recently_seen` | bool | `true` | A package re-cached from upstream since the last sweep started was demonstrably present; its probe is skipped. |
| `registries` | string[] | `[]` | Only these registries. Empty means every registry in `proxy` or `hybrid` mode. Naming an unknown or a `local` registry is a config error. |

**What `"block"` costs.** Under `"audit"` the worst outcome of a false
confirmation is an admin reading a wrong alert. Under `"block"` it is a
targeted denial of service: an attacker who can serve selective `404`s to
this instance — control of the path to the upstream, held across the whole
confirmation window, narrowly enough not to trip `outage_ratio` — picks a
package the estate depends on and the estate blocks it against itself. That
position already lets them serve fabricated metadata on a cache miss, so the
*capability* is not new; the cost is, and it is not fully mitigable. Choose
`"block"` for an estate whose threat is a withdrawn or hijacked package
reaching a build; keep the confirmation window long, keep
`retain_disappeared = true` (without it a blocked package is never read and
idle eviction deletes the copy the block was keeping —
`upstream-audit.block-without-hold` warns), and know that turning the policy
off unblocks nothing: the blocks it wrote are administrative state, listed
under `system:upstream-audit` in the console's block table, and stay until an
admin lifts them or the package reappears.

**How a sweep decides.** Per registry: every cached package is probed — one
listing request per package on the kinds that have a listing document, one
request per version (25 at most per package per sweep) on the kinds that do
not, and one `HEAD` per held file on the path-addressed kinds (`deb`, `rpm`,
`pacman`, `jetbrains`, `generic`), whose rows and blocks then name the file's
path; an upstream that fails to answer is *inconclusive* and counts on neither
side of the ratio. A miss inserts or increments a row; a successful probe
deletes it outright, never decrements it. A confirmed row is logged at `WARN`
with the coordinate and the misses, appears in the `batlehub_upstream_*`
gauges and in the console's *Operations → Upstream* table, and is sent to
every subscription on `package_disappeared_upstream`; a reappearance clears
the row, logs it and sends `package_reappeared_upstream`; a void sweep sends
`upstream_unreachable` for the registry. `POST
/api/v1/admin/upstream/recheck` probes one package now, through the same
ladder and state machine. **The first sweep after
enabling finds nothing**, by design — every miss starts at one — and the
first confirmations arrive after `confirm_min_age_secs`.

**Where it runs.** On the `worker` role (see [`[server].roles`](#31-server)):
the probe is a scanner on RFC 0018's worker, and `[worker].max_concurrent`
bounds the simultaneous upstream requests. A proxy-only process with the
section enabled logs a warning and raises `upstream-audit.no-worker-role`.
On a registry with [`[registries.security]`](#registries-security) a
confirmed disappearance is also an `UNPUBLISHED_UPSTREAM` finding on the
version's verdict — recorded and visible in `batlehub why`, never a hold
under `"audit"`.

**Metrics.** `batlehub_upstream_missing_total` and
`batlehub_upstream_disappeared_total` (gauges, per registry),
`batlehub_upstream_audit_sweeps_total` (counter, `outcome` = `ok` / `void`),
`batlehub_upstream_audit_duration_seconds`. A rising `void` rate is the
alert that says the feature has stopped working; a gauge of disappearances
alone would never show it. The eviction report's `held` count says what the
hold kept on each pass.

---

### 3.9 `[subdomain_routing]` (optional)

Every registry is always reachable at `/proxy/{name}/…`. This section adds a
second ingress: a hostname whose **root** is the registry.

```toml
[subdomain_routing]
enabled     = true                # derive "<name>.<base_domain>" per registry
base_domain = "hub.example.com"   # npm1.hub.example.com -> registry "npm1"
scheme      = "https"             # only used to advertise public URLs

[[registries]]
name         = "npm1"
type         = "npm"
hosts        = ["npm.acme.io"]    # optional extra vanity hosts
path_routing = true               # default; false => the host is the only ingress
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Derive a wildcard host per registry |
| `base_domain` | string | — | Required when `enabled = true` |
| `scheme` | string | `"https"` | Only decides whether the API advertises `https://…` or `http://…`; **never** affects routing |
| `registries[].hosts` | string[] | `[]` | Extra hostnames rooted at this registry. Independent of `[subdomain_routing]` |
| `registries[].path_routing` | bool | `true` | `false` makes `/proxy/{name}/…` return 404 |

```ini
# .npmrc — subpath
registry=https://hub.example.com/proxy/npm1/

# .npmrc — with a vanity host
registry=https://npm.acme.io/
```

**On a registry host, every path is the registry's.** There is no passthrough
allowlist, because cargo (`/api/v1/…`), GitLab (`/api/v4/…`) and Forgejo
(`/api/packages/…`) all legitimately serve paths under `/api`, and a `generic` or
`deb` registry can legitimately mirror `/healthz` or `/metrics`. The admin API,
the SPA, `/healthz` and `/metrics` therefore live on the **main host only** —
point your probes and scrapes there.

A corollary worth knowing: `https://npm1.hub.example.com/proxy/npm1/lodash`
becomes `/proxy/npm1/proxy/npm1/lodash` and 404s. Pick one ingress per client.

Every URL the server generates reflects the ingress the client actually used, so
a packument fetched from `npm.acme.io` advertises
`https://npm.acme.io/lodash/-/lodash-4.17.21.tgz` while the same packument on the
subpath keeps advertising `https://hub.example.com/proxy/npm1/…`.

#### `path_routing = false`

Makes a registry reachable **only** through its host(s). The motivation is
isolation: once a team is handed `npm.acme.io`, you may not want the same content
answering on the shared main host, where it inherits that host's CORS policy, WAF
rules and cache keys, and where a URL leaked from one ingress silently keeps
working on the other. The subpath returns **404**, not 403 — a disabled ingress
should look absent, not forbidden.

#### Operator prerequisites

- A DNS record per host (or a wildcard `*.hub.example.com`).
- A certificate covering it — a wildcard certificate for the `base_domain` case.
- A reverse proxy that forwards the original `Host` header.
- [`[server].trusted_proxies`](#31-server) listing that proxy's CIDR ranges.
  **This is mandatory:** configuring host routing with no trusted-proxy policy is
  a startup error, because routing would then depend on an ungoverned header.

With the Helm chart, add the hosts to `ingress.extraHosts` and the CIDR to
`config.server.trusted_proxies`.

#### Validation

Rejected at startup and on every reload:

| Condition | Why |
|---|---|
| `enabled = true` with no `base_domain` | the section would route nothing |
| the same host claimed by two registries | ambiguous; last-write-wins would be invisible |
| a `hosts` entry colliding with another registry's wildcard host | same ambiguity, harder to spot |
| a `hosts` entry equal to `base_domain` | would shadow the main host and hide the admin API |
| a `hosts` entry containing `/`, a scheme prefix, or empty after trimming | not a hostname |
| `path_routing = false` on a registry with no reachable host | the registry would be unreachable entirely |
| host routing with neither `[server].trusted_proxies` nor `[ip_blocking].trusted_proxies` | routing would depend on an ungoverned header |

Warned about, but accepted — see `GET /api/v1/admin/config/warnings` and the
Config Reload admin page:

| Condition | Behaviour |
|---|---|
| a registry name that is not a valid DNS label (`my_registry`, `Foo.Bar`) | no wildcard host for it; it stays reachable by path and by any explicit `hosts` entry |
| host routing satisfied only by the deprecated `[ip_blocking].trusted_proxies` | accepted and honoured; move the list to `[server]` |

#### Rollback

A config edit plus a hot reload. Nothing is persisted, and with no
`[subdomain_routing]` and no `hosts` the routing table is empty, the middleware
is a no-op, and every generated URL is byte-identical to a deployment that never
had the feature.

---

### 3.10 `[scanners]` and `[worker]` (optional) {#scanners-and-worker}

Scanners are declared once, globally, and registries opt in by name in
[`[registries.security]`](#registries-security). The worker is the process
role that runs them.

```toml
[scanners.osv]                       # implicit — declare it only to change something
type = "osv"
# api_url = "https://api.osv.dev"

[scanners.socket]                    # Socket.dev: metered, opt-in per registry, key required
type    = "socket"
api_key = "${SOCKET_API_KEY}"
# api_url = "https://api.socket.dev"

[scanners.mlab]                      # mlab.sh CVE API: CVSS/EPSS/KEV on CVE findings (enrichment)
type    = "mlab"
# api_key = "${MLAB_API_KEY}"        # optional: the endpoint answers unauthenticated
# api_url = "https://vuln.mlab.sh"

[scanners.osv.escalation]            # optional, per scanner
kinds = ["vulnerability"]            # FindingKinds that combine
count = 3                            # this many at or above `from`…
from  = "medium"
to    = "high"                       # …are raised to this severity

[server]
roles = ["proxy", "worker"]

[worker]
max_concurrent   = 4                 # scan jobs in flight in this process
registries       = []                # empty = every registry; else only these names
job_timeout_secs = 600
max_attempts     = 3                 # then the verdict carries SCANNER_ERROR

[worker.sandbox]                     # what every binary scanner runs under
runtime          = "bwrap"           # "none" is refused unless BATLEHUB_UNSAFE_NO_SANDBOX=1
memory_limit_mb  = 2048              # RLIMIT_AS on the scanner process
cpu_seconds      = 300               # RLIMIT_CPU
max_extracted_mb = 512               # the extraction policy's ceiling, refused not truncated
max_entries      = 50000

# The archive scanners of RFC 0018 phase 3. `command` must be an executable
# file (or on PATH) in the process that runs the worker role — the worker
# image (Containerfile.worker) carries all three; the proxy image none.
[scanners.postmortem]
type     = "postmortem"
command  = "/usr/local/bin/postmortem"
timeline = true                      # npm only: the transition signals (publisher changed, …)
online   = false                     # `--enrich`; keeps the sandbox's network namespace when true

[scanners.trivy]
type         = "trivy"
endpoint     = "http://batlehub-trivy:4954"   # a Trivy server; empty = the client's own database
timeout_secs = 120

[scanners.guarddog]                  # optional second opinion on npm, PyPI and Go
type       = "guarddog"
command    = "/usr/local/bin/guarddog"
ecosystems = ["npm", "pypi"]

[scanners.sigstore]                  # npm provenance attestations, checked against Rekor
type        = "sigstore"
rekor_url   = "https://rekor.sigstore.dev"
require_for = ["npm"]                # PROVENANCE_MISSING on these kinds; elsewhere absence is silent
```

| Scanner `type` | Ships in | Keys | Notes |
|---|---|---|---|
| `osv` | now | `api_url` | The OSV.dev query already behind `cve_gate`, as a scanner: a vulnerability at or above the registry's `max_severity` is a finding. Runs on every kind with a package URL; the path-proxy kinds, `nodedist`, the marketplaces and Terraform have none. |
| `postmortem` | now | `command`, `online`, `timeline` | The archive is extracted under the sandbox's policy into the layout its ecosystem keeps a dependency in (`node_modules/<name>`, `site-packages/…`, `vendor/…`), a lockfile is written from the coordinate — never by running the ecosystem's tool — and `postmortem scan --json --no-config` runs inside `bwrap`. Findings: `INSTALL_HOOK`, `MALWARE_SIGNAL` (IOC, obfuscation, sensitive API), `TYPOSQUAT_SUSPECT`; with `timeline`, the transition codes at the scanned version (npm). Covers npm, PyPI, Cargo, RubyGems, Composer, Go, Maven. |
| `trivy` | now | `endpoint`, `timeout_secs` | The Trivy **client**, against the server at `endpoint` (the chart's `trivy.enabled` deploys one) or its own database when empty. Scans the CycloneDX SBOM this instance already recorded for the artifact, else the extracted archive. Findings: `VULNERABILITY` with the CVE as reference. |
| `guarddog` | now | `command`, `ecosystems` | DataDog GuardDog on npm, PyPI and Go archives, under the same sandbox. Optional second opinion; not in the default profile, and the only scanner that is not on the worker image — it ships on the `-worker-guarddog` variant, which a deployment runs instead. The rule-to-finding mapping is by rule family and is *read, not observed* until that image runs it. |
| `sigstore` | now | `rekor_url`, `require_for` | npm provenance: the attestations the packument announces for the version are fetched and every transparency-log entry they cite is looked up in Rekor. `PROVENANCE_MISSING` on the kinds in `require_for`, `PROVENANCE_INVALID` when a cited entry is not in the log. An existence-and-inclusion check, not a full Sigstore verification. |
| `socket`, `mlab` | RFC 0018 phase 5 | `api_key` for `socket` | `socket` is refused at load without one (a `401` nobody reads otherwise); `mlab`'s CVE API answers unauthenticated, so its key is a rate-limit courtesy rather than a requirement. `mlab` only enriches other findings and is refused in `required_scanners` (`security.enrichment-required`). |

| `[worker]` field | Type | Default | Notes |
|---|---|---|---|
| `max_concurrent` | u32 | `4` | Jobs leased at once by this process. |
| `registries` | string[] | `[]` | Scope the worker to these registry names; each must exist. Empty is every registry. |
| `job_timeout_secs` | u64 | `600` | A lease that is not completed or heartbeated within this time returns to the queue. |
| `max_attempts` | u32 | `3` | Attempts before the coordinate's verdict records `SCANNER_ERROR` and the job closes. |
| `sandbox.runtime` | string | `"bwrap"` | What every binary scanner runs under: new user/pid/ipc/uts namespaces, no network unless the scanner declares it, the root read-only, the per-job directory as the only writable mount, an empty environment, argv passed with no shell. `"none"` runs the bare command and is refused unless `BATLEHUB_UNSAFE_NO_SANDBOX=1`. |
| `sandbox.memory_limit_mb`, `sandbox.cpu_seconds` | u64 | `2048`, `300` | `RLIMIT_AS` and `RLIMIT_CPU` on the scanner process. |
| `sandbox.max_extracted_mb`, `sandbox.max_entries` | u64 | `512`, `50000` | The extraction policy: an archive over either is **refused**, never truncated, as is one whose decompression ratio passes 100:1, an entry that escapes the root, a symlink, a hardlink, a device. Nested archives are written and not descended; execute bits are dropped. |

**What a scan needs.** A scanner that reads bytes (`postmortem`, `guarddog`,
`trivy` without an SBOM) has the worker fetch the version's primary
artifact — from the cache when it is there, else from upstream, not cached
— so a profile of metadata-only scanners costs no egress. A kind whose
version is a *set* of files (PyPI, Maven, conda, Terraform) has no single
artifact to hand over, and those scanners answer `SCANNER_UNSUPPORTED` for
it rather than pretending to have looked.

**Where the toolchains live.** Only the worker role opens artifacts, so only
the worker image (`Containerfile.worker`: bubblewrap, postmortem, the Trivy
client) carries the tools; the proxy image stays distroless. In the chart,
`worker.enabled` deploys it as its own Deployment from that image and
`config.server.roles = ["proxy"]` stops the proxy pod scanning.

GuardDog is the exception. It is the one scanner that is not a static binary
— it brings a Python interpreter and its own venv — and it is optional, so
it has an image of its own: `Containerfile.worker-guarddog`, published as
`…-worker-guarddog`, which is the worker image with GuardDog added. Enabling
`[scanners.guarddog]` therefore means pointing `worker.image.repository` at
that variant; the process refuses to start if the `command` is not on the
image it is running. Nothing else changes — GuardDog is still a subprocess of
the worker role under the same sandbox.

**How the queue behaves.** Jobs carry a trigger — `FirstSeen` (a user is
waiting) is dequeued before `Webhook`, `Rescan` and `Backfill`; within a
tier, oldest first. A job is leased with a heartbeat, so a worker that dies
mid-scan hands its job to the next one after `job_timeout_secs`. Several
worker processes share one queue through the database; the live ones are
counted in `batlehub_workers_live`, and a proxy-only process warns at
startup when that count is zero.

---

### 3.11 `[[flag_sources]]` (optional) {#flag-sources}

The third parties that may push vulnerability flags ([RFC 0002](/rfc/0002-vulnerability-flags-and-exposure),
recast by its §13): a SOC, a corporate vulnerability platform, an advisory
feed. A flag says *what* the source asserts about a package version (`cve`,
`malware`, `license`, `policy`, or a kind of its own) and *how hard* it wants
this instance to react (`inform`, `warn`, `gate`, `hard_block`).

```toml
[[flag_sources]]
name = "soc"                        # the path segment of the push endpoint
secret = "hmac-key-from-your-vault" # required, non-empty
max_effect = "hard_block"           # inform | warn | gate | hard_block; default gate
registries = ["npm", "pypi"]        # empty (default): any registry
max_flags_per_minute = 600          # 0 disables the limit
```

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | — | `[a-z0-9][a-z0-9_-]*`, at most 64 characters, unique — and distinct from every `[[notifications.inbound]]` name, because a `security.verdict` event stores its `hard_block` under the webhook's name and this source's secret is what revokes a flag under that name. The source pushes to `POST /api/v1/flags/{name}` and revokes with `DELETE /api/v1/flags/{name}/{external_id}`. |
| `secret` | — | HMAC-SHA256 key. The push carries `X-Hub-Signature-256: sha256=<hex>` over the raw body (over the empty string on a `DELETE`), the same scheme `[[notifications.inbound]]` verifies. An unknown name and a bad signature answer the same `404`. |
| `max_effect` | `gate` | The strongest effect this source may set. A push asking for more is stored at the ceiling and told so (`effect_capped: true`). |
| `registries` | `[]` | The registries the source may flag. An item naming another one is rejected, per item. |
| `max_flags_per_minute` | `600` | Items per minute across pushes; over it the whole push is `429` with `Retry-After`. |

**What a flag does.** On a registry with a [`[registries.security]`](#security)
profile the flag is a finding of the version's verdict: `hard_block` is
`SOC_VERDICT` and denies under any policy — a version this instance has
already judged is denied the moment the push is accepted, and the rescan
that follows re-derives the same answer; `gate` is judged at the pushed
`severity` against `max_severity`; `warn` and `inform` are recorded for the
report. On a registry without one, `FlagsRule` reads the flags on every
request: `hard_block` refuses outright, `gate` borrows the registry's
`cve_gate` threshold (`high` when none is configured), the other two never
refuse. An operator override is a gate exemption on the gate `flags`
(`gates:exempt`, time-boxed, with a reason), on either kind of registry.

**What a flag can name.** One exact `version`, or `version_range = "*"` for
every version of the package. Any other range is refused per item: a range
needs the registry kind's version ordering, which is its own RFC.

::: warning A `hard_block` source can refuse every download of what it names
The server warns at startup (`flag-source.can-hard-block`) for every source
whose ceiling is `hard_block`. There is no threshold and no role bypass on
that path; the only relief is a gate exemption on the version.
:::

The administrator reads what was pushed with `GET /api/v1/admin/flags`
(`flags:read`) and asks *who pulled a flagged version* with
`GET /api/v1/admin/exposure` (`audit:read`) — see
[Incident response](/operations/incident-response#who-pulled-a-flagged-version).

### 3.12 `[air_gap]` (optional) {#air-gap}

A server that **will not dial out** ([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate)).
Absent, or `enabled = false`, is exactly today's behaviour; this is additive
and `config_version` does not move.

```toml
[air_gap]
enabled             = true
bundle_trusted_keys = ["3b1f…"]   # hex ed25519 public keys accepted on import
synthesise_listings = true        # answer a listing from what this instance holds
record_misses       = true
miss_retention_days = 90
```

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | No proxy-mode registry attempts an upstream connection. A cache hit is served exactly as today; a miss is an immediate `503` naming the registry and coordinate, not a connect error some seconds later. |
| `bundle_trusted_keys` | string[] | `[]` | Hex-encoded 32-byte ed25519 public keys whose signature an imported bundle must carry. **Required** when `enabled`: an instance whose only content path is unauthenticated is worse than one with no content path. |
| `synthesise_listings` | bool | `true` | A listing this instance holds no document for — the packument `npm install` reads, the simple page `pip` reads, the release a pinned `mise install` asks for — is composed from the versions it *does* hold and answered `200` with `X-BatleHub-Listing: synthesised` (RFC 0008-bis). Every version such a listing names is served by the next request; a version it does not hold is not named, so the client stops by itself (`ETARGET`, "no matching distribution") instead of retrying a `503`. Composed for npm's packument, PyPI's simple page (PEP 691 JSON and PEP 503 HTML), cargo's sparse index (from the crate's own manifest, read at import), Go's `@v/list`, `@latest` and `.info`, `maven-metadata.xml`, NuGet's flat index, GitHub, Forgejo and GitLab releases (listing and by tag), nodedist's `index.tab`/`index.json`, SDKMAN's `versions/all`, RubyGems' compact index, conda's `repodata.json`, NuGet's registration page and Composer's `p2` (the last four from facts the import reads out of the package). Terraform is not composed and stays a `503`. `false` is RFC 0008's behaviour: every unheld listing a `503` and a recorded miss. Read only under `enabled`. |
| `record_misses` | bool | `true` | Record what was asked for and not held, one row per `(registry, key)` with a counter. This record is the input to the next bundle. |
| `miss_retention_days` | u32 | `90` | How long a recorded miss is kept. `0` keeps it until purged by hand. |

**Refused at load**, because each is a contradiction an operator should see
at boot rather than discover from a log:

| Condition | Why |
|---|---|
| `enabled = true` with `[proxy]` or a registry's `[registries.proxy]` | An egress proxy is a route off the site. |
| `enabled = true` with `warm_packages` or `warm_paths` on any registry | Warming fetches from an upstream this mode guarantees will never be dialled. Seed with a bundle instead. |
| `enabled = true` with `bundle_trusted_keys = []` | Import would accept any bundle. |
| A `bundle_trusted_keys` entry that is not 64 hex characters | An unusable key must not read as "signing is configured". |
| `synthesise_listings = true` with `enabled = false` | The key has no effect on a connected instance and reads as if this one answered listings offline. |

The same hex check now applies to every registry's
`[registries.signing].trusted_keys`, which had none: a typo there used to
surface as a `502` on the first download and named nothing.

**Warned about**, because each is legitimate and none means what it looks
like: a hybrid registry under `enabled = true` behaves as local (its
fall-through can never reach upstream — `air-gap.hybrid-registry`);
`bundle_trusted_keys` on a connected instance authorise imports only, which
is how a bundle is staged (`air-gap.keys-unused`); and a `deb`, `rpm`,
`pacman`, `jetbrains` or `generic` registry under `enabled = true` gets no
synthesised index — a signed `Packages` file cannot be re-signed here — so
its listing stays a `503` while a held file is served by path
(`air-gap.listing-not-synthesised`).

**What an operator sees.** A miss is `503` with
`{"code": "content_unavailable", "registry", "coordinate", "bundle_hint"}`;
a host nothing mirrors, reached through the catch-all rewrite rule, is `501`
at `/_air-gap/unmirrored/{host}/…` and fetches nothing. A coordinate an
administrator **blocked** answers `403` and is never recorded as missing — a
blocked package is not a gap in the mirror. See
[the air-gap runbook](/operations/air-gap) and
[pointing mise at BatleHub](/use/mise).

## 4. Permissions Reference

### Roles

Three built-in roles are evaluated with inheritance: `admin` inherits all `user` permissions, `user` inherits all `anonymous` permissions. This means if `anonymous` can do `releases:read`, admins can too without repeating the permission.

| Role | Description |
|---|---|
| `anonymous` | Unauthenticated request, or no auth provider matched |
| `user` | Successfully authenticated via any provider |
| `admin` | Full access |

### Permission strings

| Permission | Meaning |
|---|---|
| `releases:read` | List releases and download release assets |
| `source:read` | Download source tarballs |
| `quarantine:read` | See that a version is held or denied, its reason codes and when it becomes available (default: `user`, `admin`) |
| `findings:read` | See the findings behind those codes — CVE ids, scanner output, SOC text (default: `admin`) |
| `*` | All permissions (wildcard) |

### Group-based permissions

Groups supplement role permissions — a request passes if it satisfies either the role check or any group check. Permissions from roles and groups are additive (union).

Group names in `[registries.rbac.groups]` are matched against the namespaced group strings produced by auth providers:

- **Exact match:** `"oidc:team-a"` — only matches `team-a` from the provider named `"oidc"`
- **Wildcard prefix:** `"*:team-a"` — matches `team-a` from any provider (`oidc:team-a`, `kubernetes:team-a`, etc.)

Example:

```toml
[registries.rbac.groups]
"oidc:developers" = ["releases:read", "source:read"]
"*:ops"           = ["*"]
```

---

## 5. Environment Variable Overrides

BatleHub supports two complementary mechanisms for injecting environment variable values into the config file.

### 5.1 Inline substitution — `${VAR_NAME}` {#env-inline}

Write `${VAR_NAME}` anywhere inside a TOML **string value**. BatleHub replaces every placeholder with the corresponding environment variable's value before the TOML is parsed. This is the recommended way to inject secrets such as OIDC client secrets, upstream auth tokens, or passwords.

**Rules:**

| Syntax | Meaning |
|---|---|
| `${VAR_NAME}` | Replaced with `$VAR_NAME` at startup. Error if the variable is not set. |
| `$${VAR_NAME}` | Produces the literal string `${VAR_NAME}` — no lookup performed. |
| Any other `$` | Left unchanged. |

> If a referenced variable is not set, BatleHub exits immediately with a clear error message naming the missing variable. There is no silent fallback or empty-string default — this is intentional to prevent misconfigured deployments from starting.

**OIDC client secret:**

```toml
[[auth]]
type = "oidc"
issuer_url = "https://sso.example.com/application/o/batlehub/"
client_id   = "batlehub"
client_secret = "${OIDC_CLIENT_SECRET}"   # export OIDC_CLIENT_SECRET=<value>
redirect_uri  = "https://hub.example.com/api/v1/auth/oidc/callback"
```

**Upstream registry — Bearer token:**

```toml
[[registries]]
type = "npm"
name = "internal-npm"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/npm"]

[registries.upstream_auth]
type  = "bearer"
token = "${INTERNAL_NPM_TOKEN}"   # export INTERNAL_NPM_TOKEN=npat-xxxx
```

**Upstream registry — Basic auth:**

```toml
[[registries]]
type     = "cargo"
name     = "internal-cargo"
upstreams = ["https://nexus.corp.example.com/repository/cargo-proxy/"]

[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "${INTERNAL_CARGO_PASSWORD}"   # export INTERNAL_CARGO_PASSWORD=s3cr3t
```

**Upstream registry — Custom header:**

```toml
[[registries]]
type     = "npm"
name     = "api-keyed-npm"
upstreams = ["https://nexus.corp.example.com/repository/npm-proxy/"]

[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "${INTERNAL_NPM_API_KEY}"   # export INTERNAL_NPM_API_KEY=my-api-key
```

**Kubernetes / Docker Compose:** mount a Secret as an env var and reference it from the config file.

```yaml
# docker-compose.yml
services:
  batlehub:
    env_file: .env.secrets   # OIDC_CLIENT_SECRET=...
    volumes:
      - ./config.toml:/etc/batlehub/config.toml:ro
```

```yaml
# Kubernetes Deployment
env:
  - name: OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef:
        name: batlehub-secrets
        key: oidc-client-secret
```

**Escaping:** if a config value legitimately needs the string `${...}` (e.g. a URL template), write `$${...}`:

```toml
# This stores the literal string "${MY_VAR}" — no variable lookup:
some_template = "$${MY_VAR}/suffix"
```

---

### 5.2 Named overrides — `PROXY_CACHE__*` {#env-named}

A fixed set of top-level fields can also be overridden via named environment variables. These are useful for container deployments where the config file is baked into the image and you need to tweak infrastructure addresses (host, port, DB URL) without rebuilding.

| Variable | Config field | Notes |
|---|---|---|
| `PROXY_CACHE__SERVER__HOST` | `server.host` | |
| `PROXY_CACHE__SERVER__PORT` | `server.port` | Parsed as u16 |
| `PROXY_CACHE__SERVER__STATIC_DIR` | `server.static_dir` | |
| `PROXY_CACHE__DATABASE__URL` | `database.url` | |
| `PROXY_CACHE__DATABASE__MAX_CONNECTIONS` | `database.max_connections` | Parsed as u32 |
| `PROXY_CACHE__STORAGE__PATH` | `storage.path` | Single filesystem backend only |
| `PROXY_CACHE__STORAGE__BUCKET` | `storage.bucket` | Single S3 backend only |
| `PROXY_CACHE__STORAGE__REGION` | `storage.region` | Single S3 backend only |
| `PROXY_CACHE__STORAGE__ENDPOINT_URL` | `storage.endpoint_url` | Single S3 backend only |
| `PROXY_CACHE__OTEL__ENDPOINT` | `otel.endpoint` | Creates the `[otel]` section if absent |
| `PROXY_CACHE__OTEL__SERVICE_NAME` | `otel.service_name` | |
| `PROXY_CACHE__PROXY__URL` | `proxy.url` | Creates the `[proxy]` section if absent; applies to all registries |
| `PROXY_CACHE__PROXY__USERNAME` | `proxy.username` | |
| `PROXY_CACHE__PROXY__PASSWORD` | `proxy.password` | |
| `PROXY_CACHE__PROXY__NO_PROXY` | `proxy.no_proxy` | |

> Storage env-var overrides only work with the **single-backend** `[storage]` form. Multi-backend configs (`[[storage.backends]]`) must be changed in the file.

> **Choosing between the two mechanisms:** use `${VAR_NAME}` placeholders for **secrets** (auth tokens, passwords, client secrets) — they work for any field and keep credentials out of the TOML file. Use the `PROXY_CACHE__*` variables for **infrastructure addresses** (database URL, storage path, host/port) where the value is not secret but varies between environments.

---

---

## 6. What used to be here

This page is the configuration reference, and for a long time it was also six
other documents: worked examples, the server binary's subcommands, personal API
tokens, hot reload, private upstreams, and a second copy of the SBOM
documentation. At 15 706 words it was a quarter of everything published, and a
subsection numbered 6.16 had been sitting inside section 11 for long enough that
nobody could say which of the two was wrong (RFC 0005-bis).

They are pages now, findable by name:

| What | Where |
| --- | --- |
| Worked examples — complete `config.toml` files per scenario | [Worked examples](/guide/configuration-examples) |
| `batlehub dump-spec`, `batlehub hash-token` | [Server binary subcommands](/guide/server-cli) |
| Creating and revoking your own API token | [Using BatleHub → tokens](/use/#tokens-api) |
| Reloading configuration without a restart | [Hot reload](/guide/hot-reload) |
| Proxying a private or self-hosted upstream | [Private upstreams](/guide/private-upstreams) |
| SBOM generation, endpoints and PURL mapping | [SBOM](/guide/sbom) |
| Sizing an instance | [Capacity planning](/guide/capacity-planning) |
