# Host-based registry routing

Every registry is always reachable at a subpath:

```
https://hub.example.com/proxy/npm1/…
```

Host-based routing adds a **second ingress**: one or more hostnames whose *root*
is the registry.

```
https://npm1.hub.example.com/…      # wildcard, derived from the registry name
https://npm.acme.io/…               # explicit vanity host
```

The subpath keeps working, unchanged, for every registry. The host is an
*additional* way in — nothing changes for a registry you do not configure one for.

## Why

- **Ecosystems that assume they own the origin root.** Cargo publishes to
  `/api/v1/crates/new`, GitLab exposes `/api/v4/…`, Forgejo `/api/packages/…`.
  These work behind the path prefix today only because BatleHub replicates the
  whole shape under `/proxy/{registry}`. A dedicated host removes the class of
  problem.
- **Absolute URLs in metadata.** NuGet service indexes, PyPI simple pages,
  Composer `packages.json`, npm `dist.tarball` and Terraform `download_url` all
  embed self-referencing URLs. On a registry host they become short and stable.
- **Operational ergonomics.** A vanity host is something you hand to a team, put
  in a `settings.xml`, and keep stable while the backend moves. It also lets you
  apply per-host WAF rules, rate limits or TLS policies at the ingress without
  teaching the ingress about BatleHub's path scheme.

## Before / after

::: code-group

```ini [.npmrc — subpath]
registry=https://hub.example.com/proxy/npm1/
//hub.example.com/proxy/npm1/:_authToken=…
```

```ini [.npmrc — vanity host]
registry=https://npm.acme.io/
//npm.acme.io/:_authToken=…
```

:::

::: code-group

```toml [.cargo/config.toml — subpath]
[registries.internal]
index = "sparse+https://hub.example.com/proxy/cargo1/registry/"
```

```toml [.cargo/config.toml — vanity host]
[registries.internal]
index = "sparse+https://cargo.acme.io/registry/"
```

:::

## Configuration

```toml
# Derive a host for every registry from its name.
[subdomain_routing]
enabled     = true
base_domain = "hub.example.com"   # npm1.hub.example.com -> registry "npm1"
scheme      = "https"             # used only to render public URLs in the API/UI

[[registries]]
name         = "npm1"
type         = "npm"
hosts        = ["npm.acme.io"]    # optional extra vanity hosts
path_routing = true               # default; false => the host is the only ingress
```

- `[subdomain_routing]` is optional. Absent or `enabled = false` derives no
  wildcard hosts.
- `hosts` is independent of it: a registry can have vanity hosts with no wildcard
  configured at all.
- `scheme` **never affects routing**. It only decides whether the API advertises
  `https://npm.acme.io` or `http://npm.acme.io`.
- Incoming hosts are normalised before lookup — lowercased, port stripped,
  trailing dot stripped. `NPM.Acme.io:8443.` and `npm.acme.io` are the same host.

## The host belongs to the registry — entirely

**On a registry host, every path is the registry's.** There is no passthrough
allowlist.

```
GET https://cargo1.hub.example.com/api/v1/crates/new
  -> /proxy/cargo1/api/v1/crates/new     ✅ cargo publish

GET https://cargo1.hub.example.com/api/v1/registries
  -> /proxy/cargo1/api/v1/registries     ❌ 404 — the admin API is on the main host
```

This is forced, not a preference: cargo (`/api/v1/…`), GitLab (`/api/v4/…`) and
Forgejo (`/api/packages/…`) all legitimately serve paths under `/api`, so any
reserved prefix would shadow a real registry route. The same argument applies to
`/healthz` and `/metrics` — a `generic` or `deb` registry can legitimately mirror
those paths.

::: warning Point probes and scrapes at the main host
`/healthz`, `/metrics`, the admin API and the SPA are served on the bare
`base_domain` only.
:::

::: tip Pick one ingress per client
`https://npm1.hub.example.com/proxy/npm1/lodash` becomes
`/proxy/npm1/proxy/npm1/lodash` and 404s.
:::

## Generated URLs follow the ingress

Every self-referencing URL BatleHub generates reflects the ingress the client
actually used:

```jsonc
// GET https://npm.acme.io/lodash
{ "dist": { "tarball": "https://npm.acme.io/lodash/-/lodash-4.17.21.tgz" } }

// GET https://hub.example.com/proxy/npm1/lodash
{ "dist": { "tarball": "https://hub.example.com/proxy/npm1/lodash/-/lodash-4.17.21.tgz" } }
```

The same holds for the NuGet service index and registration `@id`s, the PyPI
simple index, Composer `metadata-url` and `dist`, the Terraform provider
`download_url`, and the cargo index `dl` / `api`.

`GET /api/v1/registries` reports each registry's preferred URL as `public_url`
(the first explicit host, else the wildcard host), and the Setup Guide uses it in
every snippet.

## `path_routing = false` — the host as the only ingress

```toml
[[registries]]
name         = "npm1"
hosts        = ["npm.acme.io"]
path_routing = false        # /proxy/npm1/… -> 404
```

