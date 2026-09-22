# Devfile registry (Che, odo)

Proxy and cache a devfile registry — `registry.devfile.io` by default — as a *typed* registry, so a stack version can be **blocked** rather than merely cached. A devfile registry is the catalogue a cloud development environment reads to offer "start a Node.js workspace": Eclipse Che's *Get Started* page reads it for its tiles, and `registry-library` — the library `odo` and the IDE plugins embed — reads it to pull a stack.

What is served, and how:

- **The index documents** — `index`, `v2index`, each with `/sample`, `/stack` and `/all` — with blocked versions removed. They describe the whole registry, so a new block reaches them within the blocked-set snapshot's 30-second lifetime, as conda's `repodata.json` does.
- **Each stack version's devfile**, its **OCI manifest** and the **layers** the manifest names (`devfile.yaml`, `archive.tar`, …), byte-exact, each verified against its digest before a byte is stored or served.
- **Starter projects**, the zips `registry-library download` fetches.

The container images a devfile names are **not** proxied: the cluster pulls those, and this is not a container registry.

## At a glance

| | |
|---|---|
| **Config type** | `devfile` |
| **Default upstream** | `registry.devfile.io` |
| **Modes** | proxy-only |
| **Addressing** | one package per stack, one version per stack version, one artifact per file of it |
| **Private publish** | ❌ proxy-only — a devfile registry is built offline into an image |
| **Client switch** | Che: `externalDevfileRegistries`; `registry-library`/`odo`: the registry URL |

## Give it a host of its own

`registry-library` resolves the index relative to the URL it is given, and then asks for the OCI manifest and layers — and for starter projects — at the **root of that URL's host**, dropping any path prefix. Pointed at `https://batlehub.example.com/proxy/<registry>/`, it reads the index and then 404s every pull at `https://batlehub.example.com/v2/…`.

So bind the registry to a host ([host routing](../guide/host-routing)), and give clients that host. Che keeps a path prefix and works either way; the server logs a warning, and the console's registry card shows it, for a devfile registry with no host.

## Proxy setup

Your administrator's registry block:

```toml
[[registries]]
name  = "<registry>"
type  = "devfile"
mode  = "proxy"                                  # the only mode: no publish protocol
hosts = ["devfile.batlehub.example.com"]         # registry-library needs the host root
# upstreams = ["https://registry.devfile.io"]    # the default; one entry only

[registries.rbac]
# Neither client sends a credential — see Authentication below.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list"]
admin     = ["*"]
```

### Eclipse Che

The dashboard reads `index/all` and follows each tile to `devfiles/{stack}/{version}`:

```yaml
# CheCluster — the dashboard reads index/all and follows each tile to devfiles/…
spec:
  components:
    devfileRegistry:
      externalDevfileRegistries:
        - url: https://batlehub.example.com/proxy/<registry>/
```

If the `CheCluster` sets `devEnvironments.allowedSources.urls`, add the registry's URL there too. Use a hostname, not an IP address: Che's resolver refuses a URL whose host is a private IP literal, with its own `403`.

### registry-library and odo

```sh
# The trailing slash matters under a path prefix: the index is resolved
# relative to the URL. The OCI requests always go to the host root.
registry-library pull https://batlehub.example.com/proxy/<registry>/ nodejs:2.2.1 --new-index-schema
odo preference add registry batlehub https://batlehub.example.com/proxy/<registry>/
```

With the registry on its own host, the URL is that host's root — `https://devfile.batlehub.example.com/`.

## Authentication

**Neither client carries a credential.** Che's dashboard fetches the index through its own backend, with no `Authorization`, and `registry-library` builds its OCI requests from the URL's host alone, so userinfo in the URL reaches the index and nothing after it. The registry therefore has to grant anonymous `releases:read` and `releases:list`, and the server warns when it does not. A catalogue that must stay private is one kept behind a network boundary, not a token.

## What blocking does to a client

- **`registry-library pull stack:version`** of a blocked version stops at the index, with its own *"the requested version … does not exist in the registry"*. No OCI request is made. A client that kept the version's tag or a layer's digest and asks for it directly is refused too: a digest is served only when a version the filtered index still lists names it.
- **Blocking a stack's default version** moves `default` in the v2 index to the highest version left, so an unpinned `pull` still works and gets that one. The legacy index names one version per stack, the default, so there the stack disappears.
- **In Che**, the stack's tile disappears once its default is blocked. The dashboard caches the index for **an hour per browser session**, so a tile for a version blocked after a user's last fetch still shows until then; opening it is refused.

::: warning `registry-library` exits 0 on every failure
Every error it hits is printed and the process exits 0, and a layer that fails its digest check is left on disk. This registry verifies every layer before serving it, which is the check that has consequences — but a script around the client should read its output, not its exit code.
:::

## The age gate

Upstream's `lastModified` is the time the whole registry was last rebuilt — the same instant on every version — so this registry dates no stack version. A `release_age_gate` rule on it must set `deny_missing_timestamp`: `true` refuses every download, `false` makes the gate inert.

## Air-gapped

A disconnected instance ([air gap](../operations/air-gap)) serves the stacks a bundle carried. Carry each stack version as its **manifest** (`/v2/devfile-catalog/{stack}/manifests/{version}`) and its **devfile** (`/devfiles/{stack}/{version}`). The import reads the layers and their digests off the manifest, and the displayed metadata and starter projects off the devfile. With `synthesise_listings` on, both indexes are composed from what is held:

- each held version is listed, and the highest is the default;
- samples are not listed — their source is a git remote the air gap does not have;
- `icon` is present and empty. Upstream's icons are URLs a disconnected browser cannot reach, and Che drops an index entry that has no `icon` at all;
- the `arch` and `deprecated=false` filters apply; the schema-version filters do not.

## See also

- [RFC 0035](../rfc/0035-devfile-registry) — the protocol as `registry.devfile.io` serves it, and why the tag is the chokepoint
- [Generic mirror](./generic) — caches a devfile registry without policy, and blocks nothing
