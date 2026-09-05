# Terraform

Proxy and cache the Terraform provider and module registry protocol (v1 API), or host private modules and providers. BatleHub serves provider version listings, provider download info, module version listings, and module source downloads, gated by RBAC and the release-age gate.

## At a glance

| | |
|---|---|
| **Config type** | `terraform` |
| **Default upstream** | `registry.terraform.io` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ module + provider upload |
| **Air gap** | offline, provider and module `versions` are composed from the held set, and a provider's `download` document for a platform whose archive, `shasums` and `shasums.sig` are all held — the plan must name all three paths — with the publisher's signing keys carried on the bundle's manifest by `mise export`; `terraform init` verifies the publisher's signature as it does connected, and this instance signs nothing. A platform missing any of the three is a `503` |

## Proxy setup

BatleHub speaks **both** Terraform protocols. They are not alternatives — pick
by what you need and how your instance is reached.

### Provider network mirror — works anywhere

A mirror serves **providers only**, needs no service discovery, and works under
ordinary path routing. This is the simplest option and the right one for an
air-gapped estate that only needs to cache public providers.

```hcl
# ~/.terraformrc  (%APPDATA%/terraform.rc on Windows)
provider_installation {
  network_mirror {
    url = "https://batlehub.example.com/proxy/<registry>/"
  }
}

credentials "batlehub.example.com" {
  token = "<your-token>"
}
```

The `{hostname}` segment in a mirror URL names the *origin* registry, and
BatleHub checks it against the registry's configured upstream: pointing a mirror
for `registry.terraform.io` at a registry that mirrors something else returns
`404` rather than silently attaching the wrong provenance.

::: warning Two things Terraform requires of a mirror
Both measured against Terraform 1.8.5.

**The mirror must be an `https:` URL.** Terraform refuses a plain-HTTP mirror
outright — *"the mirror must be at an https: URL"* — so a local instance on
`http://localhost:8080` cannot be used as one at all.

**Terraform does not authenticate the provider download.** It sends the token
from your `credentials` block to the mirror's `index.json` and `{version}.json`,
and then fetches the provider archive **without credentials**. The same is true
of the registry protocol, and of the `SHA256SUMS` and `.sig` it fetches
alongside the archive: measured against Terraform 1.8.5, every protocol document
is authenticated and every artifact fetch is not — including on the host it
authenticated one request earlier.

You have two ways to live with that, and the second is new:

- **Open the registry** — `anonymous = ["releases:read", "source:read"]` under
  `[registries.rbac]`, or an authenticating ingress in front of it. This is the
  blunt option: the grant is per *registry*, so opening the last step of one
  provider install opens every version listing and, in hybrid mode, everything
  published locally.