The motivation is isolation: once a team is handed `npm.acme.io`, you may not
want the same content answering on the shared main host, where it inherits that
host's CORS policy, WAF rules and cache keys, and where a URL leaked from one
ingress silently keeps working on the other.

- Default is `true`; existing configs are untouched.
- A registry with `path_routing = false` and no reachable host is a **config
  error** — it would be a registry nothing can talk to.
- The subpath returns **404**, not 403: a disabled ingress should look absent,
  not forbidden. It is indistinguishable from an unknown registry.
- This is isolation, not authorisation. It closes an ingress; it does not grant
  or revoke access, and a user who can reach the registry by host can reach
  exactly what they could before.

## Proxy trust — required

Routing now depends on a header. Behind a reverse proxy the host arrives as
`Forwarded` / `X-Forwarded-Host`; directly exposed it is `Host`. Which of those
BatleHub believes is a routing decision, so it has to be stated:

```toml
[server]
# CIDR ranges (or bare IPs) of the reverse proxies in front of BatleHub.
trusted_proxies = ["10.42.0.0/16", "192.168.1.10"]
```

| `trusted_proxies` | Peer | Host used for routing + URLs | Client IP |
| --- | --- | --- | --- |
| absent | any | forwarded, else `Host` | TCP peer |
| `[]` | any | `Host` header only | TCP peer |
| `["10.42.0.0/16"]` | in range | forwarded, else `Host` | first `X-Forwarded-For` |
| `["10.42.0.0/16"]` | out of range | `Host` header only | TCP peer |

::: danger Configuring host routing with no trusted-proxy policy is a startup error
Routing on a header the server has no stated policy about is not a state a
deployment should reach. The error message contains the exact TOML to paste.

For deployments *without* host routing, an absent list keeps the pre-existing
behaviour — tightening it by default would silently change the URLs they already
advertise.
:::

**Use CIDR ranges, not exact IPs.** A Kubernetes ingress sits behind a pod CIDR
that changes on every rollout. A bare address is accepted and treated as a `/32`
(`/128` for IPv6), so every value that was valid for the deprecated
`[ip_blocking].trusted_proxies` stays valid.

::: info The deprecated key still works
When `[server].trusted_proxies` is absent, `[ip_blocking].trusted_proxies` is
used — and then governs the forwarded host and scheme as well as the client IP,
including satisfying the requirement above. So a deployment that already declares
it can adopt host routing without touching its proxy-trust config; it just gets a
config warning nudging the one-line move. When both are set, `[server]` wins.
:::

**Spoofing gains nothing.** Forging a host to reach registry *B* is exactly
equivalent to requesting `/proxy/B/…`, which any client can already do.
Authorisation is evaluated on the registry, by the same RBAC rules, after the
rewrite. There is no route reachable by host that is not reachable by path.

## Operator prerequisites

1. A DNS record per host, or a wildcard `*.hub.example.com`.
2. A certificate covering it — a wildcard certificate for the `base_domain` case.
   With `cert-manager`, the wildcard needs a DNS-01 solver.
3. A reverse proxy that forwards the original `Host` header.
4. `[server].trusted_proxies` listing that proxy's CIDR ranges.

### Helm

```yaml
ingress:
  enabled: true
  host: batlehub.example.com
  extraHosts:
    - "*.batlehub.example.com"
    - "npm.acme.io"
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com
        - "*.batlehub.example.com"   # the SAN must cover the wildcard

config:
  server:
    trusted_proxies: ["10.42.0.0/16"]   # your ingress controller's pod CIDR
  subdomain_routing:
    enabled: true
    base_domain: "batlehub.example.com"
```

Find the pod CIDR with:

```sh
kubectl cluster-info dump | grep -m1 cluster-cidr
```

## Validation

Rejected at startup and on every reload:

| Condition | Why |
| --- | --- |
| `enabled = true` with no `base_domain` | the section would route nothing |
| the same host claimed by two registries | ambiguous; last-write-wins would be invisible |
| a `hosts` entry colliding with another registry's wildcard host | same ambiguity, harder to spot |
| a `hosts` entry equal to `base_domain` | would shadow the main host and hide the admin API |
| a `hosts` entry containing `/`, a scheme prefix, or empty after trimming | not a hostname |
| `path_routing = false` on a registry with no reachable host | the registry would be unreachable entirely |
| host routing with no trusted-proxy policy | routing would depend on an ungoverned header |

Warned about but accepted, and surfaced at
`GET /api/v1/admin/config/warnings` and on the Configuration admin page:

| Condition | Behaviour |
| --- | --- |
| a registry name that is not a valid DNS label (`my_registry`, `Foo.Bar`) | no wildcard host is derived for it; it stays reachable by path and by any explicit `hosts` entry |
| host routing satisfied only by `[ip_blocking].trusted_proxies` | accepted and honoured; move the list to `[server]` |

## Rollback

A config edit plus a hot reload — the host table is hot-reloadable like every
other registry-scoped map, and nothing is persisted. With no
`[subdomain_routing]` and no `hosts`, the table is empty, the middleware is a
no-op, and every generated URL is byte-identical to a deployment that never had
the feature.
