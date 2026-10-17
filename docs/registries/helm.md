# Helm

Proxy and cache a classic Helm chart repository — the `index.yaml` kind that `helm repo add` takes — as a *typed* registry, so a chart version can be **blocked** rather than merely cached. `index.yaml` is the filtered listing and the enforcement point; the `{name}-{version}.tgz` archive, and the `.prov` beside it, are the artifact. `local` and `hybrid` mode accept ChartMuseum's upload API, the call `helm cm-push` makes.

What is served, and how:

- **`index.yaml`**, with blocked versions removed and **every chart URL rewritten to this instance** — relative ones, absolute ones and ones on another host alike — so `helm` downloads nothing from anywhere else.
- **The archive and its `.prov`**, byte-exact, fetched from the URL the upstream index names. `helm pull --verify` checks the signature against bytes this instance never altered.
- **ChartMuseum's `api/charts` listings**, in `local` and `hybrid` mode.

OCI chart registries (`helm pull oci://…`) are not a chart repository and are not served; see `oci_entries` below for the index entries that point at one.

## At a glance

| | |
|---|---|
| **Config type** | `helm` |
| **Default upstream** | none — a registry is one chart repository, and you name it |
| **Modes** | proxy · local · hybrid |
| **Addressing** | one package per chart, one version per index entry, the `.tgz` and its `.prov` as files of it |
| **Private publish** | ✅ ChartMuseum's `POST api/charts` (`curl`, `helm cm-push`) |
| **Client switch** | `helm repo add` |

## Proxy setup

```sh
helm repo add batlehub https://batlehub.example.com/proxy/<registry>/helm \
  --username <user> --password <token>
helm repo update
```

```sh
helm install my-release batlehub/cert-manager --version v1.21.2
```

Your administrator's registry block:

```toml
[[registries]]
name      = "<registry>"
type      = "helm"
mode      = "proxy"
upstreams = ["https://charts.jetstack.io"]   # the repository directory, not …/index.yaml
# oci_entries = "drop"                       # the default — see below

[registries.rbac]
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list", "releases:publish"]
admin     = ["*"]
```

`batlehub-cli registry suggest` reads a chart's `Chart.lock` (or `Chart.yaml` when there is no lock) and writes one such block per `https://` repository in its `dependencies`, with each exact version in `warm_packages`; an `oci://`, `@alias` or `file://` dependency is named in a comment instead.

A helm registry has **exactly one** upstream: a second chart repository is a second registry block, and `helm repo add`s a second name. The upstream is the directory `helm repo add` takes; the loader refuses one ending in `/index.yaml`, and refuses `hybrid` with no upstream at all.

## Authentication

`helm` sends `--username`/`--password` as HTTP Basic, on the index **and** on every chart URL of the same host. Since every URL in the served index is on this host, a registry closed to anonymous reads works without `--pass-credentials`. The password is a BatleHub token; the user name is not checked.

## Blocking

A blocked version is absent from `index.yaml`. The index describes the whole repository, so a new block reaches it within the blocked-set snapshot's 30-second lifetime. But `helm` reads **its own cached copy** of the index until the next `helm repo update` — until then it still offers the version, and the archive download is what refuses it.

After an update, an exact pin on a blocked version fails with helm's own *"chart … version … not found"*, and a range resolves to the newest allowed version.

## `oci_entries`

A chart repository's index may name a version whose `urls[0]` is `oci://…` — the chart lives in an OCI registry, not beside the index. This instance cannot serve those bytes, so it decides what the entry becomes:

| `oci_entries` | Effect |
|---|---|
| `drop` (default) | The entry is omitted. That chart version is not installable through this registry — an entry the proxy cannot serve is one its policy cannot reach. |
| `relay` | The entry is served as upstream wrote it, and `helm` pulls it straight from the OCI registry, **past this instance**: uncached, unblockable, outside the audit trail. Warned at every reload, and on the console's registry card. |

The field is rejected on any other registry type.

## Publishing

`local` and `hybrid` registries accept a packaged chart:

```sh
helm package ./mychart
curl --data-binary @mychart-0.1.0.tgz \
  -H "Authorization: Bearer <token>" \
  https://batlehub.example.com/proxy/<registry>/helm/api/charts
```

`helm cm-push ./mychart batlehub` makes the same call. Both of ChartMuseum's body shapes are accepted: the archive as the whole body, or a multipart form with a `chart` field and an optional `prov` field carrying its provenance file.

A duplicate version answers `409`. `?force` replaces it and needs `releases:overwrite`. `DELETE api/charts/{name}/{version}` removes a version.

The index a local registry serves is composed from what was published — each entry built from the archive's own `Chart.yaml` and its SHA-256 digest — so it cannot name a chart that is not here.

### Endpoint reference

<!-- BEGIN endpoints: proxy/helm -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/helm/api/charts` | Every chart published here and its versions — ChartMuseum's listing, paged by chart with `offset`/`limit`. |
| `POST` | `/proxy/{registry}/helm/api/charts` | Publish a packaged chart — ChartMuseum's upload, the call `helm cm-push` makes. |
| `GET` | `/proxy/{registry}/helm/api/charts/{name}` | One chart's versions as ChartMuseum lists them. |
| `GET` | `/proxy/{registry}/helm/api/charts/{name}/{version}` | One chart version as ChartMuseum describes it. |
| `DELETE` | `/proxy/{registry}/helm/api/charts/{name}/{version}` | Delete a published chart version (`releases:delete`); it leaves the next index. |
| `GET` | `/proxy/{registry}/helm/charts/{file}` | A chart archive, or with `.prov` appended its provenance file — byte-exact, fetched from the URL the upstream index names. |
| `GET` | `/proxy/{registry}/helm/index.yaml` | The repository index `helm repo add` and `helm repo update` fetch — blocked versions removed, every chart URL on this host. |
<!-- END endpoints -->

## Hybrid mode: local wins per version

On a `hybrid` registry the served index is upstream's with the local entries laid over it. A chart version published here replaces upstream's entry for the **same `{name, version}`**; upstream's other versions of that chart stay listed. That is narrower than the whole-name rule other kinds apply, because a chart index is one document and its entries are independent.

## Air-gapped estates

An air-gapped registry composes `index.yaml` from the archives the bundle actually carries, each entry rebuilt from the chart's own `Chart.yaml`, and serves the archive and any `.prov` from storage. A version the bundle does not have is not in the index (see [RFC 0008-bis](../rfc/0008-bis-listings-across-the-gap)).

The release age gate dates a version by its index entry's `created`, which `helm repo index` writes on every entry.

## Not served

- OCI chart registries (`helm push`/`pull oci://…`), which are container registries
- ChartMuseum's multi-tenant paths and its `/health` and `/info` endpoints
- the Artifact Hub search API; `registry search` answers from what this instance holds, as it does for every kind

## See also

- [RFC 0029](../rfc/0029-helm-charts) — URL rewriting, the `oci_entries` decision and index regeneration
- [RFC 0006](../rfc/0006-blocked-versions-hidden-everywhere) — blocked versions hidden from listings
