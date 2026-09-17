# Nix binary cache

Proxy and cache a Nix *binary cache* — the substituter protocol — as a typed registry, so a store path can be **blocked** rather than merely cached. The `{hash}.narinfo` documents are the enforcement point: one is fetched for every store path in a closure, always, before any bytes move.

**Read this first: blocking a binary does not block the software.** A machine that has the derivation — every NixOS machine does — builds `hello-1.0.0.2` from source when no cache serves it, and nothing in this protocol can stop that. The lever an estate has is `max-jobs = 0` on the machines that must not build, and that is Nix configuration, not this proxy's.

## At a glance

| | |
|---|---|
| **Config type** | `nix` |
| **Default upstream** | `cache.nixos.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | the store name is the coordinate — `hello-1.0.0.2-doc` is `hello` at `1.0.0.2-doc` — and the 32-character store hash is the artifact |
| **Private publish** | ✅ `nix copy --to` |
| **Client switch** | `substituters` in `nix.conf` |

## Proxy setup

`substituters` is the one switch. Replace `<registry>` with your configured registry name:

```ini
# /etc/nix/nix.conf, or nixConfig in a flake for a trusted user.
#
# ?priority=30 sorts this cache before cache.nixos.org (priority 40; lower
# wins) when a machine lists both. Alone, the value is irrelevant.
substituters        = https://batlehub.example.com/proxy/<registry>/nix?priority=30
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=
```

The key is the **upstream's**, unchanged: this instance relays every `Sig:` byte-exact and signs nothing it proxied, so your machines keep verifying with the key they already trust. Nothing needs to be added to `trusted-public-keys` to read through the proxy.

Unprivileged users can only use substituters listed in `trusted-substituters` or in the daemon's own list, which is the operator's lever for making this instance the only cache a machine reads.

Your administrator's registry block:

```toml
[[registries]]
name      = "<registry>"
type      = "nix"
mode      = "proxy"
upstreams = ["https://cache.nixos.org"]   # the default

# Refuse to relay a narinfo carrying no Sig: at all. Off by default — a
# content-addressed path (CA:) legitimately has none, and the client's own
# require-sigs is the check that protects its store. On, it closes the one
# case the client cannot see: a mirror that silently dropped signatures.
require_upstream_sigs = false

[registries.rbac]
# narinfo and nix-cache-info are listings; NARs, .ls and realisations are reads.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list"]
admin     = ["*"]
```

Then build as you always did — substitution goes through this instance, and a
store path can be pulled from it by hand:

```sh
nix build nixpkgs#hello
nix copy --from "https://batlehub.example.com/proxy/<registry>/nix" /nix/store/…-hello-2.12.2
```

A blocked store path answers `404` on its narinfo, which to Nix means *"not in
this cache"*: it consults the next substituter, or builds from source.

## Authentication

**Nix sends no credentials unless you tell it where they are.** The downloader is libcurl with `CURLOPT_NETRC_FILE` set from the `netrc-file` setting and `CURL_NETRC_OPTIONAL`; the default path is a dummy. There is no header setting for substituters, so an authenticated registry needs that one line, and the path must be absolute:

```ini
netrc-file = /etc/nix/netrc
```

```text
machine batlehub.example.com
  login token
  password <your-token>
