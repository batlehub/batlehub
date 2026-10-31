# Devfile registry (Che, odo)

Proxy and cache a devfile registry — `registry.devfile.io` by default — as a *typed* registry, so a stack version can be **blocked** rather than merely cached. A devfile registry is the catalogue a cloud development environment reads to offer "start a Node.js workspace": Eclipse Che's *Get Started* page reads it for its tiles, and `registry-library` — the library `odo` and the IDE plugins embed — reads it to pull a stack.

What is served, and how:

- **The index documents** — `index`, `v2index`, each with `/sample`, `/stack` and `/all` — with blocked versions removed. They describe the whole registry, so a new block reaches them within the blocked-set snapshot's 30-second lifetime, as conda's `repodata.json` does.
- **Each stack version's devfile**, its **OCI manifest** and the **layers** the manifest names (`devfile.yaml`, `archive.tar`, …), byte-exact, each verified against its digest before a byte is stored or served.
- **Starter projects**, the zips `registry-library download` fetches.
- **Stacks of your own**, in `local` or `hybrid` mode: `batlehub publish` uploads a stack directory, and the server derives the index entry and the OCI objects a pull reads from it ([Publishing](#publishing)).

The container images a devfile names are **not** proxied: the cluster pulls those, and this is not a container registry.

## At a glance

| | |
|---|---|
| **Config type** | `devfile` |
| **Default upstream** | `registry.devfile.io` |
| **Modes** | proxy, local, hybrid |
| **Addressing** | one package per stack, one version per stack version, one artifact per file of it |
| **Private publish** | ✅ `batlehub publish <stack dir>` — a bundle this server defines, since upstream has no publish protocol |
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
mode  = "proxy"                                  # or "local" / "hybrid" to publish — see Publishing
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

## Publishing

A `local` or `hybrid` registry hosts stacks of its own. Upstream has no upload API — `registry-support` builds a registry offline into an image — so the upload is a bundle this server defines, and `batlehub publish` packs it from a stack directory laid out as `registry-support` lays one out:

```sh
# my-stack/devfile.yaml            metadata.name and metadata.version name the stack
# my-stack/kubernetes/deploy.yaml  anything else goes into archive.tar
# my-stack/starter-projects/hello.zip   optional, one per starterProjects[*].name
batlehub publish ./my-stack -r <registry>
batlehub publish ./my-stack/devfile.yaml -r <registry>     # the same thing
```

`--name` and `--version` are refused: the server reads both from the devfile's `metadata`, and nowhere else. The packing is deterministic — sorted paths, zero timestamps and owners — so the same directory always gives the same digests, and a second publish of it is a `409` about the version rather than about a timestamp.

Without the CLI, the bundle is an uncompressed tar whose members are exactly `devfile.yaml`, an optional `archive.tar` (gzip-compressed, despite its name: `registry-library` reads it as a `.tar.gz`) and optional `starter-projects/{name}.zip`:

```sh
curl --data-binary @acme-rust-1.2.0.devfile.tar -H 'Content-Type: application/x-tar' \
  -H "Authorization: Bearer <token>" \
  https://batlehub.example.com/proxy/<registry>/api/stacks
curl -X DELETE -H "Authorization: Bearer <token>" \
  https://batlehub.example.com/proxy/<registry>/api/stacks/acme-rust/1.2.0
```

Any other member — another path, a directory, a link, a duplicate — is a `400` naming it. A published version is never replaced.

What a published version looks like to a client:

- its **index entry** is composed from the devfile's `metadata` at publish; `icon` is `metadata.icon` as written, so on a disconnected estate point it at a file this instance serves (a `github` registry's raw path, for instance);
- its **default** is the highest release version, never a prerelease while a release exists — publish a `3.0.0` as `3.0.0-rc.1` until Che should offer it;
- its **OCI manifest** is built once at publish and stored, so its digest — the one the publish answers with — never changes;
- a **starter project** is served only when the bundle carried its zip. Che does not use the route: a workspace clones the starter's git remote itself.

Keep published stacks `public`. Che reads the index anonymously, so a stack published as `internal` or `team` is missing from *Get Started*; the server warns about a registry whose default visibility would do that.

### In hybrid mode, a published stack owns its name

A stack published here replaces upstream's stack of the same name **whole**: upstream's entries for that name leave both indexes, and none of its versions, tags or digests is fetched. A per-version merge would let a public `9.9.9` become the default Che offers over yours.

Because that replaces the stack for every user of the instance, the **first** publish of a name upstream lists needs `devfile:stack:claim` beside `releases:publish`; administrators hold it, and `[registries.rbac]` or a `[[registries.namespaces]]` block can grant it to others. When upstream cannot be reached and its index is not cached, such a publish is refused with a `503` rather than let through. Later versions of a name already published here need nothing extra, and deleting the last version releases the name.

A name upstream starts listing *after* it was published here keeps the local stack; the server logs it once and sets `batlehub_devfile_shadowed_upstream_stacks{registry}`.

## Air-gapped

A disconnected instance ([air gap](../operations/air-gap)) serves the stacks a bundle carried. Carry each stack version as its **manifest** (`/v2/devfile-catalog/{stack}/manifests/{version}`) and its **devfile** (`/devfiles/{stack}/{version}`). The import reads the layers and their digests off the manifest, and the displayed metadata and starter projects off the devfile. With `synthesise_listings` on, both indexes are composed from what is held:

- each held version is listed, and the highest is the default;
- samples are not listed — their source is a git remote the air gap does not have;
- `icon` is present and empty. Upstream's icons are URLs a disconnected browser cannot reach, and Che drops an index entry that has no `icon` at all;
- the `arch` and `deprecated=false` filters apply; the schema-version filters do not.

A bundle carries cached stacks, not published ones. Publish a local stack on each side from the same directory: the packing is deterministic, so both instances serve the same digests.

## See also

- [RFC 0035](../rfc/0035-devfile-registry) — the protocol as `registry.devfile.io` serves it, and why the tag is the chokepoint
- [Generic mirror](./generic) — caches a devfile registry without policy, and blocks nothing
