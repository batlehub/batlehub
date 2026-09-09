# Composer (PHP)

Proxy and cache Packagist for PHP Composer, or host private packages. BatleHub implements the Packagist v2 protocol (`packages.json` + `p2/` metadata endpoints), so Composer treats it as a native Composer repository — gated by RBAC and the release-age gate.

## At a glance

| | |
|---|---|
| **Config type** | `composer` |
| **Default upstream** | `repo.packagist.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `curl -X POST …/api/upload` |
| **Air gap** | offline, `p2` is composed from the held dists, each entry its `composer.json` read at import; the `~dev` variant answers empty |

## Proxy setup

Add a repository entry to `composer.json`. Replace `<registry>` with your configured registry name:

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/<registry>/"
    }
  ]
}
```

Install as usual:

```sh
composer install
composer require symfony/console
```

## Publishing (local / hybrid)

Composer packages are uploaded as ZIP archives containing a `composer.json`. BatleHub reads `name` (format `vendor/package`) and `version` from `composer.json` when a package is uploaded, so no separate metadata step is required.

### Server configuration

```toml
[[registries]]
type = "composer"
name = "internal-composer"
mode = "local"          # or "hybrid" to fall back to repo.packagist.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

For hybrid mode add `upstreams = ["https://repo.packagist.org"]`.

### Package format

A Composer package is a ZIP archive with a `composer.json` at the archive root (or inside a single top-level subdirectory — standard practice when archiving a git checkout). The `composer.json` must include `name` and `version`:

```json
{
  "name": "my-vendor/my-package",
  "version": "1.0.0",
  "description": "My private library",
  "autoload": {
    "psr-4": { "MyVendor\\MyPackage\\": "src/" }
  }
}
```

Build the archive from your project directory:

```sh
# Archive from the current directory (top-level files directly in ZIP)
zip -r my-vendor-my-package-1.0.0.zip . -x "*.git*" -x "vendor/*"

# Or use git archive for a clean export
git archive --format=zip HEAD -o my-vendor-my-package-1.0.0.zip
```

If your `composer.json` has no `version` field (common in version-controlled projects), pass it as a query parameter when uploading.

### Upload

```sh
# composer.json contains a "version" field
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @my-vendor-my-package-1.0.0.zip \
  "https://batlehub.example.com/proxy/internal-composer/api/upload"

# Override (or supply) the version via query parameter
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @my-vendor-my-package.zip \
  "https://batlehub.example.com/proxy/internal-composer/api/upload?version=1.0.0"
```

The upload records the archive's **SHA-1** alongside it and publishes that as
`dist.shasum`, because that is the digest Composer recomputes over the file it
downloads. A package uploaded before that digest was recorded has no
stored SHA-1 and is served with no `shasum` at all — Composer installs it
without verifying the digest rather than refusing it. Re-upload it (same
version, after a yank, or as a new version) to get the checksum back.

### Client setup

Composer supports two ways to supply credentials. Prefer `auth.json` over inline headers so credentials stay out of source control.

**`auth.json`** (place in the project root or `~/.composer/auth.json` for global use):

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "token",
      "password": "<your-token>"
    }
  }
}
```

Composer sends this as `Authorization: Basic base64("token:<your-token>")`. BatleHub extracts the password field and matches it against your configured token.

**Inline header in `composer.json`** (alternative when `auth.json` is not an option):

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/internal-composer/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer <your-token>"]
        }
      }
    }
  ]
}
```

### Install

With credentials configured, add the repository to `composer.json` and require the package:

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/internal-composer/"
    }
  ],
  "require": {
    "my-vendor/my-package": "^1.0"
  }
}
```

```sh
composer install
# or
composer require my-vendor/my-package
```

### Yank a version

```sh
curl -X DELETE \
  -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-composer/api/packages/my-vendor/my-package/versions/1.0.0"
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/composer -->
| Method | Path | Description |
|--------|------|-------------|
| `DELETE` | `/proxy/{registry}/api/packages/{vendor}/{package}/versions/{version}` | Yank a Composer package version (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/api/security-advisories/` | Proxy Composer security advisory queries to the upstream Packagist server. |
| `POST` | `/proxy/{registry}/api/upload` | Upload a Composer package ZIP (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/dist/{vendor}/{package}/{version}` | Download a Composer package ZIP artifact. |
| `GET` | `/proxy/{registry}/list.json` | `composer` bulk package enumeration — `list.json`. |
| `GET` | `/proxy/{registry}/p2/{path}` | Packagist v2 package metadata (all versions). |
| `GET` | `/proxy/{registry}/packages.json` | Composer registry root index. |
| `GET` | `/proxy/{registry}/search.json` | `composer search`. |
<!-- END endpoints -->

---

## Blocked versions

p2 metadata drops the blocked version, and `dist.url` is repointed at BatleHub
so downloads go through the proxy rather than straight to the upstream CDN.

Packagist serves `"minified": "composer/2.0"`, in which each entry omits every
key identical to the previous one. Deleting a middle entry from such a list
silently changes what the entries *after* it inherit — a well-formed document
describing the wrong packages. BatleHub expands the list, removes the version,
and re-minifies, so an entry after the removed one still means what it meant.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Store HTTP Basic credentials in `auth.json` (project root or `~/.config/composer/` — never commit it):

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "user",
      "password": "<your-token>"
    }
  }
}
```

When `auth.json` is present, no `Authorization` header is needed in `composer.json`.

## Notes

- `composer audit` works automatically — BatleHub proxies the Packagist security advisory API (`/api/security-advisories/`) transparently. See [Using BatleHub → security auditing](/use/#security-audit).
- Yanked versions are hidden from version listings and return `404` on download.

## Search

``composer search`` is answered in three steps: a cached result for that query, then the
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

## A plain-HTTP instance needs an explicit opt-in

Composer refuses an `http:` repository by default. If BatleHub is not behind TLS,
the project needs:

```json
{ "config": { "secure-http": false } }
```

Measured against Composer 2.10.2. Without it Composer stops before making any
request, pointing at <https://getcomposer.org/doc/06-config.md#secure-http>.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
