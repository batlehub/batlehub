# Alpine (apk)

Proxy an Alpine mirror and, in `local`/`hybrid` mode, host your own repository: publish `.apk` packages and BatleHub regenerates the per-architecture `APKINDEX.tar.gz`, signing it with an RSA key every shipping apk trusts.

This is the one member of the OS family whose artifacts carry a **coordinate**. A `.apk` file name is `{name}-{pkgver}-r{N}.apk`, so a package can be blocked, age-gated, counted and shown in the console — unlike a `.deb` or an `.rpm`, which the proxy can only address by path.

## At a glance

| | |
|---|---|
| **Config type** | `apk` |
| **Default upstream** | none — set `upstreams` explicitly for proxy/hybrid |
| **Modes** | proxy · local · hybrid |
| **Addressing** | path-addressed, with a coordinate on every `.apk` |
| **Private publish** | ✅ `curl -X PUT … /apk/upload` |
| **Air gap** | planned: the composed index is this instance's own document, so unlike the other OS kinds it *can* be re-signed offline |

## The index is relayed byte-exact, and why

`APKINDEX.tar.gz` is RSA-signed over its own bytes, and **every apk that ships verifies that signature before reading a byte of the index**. Editing one line of it invalidates the signature, and the client's response is to refuse the whole repository — not to skip the edited entry.

So BatleHub relays the upstream index unchanged, exactly as it relays the `deb`, `rpm` and `pacman` indexes, and enforces a block where it can: at the `.apk` itself, with a `403` before any byte leaves the site.

**The honest limit**, because it changes what a block does for you:

- An Alpine branch holds **one version per package**. Blocking it is blocking the package until the branch bumps it — there is no older version for the solver to fall back to.
- The index still lists the blocked version, so `apk add <pkg>` *selects* it and then fails on the download. apk 2.14 prints `ERROR: <pkg>-<version>: Permission denied`; apk 3 prints `HTTP 403: Forbidden`. In both, the transaction fails and nothing is installed.
- In **local** mode none of this applies: that index is BatleHub's own document, so a blocked version is simply absent from it and apk reports its own "unable to select".

## Proxy setup

Point `/etc/apk/repositories` at the registry. apk appends `{arch}/APKINDEX.tar.gz` and `{arch}/{file}.apk` itself, so each line names a branch and a repository:

```sh
# /etc/apk/repositories
https://batlehub.example.com/proxy/<registry>/apk/v3.22/main
https://batlehub.example.com/proxy/<registry>/apk/v3.22/community
```

apk 3 (Alpine 3.23 and later) also accepts the components form, which puts the same requests on the wire:

```sh
https://batlehub.example.com/proxy/<registry>/apk/v3.24 main community
```

Migrating a stock image is one line:

```sh
sed -i 's#https://dl-cdn.alpinelinux.org/alpine#https://batlehub.example.com/proxy/<registry>/apk#' \
  /etc/apk/repositories
```

Then `apk update` and `apk add` as usual. Alpine's own signing keys are already in the image, and the relayed index verifies against them.

### The `upstreams` entry is the tree root

```toml
[[registries]]
name      = "alpine"
type      = "apk"
mode      = "proxy"
upstreams = ["https://dl-cdn.alpinelinux.org/alpine"]   # the root, not a branch
```

Not `…/alpine/v3.22` and not `…/alpine/v3.22/main`: the client appends the branch and repository itself, so a root that already names one puts the index at `…/v3.22/v3.22/main/…`. BatleHub refuses that at startup rather than letting it fail on the first `apk update`.

Bound the tree with `path_allow` if you only run some branches:

```toml
path_allow = ["v3.22/**", "v3.24/**", "latest-stable/**"]
```

## Publishing (local / hybrid)

Upload a package with a `PUT`. The name, version and architecture are read from the embedded `.PKGINFO` — never from the file name you send — and BatleHub stores the file, regenerates `{arch}/APKINDEX.tar.gz` and signs it:

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello-1.0-r0.apk \
  https://batlehub.example.com/proxy/<registry>/apk/upload
