# RubyGems

Proxy and cache rubygems.org for Bundler and the `gem` CLI, or host private gems. BatleHub serves gem downloads, the version index, and the REST info API, gated by RBAC and the release-age gate.

## At a glance

| | |
|---|---|
| **Config type** | `rubygems` |
| **Default upstream** | `rubygems.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `gem push` |
| **Air gap** | offline, the compact index (`/versions`, `/info/{gem}`, `/names`) and the versions JSON API are composed from the held gems, each gem's dependencies read off its gemspec at import; `bundle install` resolves against it |

## Proxy setup

Install from your registry. Replace `<registry>` with your configured registry name:

```sh
gem install rake --source https://batlehub.example.com/proxy/<registry>/
```

Or in a `Gemfile`:

```ruby
source "https://batlehub.example.com/proxy/<registry>" do
  gem "rake"
end
```

## Publishing (local / hybrid)

### Server configuration

```toml
[[registries]]
type = "rubygems"
name = "internal-gems"
mode = "local"          # or "hybrid" to fall back to rubygems.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

For hybrid mode add `upstreams = ["https://rubygems.org"]`.

### Client setup

**Option A — environment variable (recommended for CI):**

```sh
export GEM_HOST_API_KEY="Bearer <your-token>"
```

gem sends the value of `GEM_HOST_API_KEY` verbatim as the `Authorization` header, so the `Bearer ` prefix is required.

**Option B — `~/.gem/credentials` (create if absent, `chmod 600` after):**

```yaml
---
:batlehub: "Bearer <your-token>"
```

The symbol (`:batlehub:`) is an arbitrary name you choose. The value must include the `Bearer ` prefix because gem sends it verbatim as the `Authorization` header. Reference the entry by name with `--key` when pushing.

### Publish

```sh
# Using GEM_HOST_API_KEY (no --key needed)
GEM_HOST_API_KEY="Bearer <your-token>" \
  gem push my-gem-1.0.0.gem --host https://batlehub.example.com/proxy/internal-gems/

# Using ~/.gem/credentials with a named key
gem push my-gem-1.0.0.gem \
  --host https://batlehub.example.com/proxy/internal-gems/ \
  --key batlehub
```

### Install

```sh
# Using GEM_HOST_API_KEY
GEM_HOST_API_KEY="Bearer <your-token>" \
  gem install my-gem --source https://batlehub.example.com/proxy/internal-gems/

# Using a named credentials key
gem install my-gem \
  --source https://batlehub.example.com/proxy/internal-gems/ \
  --key batlehub
```

Or in a `Gemfile`:

```ruby
source "https://batlehub.example.com/proxy/internal-gems" do
  gem "my-gem"
end
```

### Yank / unyank