- **Sign the downloads** — [`signed_downloads = true`](#signed-downloads),
  which lets you keep `anonymous = []`. BatleHub puts a short-lived,
  single-coordinate signature inside the document Terraform *did* authenticate,
  and accepts it on the fetches that carry no header.

The [VS Code gallery](/registries/vscode-marketplace) has the same constraint
for the same reason, and does not yet have the second option.
:::

### Registry protocol — requires host routing

The registry protocol serves **modules and providers**, and Terraform reaches it
by name: `source = "<host>/<namespace>/<type>"`. That is exactly three segments,
so `batlehub.example.com/proxy/<registry>/myorg/mycloud` is not a legal source
address — it has five.

Terraform also finds a registry's endpoints by fetching
`https://<host>/.well-known/terraform.json`, which is host-rooted by the
protocol. Both facts point the same way: **the registry protocol needs the
registry bound to its own hostname**. Configure that with `[subdomain_routing]`
or a vanity host (see [Host-based routing](/guide/host-routing)),
then:

```hcl
# ~/.terraformrc
credentials "tf.example.com" {
  token = "<your-token>"
}
```

```hcl
# main.tf
terraform {
  required_providers {
    mycloud = {
      source  = "tf.example.com/myorg/mycloud"
      version = "~> 1.0"
    }
  }
}

module "consul" {
  source  = "tf.example.com/hashicorp/consul/aws"
  version = "0.1.0"
}
```

On a path-routed request `/.well-known/terraform.json` answers `404` with the
reason, rather than guessing which of the registries under that host it should
describe.

::: warning The host must be HTTPS, and BatleHub must know it
Terraform will not speak plaintext to a registry host and offers no opt-out —
the same rule as the network mirror above. Behind a TLS terminator, BatleHub
also has to be told that the client's scheme was `https`, because it writes
absolute URLs into the download document from what it sees: without a trusted
`X-Forwarded-Proto` it advertises `http://<host>` and Terraform then fails
trying to reach it. Set the terminator's address in `trusted_proxies`:

```toml
[server]
trusted_proxies = ["10.42.0.0/16"]   # your ingress's CIDR ranges
```

Host routing refuses to start without an explicit stance here — `[]` if
BatleHub is exposed directly — so the failure is a startup error rather than a
silently wrong URL.
:::

::: tip Downloads go through the proxy
Whichever protocol you use, the archive URL BatleHub hands Terraform points back
at BatleHub — never at the upstream CDN. That is what puts provider and module
bytes through the policy gate, the cache and the audit trail. Earlier releases
forwarded the upstream URL, so the download bypassed all three.
:::

## Publishing (local / hybrid)

BatleHub supports both **provider** and **module** private registries. Modules use a simple tarball upload. Providers follow a two-step process: upload a version manifest (JSON describing platforms and checksums), then upload each platform binary.

### Server configuration

```toml
[[registries]]
type = "terraform"
name = "internal-tf"
mode = "local"          # or "hybrid" to fall back to registry.terraform.io

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

For hybrid mode add `upstreams = ["https://registry.terraform.io"]`.

### Closing the registry with signed downloads {#signed-downloads}

Terraform fetches the provider archive, its `SHA256SUMS` and the detached
signature over that **with no `Authorization` header**, and has no mechanism to
send one. Without help, the only way to make an install work is to grant
anonymous reads across the whole registry.

`signed_downloads` removes that trade. BatleHub mints a signature into the
document Terraform *did* authenticate, and accepts it on the three fetches that
carry no header:

```toml
[server.signed_urls]
# 32 bytes minimum. Interpolated from the environment like every other
# credential in this file — see "Sensitive values" in the configuration guide.
secret      = "${BATLEHUB_URL_SIGNING_SECRET}"
ttl_seconds = 300                # default; hard-capped at 3600

[[registries]]
type             = "terraform"
name             = "internal-tf"
signed_downloads = true

[registries.rbac]
anonymous = []                   # now possible
user      = ["releases:read", "source:read"]
```

What the signature is, precisely: a five-minute capability for **one registry,
one package, one version, one platform, one method**. It carries the identity
that fetched the document, and verification hands that identity to the same rule
chain, quota and audit as any other download. It authenticates a request; it
authorises nothing. A version blocked after the URL was minted stays blocked,
because the block is evaluated when the URL is redeemed.

Three consequences worth knowing before you turn it on:

- **`GET /api/v1/audit` names the user** for provider downloads, where it
  previously recorded no actor at all — with `anonymous` granted, the rule chain
  was evaluating *anonymous*, so group grants never applied and quota was
  charged to nobody.
- **The token can reach your logs — but not BatleHub's own.** BatleHub's request
  span sets `http.target` from the request's *path only*, deliberately (see the
  note below). Anything else on the path that logs a full URL still sees the
  signature for its lifetime.
- **`signed_downloads = true` with no `[server.signed_urls].secret` is a startup
  error**, not a warning. A registry that believes it is closed and is not is
  exactly the failure this feature exists to prevent.

::: warning The signature can reach logs that are not BatleHub's
A minted URL is a bearer capability until it expires, so anything that records a
full request URL records the token. `tracing-actix-web`'s own span builder sets
`http.target` from the request's path *and query*, which is why BatleHub does not
use it: `BatleHubSpanBuilder` (`server/src/server_factory.rs`) is a field-for-field
re-implementation whose one deviation is `http.target = uri.path()`, and a test
asserts the span target never carries a query string.

That covers this server and nothing else. A reverse proxy terminating TLS in
front of BatleHub, a CDN, or Terraform's own `TF_LOG=DEBUG` output will each
capture the whole URL. What that is worth to whoever reads those logs is bounded
— five minutes by default, one file, and no permission the signed-for user did
not already have. If that is still more than you want, the levers are: lower
`ttl_seconds`, and check the access-log configuration of whatever sits in front.
The audit trail itself is clean: `access_events` records the package coordinate,
never the URL.
:::

**Rotating the secret** needs no restart and no flag day. Put the new secret in
`secret`, move the old one to `previous_secrets`, and reload: URLs minted under
either verify, and only the current one mints. Drop the old entry once the
longest `ttl_seconds` has passed.

```toml
[server.signed_urls]
secret           = "${BATLEHUB_URL_SIGNING_SECRET}"
previous_secrets = ["${BATLEHUB_URL_SIGNING_SECRET_OLD}"]
```

An entry that interpolates to empty is ignored, so the `previous_secrets` line
can stay in the file between rotations — but the variable must still be *set*.
`${VAR}` expansion runs before the config is parsed and refuses an unset
variable, so once the old secret is retired either remove the line or keep
`BATLEHUB_URL_SIGNING_SECRET_OLD=""` exported.

### Publishing modules

A Terraform module is a `.tar.gz` archive of the module directory.

```sh
# Build the archive
tar -czf consul-aws-0.1.0.tar.gz -C /path/to/module .

# Upload
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/gzip" \
  --data-binary @consul-aws-0.1.0.tar.gz \
  "https://batlehub.example.com/proxy/internal-tf/v1/modules/hashicorp/consul/aws/0.1.0"
```

### Using a private module

Add credentials to `~/.terraformrc`:

```hcl
credentials "batlehub.example.com" {
  token = "<your-token>"
}
```

Reference the module in Terraform. A module source is `<host>/<namespace>/<name>/<provider>`,
so this needs the registry bound to its own hostname (see
[Registry protocol](#registry-protocol-—-requires-host-routing) above):

```hcl
module "consul" {
  source  = "tf.example.com/hashicorp/consul/aws"
  version = "0.1.0"
}
```

### Publishing providers

**Step 1 — Upload version manifest** (JSON describing the version and its platforms):

```sh
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/json" \
  -d '{
    "version": "1.0.0",
    "protocols": ["5.0"],
    "platforms": [
      {
        "os": "linux", "arch": "amd64",
        "filename": "terraform-provider-mycloud_1.0.0_linux_amd64.zip",
        "shasum": "<sha256-hex>"
      }
    ]
  }' \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/myorg/mycloud/versions"