```

## What is blocked, and what a client sees

The package and version are Nix's own. `DrvName` splits a store name at *the first dash not followed by a letter*, which is the same parser `nix-env -u` and `lib.getVersion` use — so the version you read off `nix-env -q` is the version a block takes:

| store name | package | version |
|---|---|---|
| `curl-8.21.0` | `curl` | `8.21.0` |
| `hello-1.0.0.2-doc` | `hello` | `1.0.0.2-doc` |
| `php-curl-8.4.25` | `php-curl` | `8.4.25` |
| `gcc-wrapper-14+` | `gcc-wrapper` | `14+` |
| `source` | `source` | `-` (no version part) |

Two builds of one version differ only in the 32-character hash, and a block covers **all** of them: the hash is the artifact, not a thing an admin names.

A blocked path answers `404` on its `.narinfo`, on its `.ls`, and on every `nar/{hash}/…` request. To Nix a `404` is not an error — it is *"this cache does not have it"*. It records the negative result for `narinfo-cache-negative-ttl` (3600 s) and moves to the next substituter, or builds, saying so in its own words. There is no mid-transfer refusal to design around: the narinfo is asked for before the NAR, always.

Because the NAR route re-derives the coordinate from the store hash in its own path, a client holding a narinfo it fetched *before* the block — Nix caches those for 30 days — is refused at the bytes too, rather than being handed them.

## The one line this instance changes

A narinfo is relayed with exactly one field rewritten and none removed:

```text
StorePath: /nix/store/0001npbf…-hslua-aeson-2.3.2-doc
URL: nar/0001npbf…/075lhsj….nar.zst        ← rewritten
Compression: zstd
FileHash: sha256:10k72lz1…
FileSize: 46064
NarHash: sha256:075lhsj…
NarSize: 226848
References: ghpayap4…-aeson-2.2.4.1-doc …
Deriver: y1h1bh5g…-hslua-aeson-2.3.2.drv
Sig: cache.nixos.org-1:21qiHy652KfJ…      ← untouched
```

`URL:` is rewritten so the NAR request carries the store hash its coordinate is derived from — without it, a NAR request names no package and nothing can be blocked, cached or counted. It is safe to rewrite because **the signature does not cover it**: the signed fingerprint is `1;{StorePath};{NarHash};{NarSize};{References}` and nothing else. Your client verifies exactly what it would have verified against the upstream, with the upstream's key.

If a machine still holds a narinfo from before this registry existed, it asks for the upstream's own `nar/{filehash}.nar.zst`. This instance keeps a reverse index of those, written whenever a narinfo is served; a hit is served under its coordinate, and a miss is a `404`, which makes Nix refetch the narinfo, get the rewritten URL and succeed. One extra round trip, once per path.

## `nix-cache-info`

Relayed as the upstream sends it. `StoreDir` is not a knob — a client whose own store directory differs refuses the whole cache with *"binary cache '…' is for Nix stores with prefix '…', not '…'"* — and `Priority` is the one value an operator would change, which they do on their own `substituters` line with `?priority=`.

A registry with no upstream to relay from composes one: `StoreDir: /nix/store`, `WantMassQuery: 1`, `Priority: 30`.

## The release-age gate

**The substituter protocol carries no dates anywhere.** A narinfo has hashes, a closure and a deriver, and nothing else — so every store path reaches a `release_age_gate` rule *undated*, and the `deny_missing_timestamp` field is not a tie-break, it is the whole rule:

- `true` refuses every substitution on the registry;
- `false` makes the gate inert.

There is no default. A `release_age_gate` on a `nix` registry that does not set the field is refused at startup rather than inheriting one silently.

## Publishing

A `local` or `hybrid` registry accepts `nix copy --to`:

```sh
nix copy --to "https://batlehub.example.com/proxy/<registry>/nix" ./result
```

`nix copy` sends the NAR **before** it sends the narinfo — the bytes arrive
naming no package at all — so this instance parks them until the narinfo claims
them. The narinfo is what carries the coordinate, and claiming is also when the
bytes are checked: `FileHash`, `FileSize`, `NarHash` and `NarSize` are all
recomputed here, from the bytes this server holds, before anything is signed.
A document that disagrees with its bytes is a `400` naming the field, and
nothing is stored.

A NAR can only be claimed by the narinfo of **the same publisher** that
uploaded it.

#### What the staging area will hold

The upload that arrives first names no package, so it cannot be authorized
against one. Three limits apply to it instead:

| | |
|---|---|
| **Who** | A caller who holds `releases:publish` on the registry or on one of its namespaces. A grant scoped to a *single package* does not reach this request — no package is named yet — and the `403` says so. |
| **How long** | An unclaimed NAR is swept **an hour** after it arrives — `pending_nar_ttl_secs`. A publish claims its bytes seconds later, so this is a limit on abandoned uploads, not on slow ones. |
| **How many** | **64** unclaimed uploads per publisher at once — `max_pending_nars`. The 65th answers `429` until one is claimed or swept. |

Claiming a NAR deletes it from the staging area, so a completed publish holds
nothing there.

Both are per registry:

```toml
[[registries]]
name = "<registry>"
type = "nix"
mode = "local"
pending_nar_ttl_secs = 3600   # the default
max_pending_nars     = 64     # the default
```

**Raise the window for a slow link, not for a large closure.** `nix copy` walks
path by path, so the gap between a NAR and the narinfo that claims it is one
upload and one round trip whatever the closure's size — the setting is about
latency, not volume.

**The floor under the count is the client's own concurrency.** `nix copy`
parallelises over `http-connections`, which defaults to 25, and each in-flight
path holds one unclaimed NAR; a cap below that refuses copies for their shape
rather than for their size. The server warns at load about either limit set
tight enough to do that, and refuses `0` for both — a window of zero sweeps
every NAR before its own narinfo arrives, and a count of zero refuses every
publish.

### Signing what it hosts

```toml
[registries.nix_signing]
seed_hex = "${NIX_SIGNING_SEED}"   # openssl rand -hex 32
key_name = "batlehub-nix-1"        # optional; default batlehub-{registry}-1
```

The registry signs the fingerprint it just verified — never the hashes the
document claimed. `key_name` is the half of a `trusted-public-keys` entry
before the colon, so it has to be stable across restarts and unique among the
caches a client trusts; rotation is a new suffix, which is Nix's own model.

Hand the key to every client:

```sh
curl -s https://batlehub.example.com/proxy/<registry>/nix/public-key
# batlehub-nix-1:<base64>
```

```ini
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= batlehub-nix-1:<base64>
```

**Without a `nix_signing` key a local registry still works, and every stock
client will refuse what it serves** — Nix's `require-sigs` defaults to on, and
an unsigned path fails with *"cannot add path '…' because it lacks a signature
by a trusted key"*. The server warns about this at startup rather than refusing
to start, because a fleet running `require-sigs = false` is a legitimate lab.

A publisher's own signatures are kept: `nix copy` signs client-side when the
store has `secret-key-files`, and that is somebody's provenance. The one
exception is a `Sig:` under **this registry's** key name, which is dropped — a
publisher cannot mint the signature that says this server checked something.

### Compression

`nix copy --to` compresses with **xz** by default. This registry verifies
`xz`, `zstd` and `none`; Nix's other ten algorithms are refused at publish,
because a NAR this server cannot decompress is one whose `NarHash` it cannot
check — and signing an unchecked hash is the thing the verification exists to
prevent. Choose one with the store URL:

```sh
nix copy --to "https://batlehub.example.com/proxy/<registry>/nix?compression=zstd" ./result
```

## Air-gapped estates

A store path travels in the [air-gap bundle](../operations/air-gap) with the facts its bytes do not carry — its closure (`References:`), its `Deriver`, and every `Sig:`. The disconnected instance composes the narinfo from those and serves the **publisher's** signature untouched, so a client on the far side verifies with the same key it uses connected, and the estate signs nothing.

A store path the bundle does not carry answers `503` rather than `404`: it exists, it is simply not here.

### Endpoint reference

<!-- BEGIN endpoints: proxy/nix -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/nix/{hash}.ls` | `{hash}.ls` — a JSON listing of a NAR's contents, for `nix store ls` alone. |
| `GET` | `/proxy/{registry}/nix/{hash}.narinfo` | One store path's metadata — the chokepoint every substitution goes through. |
| `PUT` | `/proxy/{registry}/nix/{hash}.narinfo` | `PUT {hash}.narinfo` — the document that names the coordinate, and where this registry signs. |
| `GET` | `/proxy/{registry}/nix/log/{drv}` | `log/{drv}` — a build log, read by `nix log`. |
| `GET` | `/proxy/{registry}/nix/nar/{file}` | A NAR asked for in the **upstream's** own shape, resolved through the reverse index. |
| `PUT` | `/proxy/{registry}/nix/nar/{file}` | `PUT nar/{file}` — the NAR, arriving before anything knows what it is. |
| `GET` | `/proxy/{registry}/nix/nar/{hash}/{file}` | The NAR, under the coordinate the rewritten `URL:` gave it. |
| `GET` | `/proxy/{registry}/nix/nix-cache-info` | `nix-cache-info` — the three lines a client reads once per substituter. |
| `GET` | `/proxy/{registry}/nix/public-key` | `GET public-key` — the line an operator pastes into `trusted-public-keys`. |
| `GET` | `/proxy/{registry}/nix/realisations/{id}.doi` | `realisations/{id}.doi` — the derivation-to-output mapping of a content-addressed derivation. |
<!-- END endpoints -->

## Not supported

- **`s3://` and `file://` stores.** Nix speaks the same layout over both; this registry is the `https://` store. An S3 bucket is what the storage backend is for, behind the HTTP surface.
- **`nix-serve`'s dynamic behaviour** — computing a narinfo from a local `/nix/store` on request. This instance has no store; it has what was proxied.
- **Re-signing anything the upstream signed.** A proxied narinfo keeps its signatures exactly.
- **Verifying upstream signatures server-side.** Your client does it, with the keys you chose; a proxy that also verified would need the same key list kept in step in a second place.
- **Blocking a single store path.** A version rebuilt with a different hash is the same software, and an admin blocking a CVE wants all of them.
- **`channels.nixos.org` and the channel tarballs** — a different host and a different protocol. The [`generic`](./generic) kind mirrors those.
- **Flake inputs from forges.** A `github:` input is the [`github`](./github) kind's business.