```sh
# Yank
curl -X DELETE \
  -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-gems/api/v1/gems/yank?gem_name=my-gem&version=1.0.0"

# Unyank
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-gems/api/v1/gems/unyank?gem_name=my-gem&version=1.0.0"
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/rubygems -->
| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/proxy/{registry}/api/v1/gems` | Publish a gem (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/api/v1/gems/{name}.json` | Get gem information JSON (latest version). |
| `PUT` | `/proxy/{registry}/api/v1/gems/unyank` | Unyank a gem version (local/hybrid registries only). |
| `DELETE` | `/proxy/{registry}/api/v1/gems/yank` | Yank a gem version (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/api/v1/versions/{name}.json` | List all versions of a gem. |
| `GET` | `/proxy/{registry}/gems/{filename}` | Download a gem file. |
| `GET` | `/proxy/{registry}/info/{gem}` | One gem's versions and dependencies — what Bundler resolves against. |
| `GET` | `/proxy/{registry}/latest_specs.4.8.gz` | Serve the latest-versions gem index (latest_specs.4.8.gz). |
| `GET` | `/proxy/{registry}/names` | Every gem name in the registry. |
| `GET` | `/proxy/{registry}/prerelease_specs.4.8.gz` | Serve the prerelease gem index (prerelease_specs.4.8.gz). |
| `GET` | `/proxy/{registry}/quick/Marshal.4.8/{filename}` | Serve a compressed gemspec file. |
| `GET` | `/proxy/{registry}/specs.4.8.gz` | Serve the full gem index (specs.4.8.gz). |
| `GET` | `/proxy/{registry}/versions` | The whole-registry version list Bundler fetches first. |
<!-- END endpoints -->

---

## The compact index in each mode

**The compact index is what `bundle install` reads** — `/versions` first, then
`/info/{gem}` per gem. The JSON APIs below it are a fallback Bundler reaches for
only when the compact index is absent, so what these three documents say is what
resolution sees.

| Mode | `/versions`, `/info/{gem}`, `/names` |
| --- | --- |
| `proxy` | upstream's documents, filtered |
| `hybrid` | upstream's documents with this registry's gems appended |
| `local` | generated from this registry's gems; upstream is not consulted |

Dependencies are read from each gem's gemspec when it is published and written
into `/info/{gem}`, because that is where the resolver looks for them. Only
runtime dependencies — `:development` ones are not part of what an installer
resolves.

::: tip Publishing and `bundle install`
A gem is visible to Bundler as soon as it is published; there is no index to
rebuild. Earlier releases served all three compact documents from upstream in
every mode, so a gem published to a `local` registry could not be installed from
it at all.
:::

### Incremental fetch

Bundler caches these documents and asks for the tail of what it holds, with an
`If-None-Match` describing its copy and a `Range`. All three answer:

- `304` when the copy is current;
- `206` with just the tail when the copy is a prefix of the current document —
  checked, not assumed, by comparing the client's validator against the digest
  of our own prefix;
- `200` with the whole document otherwise, which is also what any client that
  sends no `Range` gets.

Nothing needs configuring, and a client that ignores all of it still works. The
practical effect is on `/versions`, which describes the whole registry: in
`proxy` and `hybrid` mode that is the upstream index — tens of megabytes against
rubygems.org — and it was previously transferred in full on every resolve.

## Blocked versions

The compact index is filtered.
`/info/{gem}` drops the blocked version's line. `/versions` drops it from that
gem's comma-separated list, and drops the gem's line entirely when every version
is blocked.

`/versions` describes the whole registry, so its blocked set comes from a
30-second snapshot rather than a per-request query — the same trade conda's
`repodata.json` makes, and for the same reason: re-querying the whole block list
on the hottest path in the ecosystem is not worth the seconds it buys. **A new
block reaches `/versions` within that TTL; the download gate refuses the bytes
immediately either way.** `/info/{gem}` is per-gem and has no such lag.

When a gem's line changes, its `/info` checksum is recomputed. That field is how
Bundler decides whether to re-fetch `/info/{gem}`; leaving it alone would let a
client keep serving a copy it cached before the block, so the block would never
reach the resolver. Lines that did not change keep upstream's checksum byte for
byte, so a block on one gem does not make every client re-download every other
gem's metadata.

`/names` is **not** filtered. It lists gem names and no versions, so a block has
nothing in it to hide — and removing the name would tell Bundler the gem does not
exist, which is a worse answer than "some of its versions are restricted".

`/api/v1/versions/{name}.json` drops the blocked entry.

`/api/v1/gems/{name}.json` describes the gem at exactly one version, so it is
**rebuilt** around the newest version that is still allowed. The gem-level
fields survive; the fields that describe the hidden *release* — its checksum,
its download URL, its own dates — are removed, because carrying a checksum onto
a different version would hand a client a hash that can never match.

The Marshal indexes (`specs.4.8.gz`, `quick/Marshal.4.8/*`) are **not** filtered.
Hiding a version from them would need a Ruby Marshal encoder — and nothing reads
them: Bundler resolves from the compact index above, and the JSON APIs answer
every other client released this decade.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

`gem` sends `GEM_HOST_API_KEY` verbatim as the `Authorization` header, so the `Bearer ` prefix is required:

```sh
export GEM_HOST_API_KEY="Bearer $BATLEHUB_TOKEN"
```

Alternatively, store it in `~/.gem/credentials` (`chmod 600`) under a key name and reference it with `--key`:

```yaml
---
:batlehub: "Bearer <your-token>"
```

## Notes

- Gems are cached after the first download.
- To mirror rubygems.org transparently for an existing `Gemfile`, use `bundle config set mirror.https://rubygems.org/ …` instead of editing the `source`.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
