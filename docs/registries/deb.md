# Debian / APT

Proxy a Debian/Ubuntu APT repository and, in `local`/`hybrid` mode, host your own: publish `.deb` packages and BatleHub regenerates the `Packages`/`Release` indexes, signing them with an Ed25519 OpenPGP key when `[registries.repo_signing]` is configured.

## At a glance

| | |
|---|---|
| **Config type** | `deb` |
| **Default upstream** | none — set `upstreams` explicitly for proxy/hybrid |
| **Modes** | proxy · local · hybrid |
| **Addressing** | path-addressed |
| **Private publish** | ✅ `curl -X PUT … /deb/pool/{suite}/{component}/upload` |
| **Air gap** | no composed index offline: a signed `Packages` file cannot be re-signed here; a held file is served by path |

## Proxy setup

Add a source line under `/etc/apt/sources.list.d/`. Replace `<registry>` with your configured registry name; the suite (`stable`) and component (`main`) must match the upstream (or your locally published) layout:

```bash
REG="https://batlehub.example.com/proxy/<registry>/deb"

# Import the signing key (local/hybrid signed repos only)
curl -fsSL $REG/key.gpg | sudo tee /usr/share/keyrings/<registry>.asc >/dev/null

# Add the source
echo "deb [signed-by=/usr/share/keyrings/<registry>.asc] $REG stable main" \
  | sudo tee /etc/apt/sources.list.d/<registry>.list

sudo apt update && sudo apt install hello
```

For an unsigned **local** repository (no `[registries.repo_signing]` key), replace `[signed-by=…]` with `[trusted=yes]`.

::: warning `trusted=yes` disables apt-secure
`trusted=yes` tells apt to accept the repository with **no signature verification at all** — anything that can answer for the host, or sit on the path, can serve arbitrary packages that install as root. Restrict it to an isolated, fully trusted channel (an internal network you control end to end). Prefer configuring `[registries.repo_signing]` so BatleHub signs the indexes and consumers verify with `signed-by`.
:::

**Proxy mode** has no BatleHub key — `…/deb/key.gpg` is served only for `local`/`hybrid` registries with a `repo_signing` key. In proxy mode BatleHub relays the **upstream** repo's `InRelease`/`Release.gpg` and its signature, so apt verifies against the **upstream's** archive key. Official Debian/Ubuntu mirrors already ship it (packages `debian-archive-keyring` / `ubuntu-keyring`):

```bash
echo "deb [signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] \
  https://batlehub.example.com/proxy/<registry>/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/<registry>.list
```

## Publishing (local / hybrid)

Upload a `.deb` with a `PUT`. The distribution and component come from the upload path; BatleHub derives the pool location, regenerates the suite indexes, and re-signs `InRelease`/`Release.gpg`:

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello_1.0_amd64.deb \
  https://batlehub.example.com/proxy/<registry>/deb/pool/stable/main/upload
```

Signing requires an Ed25519 OpenPGP key configured under `[registries.repo_signing]`; without it, the generated indexes are served unsigned (consume with `[trusted=yes]`).

## Authentication

APT reads credentials from `/etc/apt/auth.conf.d/` (Debian 9+ / Ubuntu 19.04+). The `sources.list` entry stays unchanged — credentials live in a separate file that is not visible to `apt-cache policy`:

```bash
sudo tee /etc/apt/auth.conf.d/batlehub.conf > /dev/null <<'EOF'
machine batlehub.example.com
login <your-username>
password <your-token>
EOF
sudo chmod 0600 /etc/apt/auth.conf.d/batlehub.conf
```

On older systems, use `/etc/apt/auth.conf` with the same `machine / login / password` stanza. Alternatively, embed the credentials directly in the URL (less secure — the token appears in `apt-cache policy` output): `https://<user>:<token>@batlehub.example.com/proxy/<registry>/deb …`.

## Notes

- A `NO_PUBKEY` / "the following signatures couldn't be verified" error in proxy mode means the upstream's key isn't in the keyring named by `signed-by` — install `debian-archive-keyring` (Debian) or `ubuntu-keyring` (Ubuntu), or import the upstream's key into a keyring and point `signed-by` at it. Authenticate against the upstream keyring rather than reaching for `[trusted=yes]`: in proxy mode BatleHub relays the upstream signature, so verification is available and turning it off buys nothing.
- Publishing requires the registry in `local` or `hybrid` mode — ask your administrator.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
