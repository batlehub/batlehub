# Ansible Galaxy

Proxy and cache `galaxy.ansible.com` as a *typed* registry, so a collection version can be **blocked** rather than merely cached. The per-collection versions list — `v3/collections/{namespace}/{name}/versions/` — is the filtered listing and the enforcement point; the `{namespace}-{name}-{version}.tar.gz` tarball is the artifact. `local` and `hybrid` mode accept `ansible-galaxy collection publish`.

Roles — the older v1 API — are served read-only, and how much of that surface exists is the operator's choice (`roles`, below).

## At a glance

| | |
|---|---|
| **Config type** | `galaxy` |
| **Default upstream** | `https://galaxy.ansible.com/api/` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | `{namespace}.{name}` — the spelling `requirements.yml` uses — with one tarball per version |
| **Private publish** | ✅ `ansible-galaxy collection publish` |
| **Client switch** | `server_list` in `ansible.cfg` |

## Proxy setup

`ansible.cfg` has one switch. `server_list` names the servers `ansible-galaxy` uses, in order; listing only this one keeps every resolve on the proxy:

```ini
[galaxy]
server_list = batlehub

[galaxy_server.batlehub]
url   = https://batlehub.example.com/proxy/<registry>/galaxy/api/
token = <your-token>
```

```sh
ansible-galaxy collection install community.general
ansible-galaxy collection install -r requirements.yml
```

Your administrator's registry block:

```toml
[[registries]]
name      = "<registry>"
type      = "galaxy"
mode      = "proxy"                              # proxy · local · hybrid
upstreams = ["https://galaxy.ansible.com/api/"]  # the default
roles     = "proxy"                              # proxy · index · off

[registries.rbac]
# The versions list is a listing; the version document and the tarball are reads.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list", "releases:publish"]
admin     = ["*"]
```

`upstreams` is the **API root** — the URL that answers `available_versions`. Writing the host without `/api/` works too: the adapter probes for it exactly as `ansible-galaxy`'s own `g_connect` does, and retries with `/api/` appended.

## Authentication

`token` in a `[galaxy_server.*]` block is sent as **`Authorization: Token <token>`** on **every** call, reads included — so a `galaxy` registry can be closed to anonymous reads without breaking the client. That is unusual: most package managers send nothing on a read.