```

The package does **not** need to be signed itself. When apk installs from a repository it checks the package's control checksum against the index's `C:` field and never consults the package's own signature, so an unsigned package installs fine from a signed index.

### What the package has to be

**A v2 `.apk`, and `apk mkpkg` does not build one.** apk-tools 3's own builder writes the v3 (ADB) container, which a v2 `APKINDEX` has no shape to describe and no Alpine branch ships an index for. An upload in that format is refused with a `400` naming it. Use `abuild`, which still writes v2.

Two more requirements come from apk 3 rather than from BatleHub, and apk 2.14 accepts packages that break either — so a package that installs on Alpine 3.22 and fails on 3.24 is almost certainly one of these:

| Requirement | apk 3 says, if it is missing |
| --- | --- |
| `.PKGINFO` carries `datahash` (sha256 of the compressed data member) | `v2 package format error` |
| The data member names `usr/…` directly, with no `.` root entry | `file format is invalid or inconsistent` |

`abuild` satisfies both. A package assembled by hand has to satisfy them itself.

### Signing the index

```toml
[registries.apk_signing]
key_name        = "internal-apk@example.com-5f3a1c2e.rsa.pub"
private_key_pem = "${APK_SIGNING_KEY_PEM}"   # RSA, PEM, 2048 bits or more
```

Generate the pair once:

```bash
openssl genrsa -out apk-signing.pem 4096
openssl rsa -in apk-signing.pem -pubout -out internal-apk@example.com-5f3a1c2e.rsa.pub
```

**`key_name` must match the file name on the client exactly.** apk opens the public key *by the name the signature entry carries*, inside `/etc/apk/keys/`. A mismatch is not an error message — it is an untrusted index. Renaming it later means re-installing it on every client, so choose it once.

Consumers install it before their first `apk update`:

```sh
curl -fsSL -o /etc/apk/keys/internal-apk@example.com-5f3a1c2e.rsa.pub \
  https://batlehub.example.com/proxy/<registry>/apk/keys/internal-apk@example.com-5f3a1c2e.rsa.pub
echo https://batlehub.example.com/proxy/<registry>/apk >> /etc/apk/repositories
apk update
```

The key is served live, before anything has been published, so a client can be set up first.

An unsigned local repository is uninstallable by every apk unless the client passes `--allow-untrusted` — which **also switches off the package identity check**, so it is not documented here as an option. If you want one anyway, say so on the record with `apk_unsigned = true`; BatleHub refuses to infer it from silence.

### Rotating the key

Adding a key to a fleet takes as long as the slowest machine, and an index signed only with the new one is uninstallable everywhere until every machine has it. So the rotation is additive: list the outgoing key under `previous_keys` and BatleHub signs each index with **both**.

```toml
[registries.apk_signing]
key_name        = "internal-apk@example.com-9e21ff40.rsa.pub"   # the new one
private_key_pem = "${APK_SIGNING_KEY_PEM}"

[[registries.apk_signing.previous_keys]]
key_name        = "internal-apk@example.com-5f3a1c2e.rsa.pub"   # the outgoing one
private_key_pem = "${APK_SIGNING_KEY_PEM_OLD}"
```

apk installs from the first `.SIGN.*` entry whose key file it holds, so a machine that has the new key takes the new signature and one that does not falls through to the old — neither notices. Both names are served on the key route, so a straggler can still fetch the old file. Remove the `previous_keys` entry once every client has the new key; an empty list is a finished rotation.

The window is one flat list: a retired key may not carry retired keys of its own, and a name may not appear twice.

## Authentication

apk's downloader is its vendored libfetch. It has no header configuration and no netrc: the only credential mechanism is HTTP Basic embedded in the repository URL.

```sh
https://<user>:<token>@batlehub.example.com/proxy/<registry>/apk/v3.22/main
```

That works, and it leaks the token into `apk update`'s own `fetch https://…` output. Prefer `anonymous` read verbs on the registry plus an authenticating ingress, and treat the URL form as the last resort — the same advice the [generic registry](/registries/generic) page gives for the same reason.

## Notes

- `keys/` is a **reserved prefix** under `…/apk/`: it serves the signing key, so it cannot be used to proxy an upstream path of that name. Alpine's own tree has none.
- The age gate has a real date to work with here: every package an `APKINDEX` lists carries its build time in `t:`. A `release_age_gate` rule on an `apk` registry must set `deny_missing_timestamp` explicitly, because the one undated case — a version the cached index no longer lists — has two opposite right answers and BatleHub will not pick one for you.
- `latest-stable` is a symlink on the mirror to the newest stable branch. BatleHub treats it as an ordinary path, so the same file reached through `v3.24/` and through `latest-stable/` is two cache entries and one stored copy.
- Publishing requires the registry in `local` or `hybrid` mode — ask your administrator.
- **Air-gapped instances answer `apk update`.** With no upstream to relay, a disconnected BatleHub composes `APKINDEX.tar.gz` over the packages it actually holds and signs it with this registry's key, so a client resolves through a listing that is true by construction: every version it names is served by the next request, and a package the estate was never given is simply absent. That needs `[registries.apk_signing]` on the registry even in `proxy` mode — without a key the index cannot be signed, and the request stays the `503` of an air-gapped miss. `apk` is the only OS-package kind that can do this: `deb`, `rpm` and `pacman` indexes are signed by keys the estate does not hold.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
