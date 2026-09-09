# Pacman (Arch Linux)

Proxy an Arch Linux mirror and, in `local`/`hybrid` mode, host your own: publish `.pkg.tar.{zst,xz,gz}` packages and BatleHub regenerates the per-architecture `<repo>.db` / `<repo>.files` databases, signing them with an Ed25519 OpenPGP key when `[registries.repo_signing]` is configured.

## At a glance

| | |
|---|---|
| **Config type** | `pacman` |
| **Default upstream** | none — set `upstreams` explicitly for proxy/hybrid |
| **Modes** | proxy · local · hybrid |
| **Addressing** | path-addressed |
| **Private publish** | ✅ `curl -X PUT … /pacman/upload` |
| **Air gap** | no composed index offline: a signed database cannot be re-signed here; a held file is served by path |

## Proxy setup

Add a repository stanza to `/etc/pacman.conf`. The section name must match the database name, which is the registry name; `$arch` is substituted by pacman:

```ini
# /etc/pacman.conf
[<registry>]
SigLevel = Required
Server = https://batlehub.example.com/proxy/<registry>/pacman/$arch
```

Import the signing key (local/hybrid signed repos only):

```bash
curl -fsSL https://batlehub.example.com/proxy/<registry>/pacman/key.gpg \
  | sudo pacman-key --add -
sudo pacman-key --lsign-key <key-id>
```

Then:

```bash
sudo pacman -Sy
sudo pacman -S hello
```

The database is served as `$arch/<registry>.db`. For an unsigned **local** repository (no `[registries.repo_signing]` key), set `SigLevel = Optional TrustAll` (or `Never`) and skip the key import.

**Proxy mode** has no BatleHub key — `pacman/key.gpg` is served only for `local`/`hybrid` registries with a `repo_signing` key. In proxy mode the packages are signed (or not) by the **upstream** mirror, so set `SigLevel` to match the upstream's signing.

## Publishing (local / hybrid)

Upload a package with a `PUT`. The name, version, and architecture are read from the embedded `.PKGINFO`; BatleHub stores the file under `{arch}/` and regenerates the `<repo>.db` database (re-signing it when a signing key is configured):

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello-1.0-1-x86_64.pkg.tar.zst \
  https://batlehub.example.com/proxy/<registry>/pacman/upload
```

Signing requires an Ed25519 OpenPGP key under `[registries.repo_signing]`; consumers import it from `…/pacman/key.gpg` with `pacman-key --add` and locally sign it (`pacman-key --lsign-key`).

## Authentication

Pacman has no dedicated credentials file — embed the token as HTTP Basic credentials in the `Server` URL:

```ini
Server = https://<user>:<token>@batlehub.example.com/proxy/<registry>/pacman/$arch
```

## Notes

- The `[<section>]` name in `pacman.conf` **must** equal the registry name, because the database is served as `$arch/<registry>.db` and pacman derives the DB filename from the section name.
- Publishing requires the registry in `local` or `hybrid` mode — ask your administrator.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
