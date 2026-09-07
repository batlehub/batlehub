---
# The server-configuration reference: 4 100+ words of TOML surface, and it grew
# past the line when the console's page sizes and its content-security policy
# became things an operator sets. `docs:structure` asks for this declaration
# above 4 000 words — not a cap, a sentence someone had to type
# (RFC 0005-bis §4.5).
reference: true
---

# Server configuration

## Configuration {#configuration}

BatleHub reads a single TOML file, defaulting to `config.toml` in the working directory. Override the path with `--config /path/to/config.toml`.

### Loading order

1. TOML file is read from disk.
2. `${VAR_NAME}` placeholders inside string values are replaced with their environment variable values.
3. The resulting TOML is parsed.
4. Named `PROXY_CACHE__*` environment variable overrides are applied on top.
5. Registry names and types are validated.

### Secret injection with `${VAR_NAME}` {#env-inline}

Write `${VAR_NAME}` inside any TOML string value. BatleHub replaces the placeholder with the named environment variable before parsing. This works for **every field** — auth secrets, upstream tokens, passwords, and more.

::: danger Missing variable = startup failure
If a referenced variable is not set, BatleHub exits immediately with a clear error message naming the missing variable. There is no silent fallback or empty-string default.
:::

**OIDC client secret:**

```toml
[[auth]]
type          = "oidc"
issuer_url    = "https://sso.example.com/application/o/batlehub/"
client_id     = "batlehub"
client_secret = "${OIDC_CLIENT_SECRET}"   # export OIDC_CLIENT_SECRET=...
redirect_uri  = "https://hub.example.com/api/v1/auth/oidc/callback"
```

**Upstream registry credentials:**

```toml
# Bearer token (GitHub PAT, Gitea token, npm auth token)
[registries.upstream_auth]
type  = "bearer"
token = "${REGISTRY_TOKEN}"

# Basic auth (Nexus, Artifactory)
[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "${REGISTRY_PASSWORD}"

# Custom header (X-API-Key, etc.)
[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "${REGISTRY_API_KEY}"
```

**Kubernetes / Docker Compose injection:**

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

To write a literal `${...}` string (no variable lookup), escape the first `$`:

```toml
# Stores the literal string "${MY_VAR}" — no substitution performed:
some_field = "$${MY_VAR}"
```

### Named environment variable overrides {#env-named}

A fixed set of top-level fields can also be overridden with named env vars. Useful for tweaking infrastructure addresses (host, port, DB URL) in containerised deployments without modifying the config file.

| Variable | Config field |
|----------|-------------|
| `PROXY_CACHE__SERVER__PORT` | `server.port` |
| `PROXY_CACHE__SERVER__HOST` | `server.host` |
| `PROXY_CACHE__SERVER__STATIC_DIR` | `server.static_dir` |
| `PROXY_CACHE__DATABASE__URL` | `database.url` |
| `PROXY_CACHE__DATABASE__MAX_CONNECTIONS` | `database.max_connections` |
| `PROXY_CACHE__STORAGE__PATH` | `storage.path` (single filesystem backend) |
| `PROXY_CACHE__STORAGE__BUCKET` | `storage.bucket` (single S3 backend) |
| `PROXY_CACHE__STORAGE__REGION` | `storage.region` (single S3 backend) |
| `PROXY_CACHE__STORAGE__ENDPOINT_URL` | `storage.endpoint_url` (single S3 backend) |
| `PROXY_CACHE__OTEL__ENDPOINT` | `otel.endpoint` |
| `PROXY_CACHE__OTEL__SERVICE_NAME` | `otel.service_name` |

::: tip When to use which
Use **`${VAR_NAME}` placeholders** for secrets (auth tokens, passwords, client secrets) — they work for any field and keep credentials out of the TOML file entirely.

Use **`PROXY_CACHE__*` variables** for infrastructure addresses (database URL, storage path, host/port) where the value is not secret but varies between environments.
:::

### Minimal production config

