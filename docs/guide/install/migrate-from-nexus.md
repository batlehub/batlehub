# Migrating from Nexus Repository

This page moves a Sonatype Nexus Repository 3 installation to BatleHub, one
repository at a time, without a day on which every build breaks at once.

BatleHub cannot yet import the *contents* of a Nexus instance in one command —
that is on the [roadmap](/guide/roadmap) ("Seed a registry from an incumbent").
The migration below does not need it: proxy repositories are recreated against
their original upstreams, hosted repositories are copied with each ecosystem's
own publish client, and Nexus keeps serving until the last client has moved.

## Concepts side by side

| Nexus | BatleHub | Notes |
|-------|----------|-------|
| Repository `/repository/<name>/` | Registry `/proxy/<name>/…` | One `[[registries]]` block per repository. The path after `<name>` depends on the type — see each [registry page](/registries/). |
| *proxy* repository | `mode = "proxy"` | Read-through cache; the default mode. |
| *hosted* repository | `mode = "local"` | BatleHub is the source of truth; clients publish to it. |
| *group* repository | `mode = "hybrid"`, or several `upstreams` | See [Groups](#groups). |
| Blob store | `[storage]` backend | Filesystem or S3; a registry picks a named backend with `storage = "…"`. See [Configuration § `[storage]`](/guide/configuration#_3-4-storage). |
| Cleanup policy | `[registries.cache]` eviction | `artifact_ttl_secs`, `idle_days`, `max_size_bytes`, `keep_latest_n` — see [Caching](/guide/caching). |
| Remote authentication | `[registries.upstream_auth]` | `basic`, `bearer` or `header` — see [Private upstreams](/guide/private-upstreams). |
| Users, roles, LDAP/SAML realms | `[[auth]]` providers (token, OIDC, Kubernetes, Actions OIDC) | Groups come from the identity provider. |
| Privileges, content selectors | Grants on the instance, registry, namespace, package or version | See [Access control](/guide/access-control). |
| User token | Personal access token | See [Getting a token](/use/#getting-a-token). |

## Formats

| Nexus format | BatleHub `type` | Hosted equivalent |
|--------------|-----------------|:-----------------:|
| `maven2` | [`maven`](/registries/maven) | ✅ |
| `npm` | [`npm`](/registries/npm) | ✅ |
| `pypi` | [`pypi`](/registries/pypi) | ✅ |
| `nuget` | [`nuget`](/registries/nuget) (v3 protocol) | ✅ |
| `rubygems` | [`rubygems`](/registries/rubygems) | ✅ |
| `go` | [`goproxy`](/registries/goproxy) | ✅ |
| `apt` | [`deb`](/registries/deb) | ✅ |
| `yum` | [`rpm`](/registries/rpm) | ✅ |
| `conda` | [`conda`](/registries/conda) | ✅ |
| `cargo` | [`cargo`](/registries/cargo) | ✅ |
| `composer` | [`composer`](/registries/composer) | ✅ |
| `raw` | [`generic`](/registries/generic) | ❌ proxy only |
| `docker`, `helm`, `r`, `conan`, `cocoapods`, `p2`, `bower`, `gitlfs` | — | — |

The last row is **not supported yet**. Some of those formats are on the
[roadmap](/guide/roadmap), and the list of supported types grows with each
release:

- `helm` — a chart repository (`index.yaml` + `.tgz`) is planned in
  [RFC 0029](/rfc/0029-helm-charts) (Draft). OCI-based charts stay out of scope.
- `docker` — not planned: the roadmap points to a dedicated OCI registry such
  as [Harbor](https://goharbor.io).
- `r`, `conan`, `cocoapods`, `p2`, `bower`, `gitlfs` — not on the roadmap
  today; open an issue if you need one.

Until then, keep those repositories in Nexus, or move them to a dedicated tool,
before planning to switch Nexus off.

A `raw` *hosted* repository has no home either: `generic` only mirrors an
upstream file tree.

## 1. Take the inventory

The Nexus REST API lists every repository with its format, type and remote URL:

```bash
curl -su admin "https://nexus.example.com/service/rest/v1/repositories" \
  | jq -r '.[] | [.name, .format, .type, (.attributes.proxy.remoteUrl // "")] | @tsv'
```

Sort the result into three piles — proxy, hosted, group — and cross out the
formats of the last row of the table above. Each pile has its own step below.

## 2. Proxy repositories

Point the BatleHub registry at the **original** upstream, not at Nexus:

```toml
[[registries]]
type      = "maven"
name      = "maven-central"
upstreams = ["https://repo1.maven.org/maven2"]
```

The cache starts empty and fills on the first request for each artifact. To
avoid a slow first build, warm the packages you know you need:

```toml
[registries.cache]
warm_packages = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n = 3
```

When the upstream needs credentials Nexus used to hold (a vendor's private
feed), move them to `[registries.upstream_auth]` — see
[Private upstreams](/guide/private-upstreams).

## 3. Hosted repositories

A hosted repository holds the only copy of what your teams published. Its
history is copied into a `local` registry **before** anyone publishes to
BatleHub.

::: warning Do not bridge with `hybrid` and Nexus as upstream
A `hybrid` registry answers a package's version listing from its local
versions alone as soon as it has one, and does not consult the upstream for
that name. Publish `foo@1.0.1` to a hybrid registry whose upstream is the Nexus
repository holding `foo@1.0.0`, and npm, Maven, pip, NuGet, cargo and Go
clients stop seeing `1.0.0`. This is not how a Nexus group behaves: a group
merges the versions of a package across its members.
:::

### Create the local registry

```toml
[[registries]]
type = "npm"
name = "npm-internal"
mode = "local"

[registries.grants]
"role:user"        = ["releases:read", "releases:list"]
"group:*:engineer" = ["releases:publish"]
```

### Copy the history

List the hosted repository's assets with the Nexus components API (it pages
with `continuationToken`):

```bash
repo=npm-hosted token=""
while :; do
  page=$(curl -su batlehub-reader \
    "https://nexus.example.com/service/rest/v1/components?repository=$repo${token:+&continuationToken=$token}")
  jq -r '.items[].assets[].downloadUrl' <<<"$page"
  token=$(jq -r '.continuationToken // empty' <<<"$page")
  [ -z "$token" ] && break
done > assets.txt
```

Download them (`wget --user batlehub-reader --ask-password -i assets.txt`), then
publish each file with the ecosystem's own client, as you would any new version:

| Type | Republish with |
|------|----------------|
| `maven` | `mvn deploy:deploy-file -Dfile=<jar> -DpomFile=<pom> -Durl=https://batlehub.example.com/proxy/<registry>/maven2/ -DrepositoryId=<server-id>` |
| `npm` | `npm publish <package>-<version>.tgz` |
| `pypi` | `twine upload --repository-url https://batlehub.example.com/proxy/<registry>/legacy/ <files>` |
| `nuget` | `dotnet nuget push <file>.nupkg --source <source>` |
| `rubygems` | `gem push <file>.gem --host https://batlehub.example.com/proxy/<registry>` |

Each registry page has the exact client configuration under *Publishing*.
Every republish is an ordinary publish, so quotas, ownership and scans all
apply.

::: warning Turn `monotonic` off while copying
A history is published oldest-first; a namespace with
`versioning.monotonic = true` refuses that. Turn it on again afterwards — see
[Immutability and ordering](/guide/access-control#versioning).
:::

### Switch

1. Make the Nexus repository read-only, so nothing is published there any more.
2. Copy the versions published since the first copy.
3. Move the publishers (CI jobs, `distributionManagement`, `publishConfig`) and
   the consumers to BatleHub — see [Move the clients](#_4-move-the-clients).

## Groups {#groups}

A Nexus group has no one-to-one equivalent. Pick the shape that matches what it
contained:

- **One hosted and one proxy repository** (the classic `maven-public`): a single
  `hybrid` registry, with the hosted history copied into it as in step 3 and the
  public upstream in `upstreams`. A name published locally is served from its
  local versions only; any other name falls through to the upstream.
- **Several proxy repositories**: a single `proxy` registry with several
  `upstreams`. They are tried in order and the next one is used on a `404`.
- **A credentialed upstream and a public one**: `upstream_auth` is sent to
  *every* upstream of a registry, so split them — see
  [Mixing a private upstream with a public fallback](/guide/private-upstreams#mixing-a-private-upstream-with-a-public-fallback).

## 4. Move the clients

Replace the Nexus URL and credential in each client's configuration. The URL
shape per type is on the [registry pages](/registries/); a few examples:

| Type | Nexus | BatleHub |
|------|-------|----------|
| `maven` | `https://nexus.example.com/repository/maven-public/` | `https://batlehub.example.com/proxy/<registry>/maven2/` |
| `npm` | `https://nexus.example.com/repository/npm-group/` | `https://batlehub.example.com/proxy/<registry>/` |
| `pypi` | `https://nexus.example.com/repository/pypi-group/simple/` | `https://batlehub.example.com/proxy/<registry>/simple/` |
| `nuget` | `https://nexus.example.com/repository/nuget-group/index.json` | `https://batlehub.example.com/proxy/<registry>/nuget/v3/index.json` |

The credential becomes a BatleHub personal access token. Clients that send
username and password (Maven, pip, NuGet) keep doing so, with the token as the
password — the username each one expects is on its registry page.

`batlehub-cli registry suggest --client-env` reads a project's manifests and
prints the matching registries and client settings — see the
[CLI reference](/use/cli#registry-suggest).

## 5. Switch Nexus off

Nexus can go once:

- no BatleHub registry lists a Nexus URL in `upstreams`;
- the Nexus request log shows no client other than BatleHub;
- every hosted repository has been copied, or deliberately left behind.

Keep a read-only Nexus, or a backup of its blob stores, for as long as your
retention policy asks.
