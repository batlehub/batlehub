# PyPI

Proxy and cache PyPI through BatleHub for pip, uv, Poetry, and other Python package managers, or host private packages. BatleHub serves the [PEP 503](https://peps.python.org/pep-0503/) Simple index and caches wheels and source distributions after the first download.

## At a glance

| | |
|---|---|
| **Config type** | `pypi` |
| **Default upstream** | `pypi.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `twine upload` |
| **Air gap** | offline, the simple page (JSON and HTML) is composed from the held files; `pip install` resolves against it |

## Proxy setup

Point pip at the Simple index (`~/.pip/pip.conf` on Linux/macOS, `%APPDATA%\pip\pip.ini` on Windows):

```ini
[global]
index-url = https://batlehub.example.com/proxy/<registry>/simple/
```

For **uv**, add the index to `pyproject.toml`:

```toml
[[tool.uv.index]]
name = "batlehub"
url = "https://batlehub.example.com/proxy/<registry>/simple/"
default = true
```

All three clients (pip, uv, Poetry) read the same `simple/` index. BatleHub rewrites the file download links inside the index so wheels and sdists are fetched — and cached — through the proxy rather than directly from `files.pythonhosted.org`.

## Publishing (local / hybrid)

The registry must be in `local` or `hybrid` mode. Build, then upload with `twine` against the `legacy/` (upload) endpoint — the filename, name, and version are derived from the wheel or sdist metadata automatically:

```bash
python -m build

twine upload \
  --repository-url https://batlehub.example.com/proxy/<registry>/legacy/ \
  --username __token__ \
  --password $BATLEHUB_TOKEN \
  dist/*
```

Or configure `~/.pypirc` with a `repository = https://batlehub.example.com/proxy/<registry>/legacy/` entry and run `twine upload --repository batlehub dist/*`.

## Blocked versions

A blocked version disappears from the simple index in **both** of its
representations — PEP 503 HTML and PEP 691 JSON — and every file of that
version goes with it, the wheel and the sdist alike. PEP 700's `versions`
summary is filtered alongside the file list so the two cannot disagree.

The index lists files rather than versions, so the version is recovered from
each distribution filename. A filename BatleHub does not recognise is **kept**:
over-listing one file is the safe direction, where a mis-parse that dropped it
would hide a package's whole file set. Versions are compared under PEP 440, so a
block recorded as `1.0` hides a wheel listed as `1.0.0`.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Twine sends the token as the password with the literal username `__token__`. For installs, pip/uv/Poetry read credentials from `~/.netrc` automatically:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Keep the token out of the index URL: a token embedded in `index-url` ends up in `pip.conf`, in build logs, and in `pip --verbose` / `pip config list` output. Use `~/.netrc` (above) or a [pip credential helper](https://pip.pypa.io/en/stable/topics/authentication/) instead.

## Notes

After publishing, the package appears in the Simple index immediately:

```bash
curl -s "https://batlehub.example.com/proxy/<registry>/simple/my-package/" \
  -H "Authorization: Bearer $BATLEHUB_TOKEN"
```

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