```toml
[server]
host = "0.0.0.0"
port = 8080
static_dir = "/app/ui/dist"
cors_allowed_origins = ["https://batlehub.example.com"]

[database]
type = "postgresql"
url  = "${DATABASE_URL}"

[[auth]]
type = "token"

[[auth.tokens]]
value   = "${ADMIN_TOKEN}"
role    = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "/var/cache/batlehub"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

---

### Proxy trust {#trusted-proxies}

BatleHub sits behind a reverse proxy in most deployments, and three headers from
that proxy shape what it does: `Forwarded` / `X-Forwarded-Host` decides the host
in every generated URL (and, with
[host-based routing](/guide/host-routing), *which registry* serves the request),
`X-Forwarded-Proto` decides `http` vs `https`, and `X-Forwarded-For` decides the
client IP the fail2ban middleware counts violations against.

`[server].trusted_proxies` states which peers may set them:

```toml
[server]
# CIDR ranges (or bare IPs) of the reverse proxies in front of BatleHub.
trusted_proxies = ["10.42.0.0/16", "192.168.1.10"]
```

| Value | Behaviour |
| --- | --- |
| absent | forwarded host/scheme believed from any client, `X-Forwarded-For` ignored (the pre-existing default) |
| `[]` | forwarded headers ignored entirely — the `Host` header and the connection decide |
| `[nets]` | honoured only from peers inside those prefixes |

Use CIDR ranges rather than exact IPs: a Kubernetes ingress sits behind a pod
CIDR that changes on every rollout. A bare address is treated as a `/32`
(`/128` for IPv6).

::: warning Mandatory with host-based routing
Once `[subdomain_routing]` is enabled or any registry declares `hosts`, an absent
list is a startup error — routing would otherwise depend on a header the server
has no stated policy about. The error message contains the TOML to paste.
:::

::: info `[ip_blocking].trusted_proxies` is deprecated
It still works: when `[server].trusted_proxies` is absent it is used, and then
governs the forwarded host and scheme as well as the client IP — including
satisfying the requirement above. When both are set, `[server]` wins. Either way
you get a [config warning](#config-warnings).
:::

### Registry modes

Every registry can run in one of three modes:

| Mode | Behaviour |
|------|-----------|
| `proxy` | Default. Forwards all requests to upstream; publishing is rejected. |
| `local` | BatleHub is the only source. No upstream needed. Teams publish directly. |
| `hybrid` | Local-first. Serves locally-published packages; falls back to upstream for everything else. |

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"         # or "hybrid"

[registries.rbac]
user  = ["source:read"]
admin = ["*"]
```

---

### README capture {#readme-capture}

Every registry that has a notion of *a package as a thing a person reads about*
carries a README, and most carry a different one per version. BatleHub stores it
keyed by `(registry, name, version)` and renders it — sanitised, on the server —
on the package page and through `batlehub package readme`.