```

**Step 2 — Upload platform binaries**:

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @terraform-provider-mycloud_1.0.0_linux_amd64.zip \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/myorg/mycloud/1.0.0/artifact/linux/amd64"
```

Repeat the binary upload for each supported platform.

### Using a private provider

```hcl
# ~/.terraformrc
credentials "tf.example.com" {
  token = "<your-token>"
}
```

```hcl
# main.tf
terraform {
  required_providers {
    mycloud = {
      source  = "tf.example.com/myorg/mycloud"
      version = "~> 1.0"
    }
  }
}
```

### Yank a version (admin)

Use the admin bulk-operations API (see [Administration guide](/guide/administration)):

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages": [{"name": "modules/hashicorp/consul/aws", "versions": ["0.1.0"]}]}' \
  "https://batlehub.example.com/api/v1/admin/registries/internal-tf/bulk-yank"
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/terraform -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/.well-known/terraform.json` | `GET /.well-known/terraform.json` — the document Terraform reads first. |
| `GET` | `/proxy/{registry}/.well-known/terraform.json` | The same document at the path the host-routing middleware actually produces. |
| `GET` | `/proxy/{registry}/{hostname}/{namespace}/{ptype}/{version}.json` | `GET {mirror}/{hostname}/{namespace}/{type}/{version}.json` — where one |
| `GET` | `/proxy/{registry}/{hostname}/{namespace}/{ptype}/index.json` | `GET {mirror}/{hostname}/{namespace}/{type}/index.json` — the versions a |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}` | `GET /v1/modules/{ns}/{name}/{provider}/{version}` — one module version's |
| `POST` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}` | Upload a Terraform module tarball to the local registry. |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/artifact` | Download the tarball for a locally-published Terraform module. |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/download` | Get the download URL for a specific Terraform module version. |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions` | List available versions for a Terraform module. |
| `DELETE` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}` | Yank a Terraform module version (local/hybrid registries only). |
| `POST` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}/unyank` | Unyank a Terraform module version (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}` | Download a Terraform provider platform binary from local storage. |
| `PUT` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}` | Upload a platform binary for a locally-published Terraform provider. |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/download/{os}/{arch}` | Get download information for a specific Terraform provider version and platform. |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums` | The provider's checksum manifest (`SHA256SUMS`) and its detached signature. |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig` | The detached signature over the checksum manifest. See |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions` | List available versions for a Terraform provider. |
| `POST` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions` | Upload a Terraform provider version manifest (JSON describing version + platforms). |
| `DELETE` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}` | Yank a Terraform provider version (local/hybrid registries only). |
| `POST` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}/unyank` | Unyank a Terraform provider version (local/hybrid registries only). |
<!-- END endpoints -->

---

## Blocked versions

Both facets of `/v1/{namespace}/versions` are filtered — module versions
(nested under `modules[].versions`) and provider versions (a flat `versions`
array) — so `terraform init` never selects a version it will then be refused
mid-plan. Neither document names a preferred version, so there is nothing to
repair beyond removing the entry.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Terraform reads per-host credentials from the `credentials "batlehub.example.com"` block in `~/.terraformrc` (shown above) and sends the token as a Bearer header.

## Notes

- Providers are cached after first download in proxy/hybrid mode, or served entirely from local storage in local mode.
- The module upload response includes an `X-Terraform-Get` header pointing at the artifact download URL.
- Provider download responses always carry a `signing_keys` object. Terraform refuses a provider whose download document omits it, so the field is present (empty when the registry publishes no keys) rather than left out.
- `shasums_url` and `shasums_signature_url` still name the upstream in proxy mode. The provider *archive* is proxied and gated; its checksum manifest is not yet, so a fully offline provider install is not complete.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