`Token` is Django REST Framework's scheme, not HTTP's, and it is what `GalaxyToken.token_type` names; BatleHub normalises it before its auth providers see it. (`Bearer` is Automation Hub's `KeycloakToken`, which is [not served](#not-served).)

A `username`/`password` pair sends HTTP Basic instead, which this server also accepts.

::: warning `--api-key` is not a substitute for `token =`
When the server comes from `server_list`, `ansible-galaxy … --api-key <token>` attaches **no credential at all** — measured against ansible-core 2.19.3, the request carried no `Authorization` header. Put the credential in the `[galaxy_server.*]` block.
:::

Reads with no `token` line are anonymous, which is the ordinary `anonymous = [...]` decision above.

::: tip Not Automation Hub's token exchange
`auth_url`, `client_id` and `client_secret` in a `[galaxy_server.*]` section drive an OAuth2 refresh against a Keycloak. This server issues its own bearer tokens; use `token`.
:::

## What blocking does to a client

A blocked version is **absent from the versions list**, and `ansible-galaxy`'s resolver picks only from that list — for every direct requirement *and* every dependency, including a requirement pinned to one exact version. So:

- a **range** resolves to the newest version you do allow, and the install succeeds;
- an **exact pin** on a blocked version stops with ansible's own *"Failed to resolve the requested dependencies map. Could not satisfy the following requirements:"*, before any metadata or artifact request;
- a client holding a **stale listing** from before the block is refused at the version document (`404`) and again at the tarball (`403`).

Blocking the newest version also moves the collection document's `highest_version` onto the newest surviving one, and bumps its `updated_at`. That second edit is what makes the first take effect promptly: `ansible-galaxy` caches a versions list for 24 hours and re-reads the collection document on every resolve to decide whether that copy is still good. Without the bump, a warm client would keep offering the blocked version to its own resolver for up to a day and the install would fail rather than quietly resolve.

## Four things worth knowing

**The tarball is byte-exact.** `ansible-galaxy` hashes the body as it downloads and compares it with `artifact.sha256` from the version document, failing on *"Mismatch artifact hash with downloaded file"*. This server relays both unchanged, and never rewrites a collection archive. The GPG signatures upstream publishes over a collection's `MANIFEST.json` are relayed as served — nothing here mints one, and nothing here can.

**Every listing is one page.** `links.next` is always `null`, and this server walks upstream's pages itself. That is not a simplification: `ansible-galaxy` resolves a continuation link with `urljoin(api_server, next)`, and upstream's links are absolute *paths*, which replace the whole path — a link relayed as received would send the next request to the root of the host rather than back to `/proxy/<registry>/galaxy/…`. The roles half is worse: its client strips the path deliberately. Upstream caps a page at 100 entries whatever you ask for, so the largest collection in existence (`community.general`, 241 versions) is three upstream requests per cache fill.

**A token is sent on reads as well as writes** — see *Authentication* above.

**Roles have a limit this server cannot remove.** `ansible-galaxy role install` builds `https://github.com/{user}/{repo}/archive/{version}.tar.gz` itself, and only prefers a `download_url` when the role's versions listing carries one. galaxy.ansible.com carries one for every published version, so `roles = "proxy"` routes those bytes through this instance. Two cases it cannot reach, whatever you configure:

- a role with **no published versions** installs from its default branch, straight from GitHub;
- a `requirements.yml` entry with an explicit `src:` URL was never a registry request at all.

## The `roles` setting

```sh
ansible-galaxy role install geerlingguy.docker
```

How much of that command this instance serves is the operator's choice:

| `roles` | Behaviour |
|---|---|
| `proxy` (default) | The v1 read endpoints are served, each version's `download_url` is rewritten, and this server fetches the archive from the host upstream named — through the SSRF guard, and only from `github.com` or the registry's own upstream. |
| `index` | The same endpoints with `download_url` relayed. Role *metadata* is proxied; role *bytes* are not, so the server makes no outbound connection to GitHub. |
| `off` | The v1 endpoints answer `404`, and `v1` is absent from the discovery document — so `ansible-galaxy role install` fails on its own *"requires API versions 'v1'"* rather than on a `404` that looks like a proxy fault. |

`roles` is rejected on any other registry type.

## Publishing

`local` and `hybrid` mode accept the real publish protocol:

```sh
ansible-galaxy collection build
ansible-galaxy collection publish ./acme-util-1.0.0.tar.gz --server batlehub
```

The credential is the `token =` line in `ansible.cfg`, not `--api-key` — see the warning under *Authentication*.

The publish is **synchronous**. `ansible-galaxy` posts the tarball and then polls the import task; here the work is finished before that POST answers, so the task is already `completed` on the first poll and an install straight afterwards cannot race it.

The POST answers with a bare task **id**, and the client builds the poll URL itself — `…/v3/imports/collections/{id}/`. It does not follow a URL the server supplies.

Three checks run before anything is stored, each a `400`:

- the `sha256` form field must equal the digest of the uploaded bytes — a mismatched pair would fail every install, because the client checks the same digest (the file part itself arrives base64-encoded, which `ansible-galaxy` marks with `Content-Transfer-Encoding`; a plain binary part is accepted too);
- the tarball must contain a `MANIFEST.json` at its root;
- the file name must agree with what that manifest says, so a version cannot be stored under one coordinate and served under another.

A duplicate version answers `409`, which `ansible-galaxy` renders as *"(HTTP Code: 409, Message: … Code: …)"*.

`FILES.json` travels inside the artifact and is never rebuilt: it is what a client verifies the extracted tree against, so a re-derived copy that disagreed by one byte would fail an install this server had accepted.

### Endpoint reference

<!-- BEGIN endpoints: proxy/galaxy -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/galaxy/api/` | The API versions this registry serves — read by `g_connect` before any action, and `v1` is absent when `roles = "off"`. |
| `GET` | `/proxy/{registry}/galaxy/api/v1/roles/` | Look a role up by owner and name — what `lookup_role_by_name` reads for the numeric id every later v1 request uses. |
| `GET` | `/proxy/{registry}/galaxy/api/v1/roles/{id}/download/{filename}` | A role archive, fetched server-side from the host its listing named — served only under `roles = "proxy"`. |
| `GET` | `/proxy/{registry}/galaxy/api/v1/roles/{id}/versions/` | Every allowed version of a role, as one page, each `download_url` pointed here under `roles = "proxy"`. |
| `POST` | `/proxy/{registry}/galaxy/api/v3/artifacts/collections/` | Publish a collection — synchronous, so the import task it answers with is already finished. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/artifacts/collections/{filename}` | The collection tarball, byte-exact — the client hashes it against `artifact.sha256` from the version document. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/` | The collection document, with `highest_version` moved off a blocked version and `updated_at` bumped past the block. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/` | Every allowed version of a collection, as one page — the chokepoint the resolver picks from. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/{version}/` | One version's document — `download_url` pointed here, `artifact.sha256` relayed untouched, `404` when the version is blocked. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/imports/collections/{task}/` | The import task a publish returned — always `completed`, because the work is done before the task exists. |
<!-- END endpoints -->

## Hybrid mode: local wins whole, not per version

On a `hybrid` registry a collection is served **either** from here **or** from
upstream, never merged: if this instance has published any version of
`acme.util`, that is the collection a client sees, and upstream's versions of
the same name are not offered. Upstream is consulted only for a collection this
instance has never published.

That is how every registry type in BatleHub behaves, not a Galaxy rule — and it
is the behaviour that keeps a private collection from being silently replaced by
a public one of the same name. The consequence worth knowing: publishing one
patched version of a public collection here takes over the whole name.

## Air-gapped estates

An air-gapped registry composes all three documents an install reads — the versions list, the collection document and the per-version document — from the versions the bundle actually carries, and serves the tarball from storage. A version the bundle does not have is absent from the listing, and a request for its document answers `503` rather than `404`: it exists, it is simply not here (see [RFC 0008-bis](../rfc/0008-bis-listings-across-the-gap)).

An imported version carries no publication date, so a `release_age_gate` rule on a `galaxy` registry must set `deny_missing_timestamp` explicitly — the config loader refuses the rule without it.

## Not served

- Automation Hub's OAuth2 token exchange (`auth_url`/`client_id`/`client_secret`)
- the role **write** APIs — `ansible-galaxy role import`, `delete`, `setup` — which are a forge integration rather than a registry operation
- Galaxy's search, namespace-listing and deprecation APIs; `registry search` answers from what this instance holds, as it does for every kind
- `docs_blob` and the rendered content views
- Ansible Execution Environments, which are OCI images

## See also

- [RFC 0031](../rfc/0031-ansible-galaxy) — why the versions list is the chokepoint, and why every listing is one page
- [RFC 0006](../rfc/0006-blocked-versions-hidden-everywhere) — blocked versions hidden from listings