**Absent means on.** For the metadata-borne registry types the text is a field of
a document the proxy already fetches and parses, so the default costs one
deserialised field. Which types those are, and where each one's README comes
from, is in the [README support table](/registries/#readmes).

```toml
[registries.readme]
enabled         = true      # store and serve READMEs for this registry
from_archive    = true      # extract from the cached artifact when the metadata carries none
max_bytes       = 262144    # cap on stored source (256 KiB); larger is truncated and flagged
remote_images   = "strip"   # "strip" | "proxy"
remote_image_hosts = []     # under "proxy": which hosts may be fetched from; [] means every host
image_max_bytes = 2097152   # cap on one proxied image (2 MiB); larger is not served
```

- **`from_archive`** is the one part of the default that is not free. It rides
  the artifact read SBOM already performs when SBOM is on, and adds one storage
  read per newly-cached version when it is not. It is inert on a registry whose
  README is metadata-borne only, and on a `firewall_only` registry — which
  streams without buffering, so no artifact is ever cached to extract from.
  Both are warned about rather than rejected.
- **`max_bytes`** caps the *stored source*, after decompression. Truncation is
  recorded and shown to the reader, never silent. `0` with `enabled = true` is
  refused: it stores nothing while claiming to be on.
- **`remote_images`** decides what happens to an `<img>` pointing at a
  third-party host. `"strip"` (the default) replaces it with an inline chip
  carrying the alt text and the host, so the reader can see that an image was
  there and where it pointed. Rendering it would mean every console page view
  sending a request — with a `Referer` — to a host the package author chose,
  announcing that someone inside your network is reading about this package
  right now. There is deliberately **no `"allow"`**: the console's CSP is built
  into the document and the server may only ever *narrow* it (see
  [the console's policy](#csp)), so the setting could only ever produce broken
  images with no error anywhere.

  `"proxy"` renders the images, fetched by **this server** and served from this
  origin, so the reader's browser still never talks to a host the package author
  chose. What the panel receives is an `<img>` whose `src` points back here,
  carrying the image's *index* in that version's README rather than its URL:

  ```
  GET /api/v1/explore/packages/{registry}/{name}/{version}/readme-image/{n}
  ```

  **No caller ever supplies a URL.** The server resolves the index against the
  stored README, which is what keeps this from being an open image proxy for
  whatever a package author writes — there is no signing key to rotate and no
  list of CDNs to maintain. The fetch goes through the same SSRF guard artifact
  downloads use, validating every redirect hop, and the response type must be on
  a short allow-list. An image that cannot be got — a dead URL, a wrong type, one
  over the cap — falls back to the chip `"strip"` would have shown, which is a
  better answer than a broken-image icon.

  SVG is served, and it is the case worth stating: two-thirds of the images in
  real READMEs are SVG, so refusing them would make this setting render a third
  of a badge row. Each one goes through an XML allow-list that drops `<script>`,
  `<foreignObject>`, every `on*` handler and every external reference, **and**
  the response carries `Content-Security-Policy: default-src 'none'; …; sandbox`,
  which stops script even for a reader who opens the image in a new tab. Either
  control is sufficient on its own.
- **`remote_image_hosts`** narrows `"proxy"` to a named set of hosts. It is the
  middle setting between a beacon and a blank: a README that badges from
  `shields.io` and screenshots from somebody's personal domain gets the badges
  proxied and a chip for the screenshot, rather than an all-or-nothing choice.

  ```toml
  [registries.readme]
  remote_images      = "proxy"
  remote_image_hosts = ["img.shields.io", "badgen.net", "codecov.io"]
  ```

  An entry matches the host itself or any subdomain of it, so `shields.io`
  covers `img.shields.io` — and does **not** cover `notshields.io`, because the
  dot is required. Ports and userinfo are not part of the comparison. Anything
  not matched becomes the same chip `"strip"` produces, so the reader still sees
  that an image was there and where it pointed.

  **An empty or absent list means every host**, which is what `"proxy"` did
  before this setting existed: adding a key to your config must not change what a
  running instance already serves. Narrowing is one line, and it is checked in
  two places — when the page is rendered, and again before this server dials the
  host, so removing an entry takes effect on the next request rather than on the
  next render-cache miss.

  Inert under `"strip"`, where nothing is fetched at all.
- **`image_max_bytes`** caps **one proxied image**, separate from `max_bytes`,
  which caps the stored *text*. They are not the same number for the same
  reason, and sharing one would make raising either a decision about the other.
  The largest image in a survey of 150 real README image URLs was 1.6 MB against
  this 2 MiB default, so it is generous rather than restrictive. `0` with
  `remote_images = "proxy"` is refused — it serves nothing while claiming to
  render images — and anything over 16 MiB is refused too, because the bytes are
  held in memory while their type and size are checked.

Rendering is server-side, allow-listed and fuzzed. Blocked versions serve no
README (`403`, with the same reason the download path gives); yanked, deprecated
and unlisted versions serve theirs normally, because withdrawing a
recommendation is not withdrawing the documentation.

---

### The console's discovery read {#the-console-s-discovery-read}

Whether the package page may ask upstream about a package this instance holds
nothing of.

Without it, the console's own search — which finds packages and flags them
"not yet proxied" — links to a page that says *no versions yet*. With it, the
page lists the versions upstream knows about, marks every one **not held here**,
and shows the README where the protocol carries one.

**Absent means on.** It is inert on a `local`-mode registry (there is no
upstream to ask) and on the registry kinds that cannot be asked about a package
at all; both are warned about rather than rejected.

```toml
[registries.upstream_detail]
enabled           = true    # the console may ask upstream about a package we hold nothing of
max_versions      = 300     # cap on upstream-only versions returned for one package
negative_ttl_secs = 300     # how long an upstream "no such package" is remembered
```

- **There is no TTL of its own.** The document lands in the metadata cache under
  the key the proxy path already uses, so it obeys this registry's
  `cache.metadata_ttl_secs` and `cache.serve_stale`. A second, independently
  clocked expiry for the same bytes is how two caches come to disagree.
- **`max_versions`** bounds the *response*, not the fetch — the document is one
  document whatever its size. The page says when it was applied.
- **`negative_ttl_secs`** remembers an upstream `404`, so a bad URL, a typo or a
  crawler cannot turn every reload into an upstream request. A *connection
  failure* is not a fact about the package and is never remembered.

**Looking at a package is not downloading it.** The read fetches one metadata
document and nothing else: no artifact, no `package_statuses` row, no download
count, no `last_accessed`, no quota, no storage entry — and the package does not
appear in the catalogue because somebody looked at it. What leaves the instance,
and how to turn it off, is in [what leaves this
instance](/operations/egress#the-console-s-discovery-read).

---

### How long a list this server hands out {#per-page}

Both browse endpoints answer with one **page**, and these two keys are how long a
page is.

```toml
[limits]
versions_per_page = 100     # one package's versions; the default, 1–1000
packages_per_page = 20      # the catalog; the default, 1–1000
```

Each key has two readings, deliberately: it is what a caller that asks for no
`per_page` gets, **and** the most any caller may ask for. A separate ceiling and
default would be two numbers that can contradict each other, and the question an
operator actually has is one question — how much of a list this server will
build, hold in memory and serialise for one request.

They are **two keys and not one** because the two lists are not the same
question. A catalog row is a name and a handful of counts, and 20 of them is a
screenful. A version row costs a vulnerability read and a licence read before it
is serialised, and `@babel/plugin-transform-runtime` has 169 versions. An
operator sizing a screen should not be sizing a query at the same time.

The console treats them differently for the same reason, and it is worth knowing
which is which:

| List | What the console sends | Why |
| --- | --- | --- |
| `GET …/explore/packages` (catalog) | nothing | The catalog *is* the list, so the operator's number is the right one. A console asking for its own would make `packages_per_page` inert on the one screen it exists for. |
| `GET …/explore/packages/{registry}/{name}` (versions) | `per_page=25` | The version table sits above a README on a package page; 25 is how many rows fit there. `versions_per_page` is the ceiling over it. |

Either way the console sizes its pager from the `per_page` that comes back
rather than from what it asked for.

A request may ask for less and may ask for more, in which case it gets the
configured number rather than an error — the ask is not illegitimate, it is
simply more than this server hands out at once. What was applied always comes
back in the answer, so a caller pages rather than silently missing rows:

```json
"versions_page": { "page": 0, "per_page": 100, "total": 169,
                   "unfiltered_total": 169, "prerelease_total": 12,
                   "hidden_prereleases": 0 }
```

(the catalog's own envelope is flatter — `total`, `page`, `per_page` beside the
`items` — because it has one list and no filters to count against.)

On the version endpoint the other parameters narrow the list before it is paged —
`q=` filters on the version string, `prereleases=hide` drops pre-releases,
`version=` names one that must survive that filter and, when no `page` is asked
for, chooses the page holding it.

`0` is refused at startup for either key: it would answer every caller with an
empty list, and the failure would land on a page rather than on the operator.

::: warning A narrower answer than before
Before `versions_per_page` existed the version endpoint returned **every**
version it could assemble. A client that reads `versions` and assumes it has the
whole list now sees at most 100 of them unless it pages; the counts in
`versions_page` are what tell it there is more. The catalog is unaffected — it
has always paged, and `packages_per_page` only makes its 20 an operator's number
instead of a literal.
:::

---

### Serving the console {#serving-the-console}

`[server].static_dir` points at the built SPA, and the server serves it three
ways:

- **the document** (`/` and `/index.html`), with its policy narrowed to your
  configuration — see [below](#csp);
- **the files beside it**, straight off disk;
- **every other console URL** — `/packages/npm/chalk?version=4.0.2`, `/setup`,
  `/me/tokens` — with that same document, because a single-page application has
  one document and many URLs. Without this a pasted link, a reload or a bookmark
  answered `404`.

The fallback is deliberately narrow, because the way to get it wrong is to hand
the console to something that is not a browser. It answers **only** `GET`, and
never for:

| Not the console | Why |
| --- | --- |
| `/api`, `/proxy`, `/scalar`, `/metrics`, `/healthz`, `/livez` | this server's own paths — every registry protocol lives under `/proxy/{registry}/…`, and a registry host's paths are rewritten into that shape before routing, so a package manager's request cannot reach the fallback |
| `/assets/…`, `/fonts/…` | the build's own directories: a stale hashed asset must fail as an asset, not arrive as HTML a browser then tries to run as JavaScript |
| a dotted name at the root — `/favicon.ico`, `/logo.svg` | asked for by name; if it is not there, it is not there. The rule stops at the root, so `/packages/npm/lodash.merge` is still a link |

If your ingress already rewrites unknown paths to `index.html`, nothing changes —
those requests never reach the fallback.

---

### The console's content-security policy {#csp}

The console's document carries its own `Content-Security-Policy`, in a
`<meta http-equiv>` rather than a response header — the static-file service
behind it cannot carry a header of its own, and the three things this origin
serves need three different policies. The other two are sent as headers:

| Path | Policy | Why |
| --- | --- | --- |
| `/proxy/**` | `default-src 'none'; sandbox` | protocol documents can carry publisher-controlled strings, and a sandboxed document has no access to the console's origin or its stored tokens |
| `/scalar` | `default-src 'none'`, `script-src 'self'`, `connect-src 'self'` | the API reference loads nothing from anywhere but this server — see below |

#### The API reference makes no outbound requests {#scalar-self-hosted}

`/scalar` used to load its bundle from a public CDN, unversioned. It is now
served from this origin, out of the console's own build output
(`assets/scalar/standalone.js`). Three things follow, and the last one is a
behaviour change:

- **It works with no egress.** The reference used to be a blank page on any
  deployment that could not reach the internet — which is most private
  registries. Loading it no longer sends your operators' IP addresses anywhere,
  and no longer depends on a CDN being up.
- **The bundle is in `ui/pnpm-lock.yaml`**, so `pnpm audit`, postmortem and the
  SBOM all cover it. That code was always executed by your browser; it just was
  not declared anywhere a scanner could see it.
- **It needs `static_dir`.** A server configured without the console assets has
  no bundle to serve, so `/scalar` answers with a short page saying exactly that
  and how to fix it. It deliberately does **not** fall back to the CDN: that
  would quietly reinstate the third-party script on precisely the air-gapped
  deployments least able to reach it. The OpenAPI document stays embedded in
  that page either way, so `curl` on the URL still yields it, as does
  `batlehub dump-spec`.

`connect-src 'self'` is deliberate rather than incidental. The bundle calls
`api.scalar.com` on load; those URLs are compiled into it and no setting turns
them off, so the policy is what stops them. Nothing is lost — the generated spec
declares no `servers` block, so "Test Request" targets this server, which is the
one the page documents.

The policy is built with the console, and the server **narrows it to your
configuration** when it serves the document. Narrowing only ever *removes*
sources; nothing in a config file can add one. Today one source is decided this
way:

| Source | Kept when |
| --- | --- |
| `https://badge.socket.dev` | at least one registry has `[registries.feature_flags] socket_badge` on — which is the default, so it is dropped only when you have turned the badge off everywhere |

That is the difference between what the page *may* load and what it *does*: an
instance with the badge off everywhere used to ship a document announcing a
third-party origin it would never call. Turning the flag off now takes it out of
the policy too, on the next document load — no rebuild, and it follows a hot
reload.

The document is served with `Cache-Control: no-cache` for this reason: it
describes an instance whose configuration can change under it. The assets beside
it are untouched and still served by the file service.

::: info Why the server cannot widen it
A policy that could grow from config would let a wrong config file open the
console to an origin the build never allowed. The built policy is the maximum and
the server owns only subtraction — which is also why a deployment whose
`index.html` predates this behaviour serves its policy unchanged rather than
failing.
:::

---

### Fetching a version from the console {#console-fetch}

Whether a reader may ask this instance to fetch a version from the page that
told them it exists.

The discovery read above makes the package page honest about what it holds: it
lists every version upstream knows about and marks each one **not held here**.
That is a wall. This is the door — a **Fetch this version** button on those rows.

The catalogue has the same door. A search that finds a package upstream lists it
as an `upstream` row, and that row offers a button naming the version the
upstream search returned: **Fetch 4.17.21**. It is the same endpoint, the same
gates and the same audit row as the button on the package page; one switch turns
both off. Where a registry does not offer it, the listing draws nothing and the
package page states the reason.

```toml
[[registries]]
console_fetch = true   # default
```

**It admits nothing.** The button runs the same download a package manager would
run, under the caller's own identity, through every gate that download would
pass:

- the rules run — RBAC, the block list, the release-age gate, the licence gate,
  `require_signed_release`, the version gate. A refusal shows the rule's own
  reason, the same string the download would have given, so the console's RBAC
  simulator (`POST /api/v1/admin/access-check`) explains the same verdict;
- integrity verification runs, including `block_on_mismatch`. Bytes that fail
  their advertised checksum are not stored;
- quota is consumed where quota applies;
- **the access event is recorded, with the caller as the actor.** That is the
  difference from a page view in one line: a page view has no actor because
  nobody decided anything. A fetch has one, and the audit log names them;
- SBOM and README extraction run, because the artifact lands in storage through
  the ordinary path. A version fetched from the page therefore gains its licence,
  its dependency manifest and its archive-borne README — and `not scanned`
  becomes a real answer once the scanner next runs.

The switch exists for the operator who wants the console strictly read-only,
which is a legitimate posture and not one the software should have to guess at.
It is inert on a `local`-mode registry: there is no upstream to fetch from.

The button is **not shown** where "fetch this version" has no single meaning —
Maven's artifact is a set of files, a Terraform provider needs an OS and an
architecture, a PyPI version is an sdist plus a wheel per interpreter and
platform, a conda artifact needs a channel platform and a build string — and the
page says why rather than showing a disabled button with no explanation. Which
kinds those are is the *Fetchable* column of the
[README support table](/registries/#readmes).

See [what leaves this instance](/operations/egress#someone-presses-fetch).

---

### Searching README prose {#search-readmes}

Whether the catalogue's search can match what a package **says** as well as what
it is called.

The search box matches names. That answers *"do we have something called
`retry`"* and cannot answer *"which of our internal libraries does exponential
backoff"* — which is the question a developer actually arrives with, and the one
an internal package page is the only place in the world that could answer.

```toml
[search]
readmes     = false     # default: names only
text_config = "english" # the Postgres text search configuration
```

- **Off by default.** Unlike README *capture*, which defaults on because it costs
  one already-parsed field, this builds an index over prose: a generated
  `tsvector` column and a GIN index over `package_readmes`. The cost is storage
  plus write amplification on every capture. You should choose it.
- **`text_config`** is the Postgres text search configuration the index is built
  with. `english` is the default because it is measurably better at the question
  this feature exists to answer: a reader who types `retry` finds a README that
  says `retrying`, and one who types `cache` finds `caching`. `simple` finds
  neither. Stemming does mangle identifiers — `axios` is stored as `axio` — and
  it does so *symmetrically*, because the query is stemmed too, so it still
  matches.
- **Changing it rebuilds the column.** `to_tsvector` in a generated column must
  be immutable, so the configuration is a literal: changing it drops and re-adds
  the column, rewriting every row's index. The server does this on startup and
  says so in the log. Take the decision at install rather than tuning it later.
- **`readmes = true` needs Postgres**, and the server refuses to start without
  it. Failing at startup beats a search that quietly matches nothing.

With it on, the listing endpoint accepts `?q=…&in=name|readme|both`:

- `in` defaults to `name`, which is today's behaviour byte for byte;
- **a name match always outranks a prose match.** A package literally called
  `retry` comes before one that mentions retrying, however densely. That is what
  a reader means when they type a name, and it is not a tuning parameter;
- every result says `matched_in` — `name`, `readme` or `both` — because a row
  whose name has nothing to do with the query and whose README mentions it in
  passing is a *correct* result and an inexplicable one without the label;
- the `snippet` is **plain text** and is rendered as text. It never reaches the
  markup path the README panel uses.

With it off, `in=readme` is accepted and answers exactly as `in=name` does, and
the response says `readme_search_enabled: false` so a client can tell "no package
here says that" from "this instance does not search prose".

**Only stored READMEs are searchable**, which is to say only versions this
instance holds or hosts. A README derived on the fly for an upstream-only version
has no row, and writing one is what the discovery read refuses to do.

---

### Auth providers {#auth}

Auth providers are evaluated in declaration order. The first provider that recognises a credential wins. Requests with no matching credential are treated as `anonymous`.

#### Static tokens

```toml
[[auth]]
type = "token"

[[auth.tokens]]
value   = "ci-pipeline-token"
role    = "user"
user_id = "ci"
```

#### OIDC (Authentik, Keycloak, Dex, …)

```toml
[[auth]]
type          = "oidc"
issuer_url    = "https://sso.example.com/application/o/batlehub/"
client_id     = "batlehub"
client_secret = "${OIDC_CLIENT_SECRET}"   # inject from env — never commit secrets
redirect_uri  = "https://batlehub.example.com/api/v1/auth/oidc/callback"
scopes        = ["openid", "profile", "email", "groups"]

user_id_claim = "preferred_username"
role_claim    = "groups"

[auth.role_mappings]
"authentik Admins" = "admin"
"proxy-users"      = "user"
```

#### Kubernetes service accounts

```toml
[[auth]]
type = "kubernetes"
# api_server, ca_cert_path, token_path all default to in-cluster values

[auth.role_mappings]
"system:serviceaccount:prod:ci-deployer" = "admin"
"system:serviceaccounts:staging"         = "user"
```

#### User-generated API tokens

Authenticated users (OIDC sessions) can generate short-lived tokens via the Web UI or API:

```sh
curl -X POST \
  -H "Authorization: Bearer <oidc-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-token", "expires_in_days": 30, "role": "user"}' \
  https://batlehub.example.com/api/v1/auth/tokens
```

The raw token value is returned **once** — save it immediately.

---

## Hot reload {#hot-reload}

BatleHub can reload its configuration at runtime — add or remove registries, update RBAC rules, or change policy settings — without restarting the process. In-flight requests finish with the old configuration before the new one takes effect.

### How it works

1. When `config.toml` changes on disk, the built-in file watcher validates the new config, runs connectivity probes against upstream URLs, and stores a **pending reload** in memory.
2. An administrator reviews the pending diff in the **Config Reload** admin page (`/admin/config-reload`) and clicks **Apply** — or discards it.
3. Alternatively, the `POST /api/v1/admin/config/reload` endpoint applies a reload immediately (load + validate + apply atomically), which is useful in CI/CD pipelines.

```sh
# Immediate reload (no confirmation step)
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/reload

# Check for a pending reload loaded by the file watcher
curl -s -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/pending

# Apply the pending reload
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/pending/apply

# Discard without applying
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/pending
```

Pending reloads expire after **10 minutes** if not applied or discarded.

### What can be hot-reloaded

| Component | Hot-reloadable |
|-----------|---------------|
| Registry list (add / remove / update) | ✅ |
| Per-registry RBAC (`anonymous`, `user`, `admin`, groups) | ✅ |
| Per-registry rules (age gate, deny latest) | ✅ |
| Per-registry versioning / signing / beta-channel | ✅ |
| Artifact size limit | ✅ |
| Server host / port | ❌ requires restart |
| Database URL | ❌ requires restart |
| Auth providers | ❌ requires restart |
| Storage backends | ❌ requires restart |

### Config warnings {#config-warnings}

Some config states are worth telling an operator about but not worth refusing to
start over — a registry name that cannot become a DNS label, a deprecated key
being shadowed, a permissive security default left in place. These are logged at
startup and on every reload, **and** served from an endpoint so they are actually
seen:

```sh
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/config/warnings
```

```json
{
  "warnings": [
    {
      "code": "proxy-trust.unconfigured",
      "path": "server.trusted_proxies",
      "message": "no trusted-proxy list is configured, so Forwarded / X-Forwarded-Host / …"
    }
  ]
}
```

`code` is a stable slug, safe to match on in automation; `path` points at the
offending config location verbatim, so you can search for it in the TOML.

`POST /api/v1/admin/config/validate` and `/config/from-content` return the same
shape inline under `warnings`, describing the **candidate** config — so you see
them before applying a pending reload rather than after. The Config Reload admin
page renders both, the active ones as a dismissible list.

### Audit trail

Every reload (applied or rejected) is written to the `config_changes` table and visible in the admin page change history:

```sh
curl -s -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/config/changes?per_page=20"
```

### Disabling hot reload

Set `BATLEHUB_DISABLE_HOT_RELOAD=1` in the server environment to disable the file watcher and all reload endpoints with a `503 Service Unavailable`. This is recommended when `config.toml` is mounted as a read-only Kubernetes ConfigMap, where the file will not change at runtime.

```yaml
# Kubernetes Deployment env
- name: BATLEHUB_DISABLE_HOT_RELOAD
  value: "1"
```

---

## Global banner {#global-banner}

Administrators can broadcast a short message to all website visitors — authenticated or not. Common uses: maintenance windows, reload-in-progress notices, and policy announcements.

The banner is automatically set to "Configuration reload in progress…" when a hot reload starts and cleared when it completes.

### Set the banner

From the **Config Reload** admin page, fill in the message and select a level (info / warning / error), then click **Set Banner**.

```sh
# Set via API
curl -s -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"message":"Scheduled maintenance in 30 min","level":"warning"}' \
  http://localhost:8080/api/v1/admin/banner

# Clear
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/banner
```

The frontend polls `GET /api/v1/banner` every 30 seconds (no authentication required) and displays the banner as a dismissible bar at the top of every page.

### High-availability banner propagation

The banner backend is selected from the same pool as the metadata cache:

| `[cache] type` | Banner storage |
|----------------|---------------|
| `"memory"` (default) | In-process — not shared across replicas |
| `"redis"` | Redis key `batlehub:system:banner` — shared across all replicas |
| `"postgres"` | `system_kv` table — shared across all replicas |

In an HA deployment, use `"redis"` or `"postgres"` so that all replicas show the same banner regardless of which instance the client reaches.
