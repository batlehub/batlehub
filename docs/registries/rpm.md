# RPM / YUM (DNF)

Proxy a YUM/DNF repository and, in `local`/`hybrid` mode, host your own: publish `.rpm` packages and BatleHub regenerates `repodata/`, signing `repomd.xml.asc` with an Ed25519 OpenPGP key when `[registries.repo_signing]` is configured.

## At a glance

| | |
|---|---|
| **Config type** | `rpm` |
| **Default upstream** | none — set `upstreams` explicitly for proxy/hybrid |
| **Modes** | proxy · local · hybrid |
| **Addressing** | path-addressed |
| **Private publish** | ✅ `curl -X PUT … /rpm/upload` |
| **Air gap** | no composed index offline: a signed `repomd.xml` cannot be re-signed here; a held file is served by path |

## Proxy setup

Add a `.repo` file under `/etc/yum.repos.d/`. Replace `<registry>` with your configured registry name:

```ini
# /etc/yum.repos.d/<registry>.repo
[<registry>]
name=<registry>
baseurl=https://batlehub.example.com/proxy/<registry>/rpm
enabled=1
repo_gpgcheck=1
gpgcheck=0
gpgkey=https://batlehub.example.com/proxy/<registry>/rpm/repodata/repomd.xml.key
```

```bash
sudo dnf makecache && sudo dnf install hello
```

For an unsigned **local** repo (no `[registries.repo_signing]` key), set `repo_gpgcheck=0` and omit `gpgkey`.

**Proxy mode** has no BatleHub key — `repodata/repomd.xml.key` is served only for `local`/`hybrid` registries with a `repo_signing` key. In proxy mode BatleHub relays the **upstream** `repodata` (including any `repomd.xml.asc`), so either point `gpgkey` at the **upstream project's** key with `repo_gpgcheck=1`, or set `repo_gpgcheck=0` if you trust the channel.

## Publishing (local / hybrid)

Upload an `.rpm` with a `PUT`; BatleHub regenerates `repodata/` and re-signs `repomd.xml.asc` when a signing key is configured:

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello-1.0-1.x86_64.rpm \
  https://batlehub.example.com/proxy/<registry>/rpm/upload
```

Signing requires an Ed25519 OpenPGP key under `[registries.repo_signing]`. Consumers verify it against `…/rpm/repodata/repomd.xml.key` with `repo_gpgcheck=1`.

## Authentication

DNF/YUM reads `username` and `password` directly from the `.repo` file:

```ini
[<registry>]
name=<registry>
baseurl=https://batlehub.example.com/proxy/<registry>/rpm
enabled=1
repo_gpgcheck=0
gpgcheck=0
username=<your-username>
password=<your-token>
```

Alternatively, use `~/.netrc` (DNF and libcurl honour it for HTTP Basic Auth):

```text
machine batlehub.example.com
login <your-username>
password <your-token>
```

## Notes

- `repo_gpgcheck` controls verification of the **repository metadata** signature (`repomd.xml.asc`); `gpgcheck` controls per-package RPM signatures — BatleHub signs the metadata, not the individual packages, so `gpgcheck=0` is expected.
- Publishing requires the registry in `local` or `hybrid` mode — ask your administrator.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
