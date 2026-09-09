# Cargo

Proxy and cache crates.io, or host private crates. BatleHub implements the Cargo [sparse registry protocol](https://doc.rust-lang.org/cargo/reference/registry-protocols.html#sparse-protocol), serving the index and `.crate` downloads through the cache. Index checksums match the cached `.crate` files, so verification keeps working.

## At a glance

| | |
|---|---|
| **Config type** | `cargo` |
| **Default upstream** | `crates.io` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `cargo publish` |
| **Air gap** | offline, the sparse index is composed from the held crates, their dependencies read off each crate's manifest at import; a crate imported without its manifest is not listed |

## Proxy setup

Replace the default crates.io source so all `cargo add` / `cargo build` requests go through BatleHub:

```toml
# .cargo/config.toml (project) or ~/.cargo/config.toml (global)
[source.crates-io]
replace-with = "batlehub"

[source.batlehub]
registry = "sparse+https://batlehub.example.com/proxy/<registry>/registry/"
```

The `index` / `registry` URL uses the `sparse+` prefix and must end with `/registry/`.

## Publishing (local / hybrid)

### Server configuration

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"          # or "hybrid" to fall back to crates.io

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

For hybrid mode add:
```toml
upstreams = ["https://static.crates.io/crates"]
index_url = "https://index.crates.io"
```

### Client setup

Edit `~/.cargo/config.toml` or `.cargo/config.toml` in the project root:

```toml
[registries.internal]
index = "sparse+https://batlehub.example.com/proxy/internal/registry/"
token = "<your-token>"
```

Alternatively export the token as an environment variable (useful in CI):

```sh
export CARGO_REGISTRIES_INTERNAL_TOKEN=<your-token>
```

### Publish

```sh
cargo publish --registry internal
```

Cargo serialises crate metadata + the `.crate` archive into a single binary payload and sends it to `PUT /proxy/internal/api/v1/crates/new`. The checksum is verified server-side.

### Depend on a privately published crate

```toml
# Cargo.toml
[dependencies]
my-lib = { version = "0.1", registry = "internal" }
```

### Yank / unyank a version

```sh
cargo yank --registry internal my-lib@0.1.0
cargo yank --undo --registry internal my-lib@0.1.0
```

### Verify

```sh
cargo add my-lib --registry internal
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/cargo -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{name}/{version}/download` | Download a `.crate` file for a specific version. |
| `GET` | `/proxy/{registry}/api/v1/crates` | `cargo search`. |
| `PUT` | `/proxy/{registry}/api/v1/crates/{name}/{version}/unyank` | Unyank a previously yanked crate version. |
| `DELETE` | `/proxy/{registry}/api/v1/crates/{name}/{version}/yank` | Yank a published crate version. |
| `GET` | `/proxy/{registry}/api/v1/crates/{name}/owners` | List owners of a crate (`cargo owner --list`). |
| `PUT` | `/proxy/{registry}/api/v1/crates/{name}/owners` | `cargo owner --add`. |
| `DELETE` | `/proxy/{registry}/api/v1/crates/{name}/owners` | `cargo owner --remove`. |
| `PUT` | `/proxy/{registry}/api/v1/crates/new` | Publish a new crate version (`cargo publish`). |
| `GET` | `/proxy/{registry}/registry/{path}` | Cargo sparse registry index entries. |
| `GET` | `/proxy/{registry}/registry/config.json` | Cargo sparse registry `config.json`. |
<!-- END endpoints -->

---

## Blocked versions

Cargo is the one ecosystem where a blocked version is **marked rather than
removed**. The sparse-index line stays, with `"yanked": true` set on it.

That is cargo's own mechanism for "this exists, do not select it": resolution
skips a yanked version, while an existing `Cargo.lock` that already pins it
still resolves — and then meets the download gate, which answers with the
operator's reason. Deleting the line instead would make cargo report the crate
as never having had that version, and the developer would get "no matching
package found" instead of an explanation.

The sparse-index route is authorised like every other proxied read: a client
without read access to the registry gets a `403` rather than the crate list.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Cargo sends the `token` from the `[registries.<name>]` block. In CI, set it via the environment instead: `export CARGO_REGISTRIES_INTERNAL_TOKEN=$BATLEHUB_TOKEN` (uppercase the registry name).

## Notes

If `cargo publish` fails with "invalid token", verify the `index` URL ends with `/registry/`. Checksums returned by the sparse index match the cached `.crate` files, so `cargo verify-project` continues to work.

## Search

``cargo search`` is answered in three steps: a cached result for that query, then the
upstream, then — when the upstream is unreachable — **the packages this registry
already holds**. An outage degrades search to what BatleHub can honestly answer
for, rather than to an error or to an empty result list.

Every response carries `X-BatleHub-Cache: hit | miss | stale`. `stale` means the
upstream could not be reached and the answer came from the cache or from the held
set, so a short result list is never silently presented as complete.

::: warning Search queries reach the upstream
Step two forwards the query string to the configured upstream. Search terms are a
record of what your organisation is looking for. Set `serve_stale = false` and
leave the registry without an upstream if you want the held-package answer and no
egress at all.
:::

Blocked versions are removed from results, and the reported total is adjusted to
match — clients paginate by offset, so a silently shortened page would make the
next one skip a result.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
