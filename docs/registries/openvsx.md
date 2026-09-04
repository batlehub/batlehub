# OpenVSX

Proxy and cache VS Code extensions from [open-vsx.org](https://open-vsx.org), or host private ones. BatleHub serves the **VS Code gallery protocol** and the **OpenVSX REST API**, so an editor can be pointed at it as its extension marketplace and `ovsx` can query it. Extension IDs follow the `{publisher}.{name}` convention.

## At a glance

| | |
|---|---|
| **Config type** | `openvsx` |
| **Default upstream** | `open-vsx.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ VSIX upload (`PUT …/vsix`) |

## Proxy setup

### Use BatleHub as your extension gallery

Point the editor at this registry by adding to `product.json` (VSCodium and
Code - OSS read `~/.config/VSCodium/product.json`; for VS Code itself, edit the
`product.json` inside the installation):

```jsonc
{
  "extensionsGallery": {
    "serviceUrl": "https://batlehub.example.com/proxy/<registry>/vscode/gallery",
    "itemUrl": "https://batlehub.example.com/proxy/<registry>/vscode/item",
    "resourceUrlTemplate": "https://batlehub.example.com/proxy/<registry>/vscode/unpkg/{publisher}/{name}/{version}/{path}"
  }
}
```

Restart the editor; search, install and update then go through BatleHub, and
every VSIX it fetches is cached, audited and subject to the registry's policy
rules.

::: warning The editor cannot authenticate
VS Code sends **no `Authorization` header** to its gallery, and `product.json`
has nowhere to put a token. A registry used as a gallery therefore needs

```toml
[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

or an ingress that authenticates in front of BatleHub. A gallery registry that
requires a bearer token answers every query with an empty list, and the editor
reports that no extensions were found — which looks like a broken proxy rather
than a configuration choice.

**Unless you build the editor yourself.** A build you compile can carry a
small patch that reads a credential and attaches it, and this repository ships
the module and the integration steps at
[`patches/che-code/`](https://batleforc.git.batleforc.fr/batlehub/tree/main/patches/che-code).
It reads the same file `batlehub-cli auth write-token-file` writes; see
[the credential contract file](#the-credential-contract-file) below.
:::

### Use it with `ovsx`

The OpenVSX REST API is served too, so the CLI works against this registry:

```sh
export OVSX_REGISTRY_URL="https://batlehub.example.com/proxy/<registry>"
ovsx get acme.tool
```

### Download a VSIX directly

Download a VSIX directly by coordinate and install it. Replace `<registry>` with your configured registry name:

```sh
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/ms-python.python/2024.2.1/vsix" \
  -o ms-python.python-2024.2.1.vsix

code --install-extension ms-python.python-2024.2.1.vsix
```

## Publishing (local / hybrid)

Both registry types (`openvsx` and `vscode-marketplace`) use the same upload endpoint. There is no dedicated CLI tool — extensions are published with a plain `PUT` request carrying the raw VSIX bytes.

### Server configuration

```toml
[[registries]]
type = "openvsx"        # or "vscode-marketplace"
name = "internal-ext"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

### Extension ID convention

Extension IDs follow the `{publisher}.{name}` format used by the VS Code Marketplace, e.g. `my-org.my-extension`.

### Upload

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-org.my-extension-1.0.0.vsix \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-extension/1.0.0/vsix"
```

The server reads the publisher and extension name from the URL path. The `{extension_id}` segment is the full `{publisher}.{name}` identifier.

A package built with `vsce package --pre-release` (or published with `ovsx publish --pre-release`) keeps its pre-release marker: BatleHub reads it from the VSIX's own `extension.vsixmanifest`, reports it to the gallery as `Microsoft.VisualStudio.Code.PreRelease` and to the OpenVSX API as `preRelease`, and an editor then offers that version only to users who opted into pre-releases.

### Download / install

```sh
# Download the VSIX
curl -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-extension/1.0.0/vsix" \
  -o my-org.my-extension-1.0.0.vsix

# Install into VS Code
code --install-extension my-org.my-extension-1.0.0.vsix
```

### Verify

```sh
# Confirm the ZIP magic bytes (PK\x03\x04) to validate the upload was accepted
curl -s -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-extension/1.0.0/vsix" \
  | xxd | head -1
# Should show: 50 4b 03 04 ...
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/openvsx -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{extension_id}/{version}/vsix` | Download a VS Code extension VSIX package. |
| `PUT` | `/proxy/{registry}/{extension_id}/{version}/vsix` | Upload a VS Code extension VSIX package. |
| `POST` | `/proxy/{registry}/api/-/namespace/create` | Claim an OpenVSX publisher namespace. |
| `POST` | `/proxy/{registry}/api/-/publish` | `ovsx publish` — `POST /api/-/publish`. |
| `GET` | `/proxy/{registry}/api/-/search` | Search the registry — `GET …/api/-/search`. |
| `GET` | `/proxy/{registry}/api/{namespace}` | `GET /api/{namespace}` — what a publisher has here. |
| `GET` | `/proxy/{registry}/api/{namespace}/{extension}` | The newest version of one extension — `GET …/api/{namespace}/{extension}`. |
| `GET` | `/proxy/{registry}/api/{namespace}/{extension}/{version}` | One specific version — `GET …/api/{namespace}/{extension}/{version}`. |
| `GET` | `/proxy/{registry}/api/{namespace}/{extension}/{version}/file/{filename}` | One file out of an extension — `GET …/api/{ns}/{ext}/{version}/file/{name}`. |
| `GET` | `/proxy/{registry}/api/version` | `GET /api/version` — the registry's own version document. |
| `GET` | `/proxy/{registry}/vscode/asset/{publisher}/{name}/{version}/{asset_type}` | `GET …/vscode/asset/{publisher}/{name}/{version}/{asset_type}` |
| `POST` | `/proxy/{registry}/vscode/gallery/extensionquery` | Query the extension gallery. |
| `GET` | `/proxy/{registry}/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage` | `GET …/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage` |
| `GET` | `/proxy/{registry}/vscode/item` | `GET …/vscode/item?itemName=publisher.name` |
| `GET` | `/proxy/{registry}/vscode/unpkg/{publisher}/{name}/{version}/{path}` | `GET …/vscode/unpkg/{publisher}/{name}/{version}/{path}` |
<!-- END endpoints -->

---

## Authentication

Pass a BatleHub token as a Bearer header on the VSIX request. Anonymous access works only when the registry's RBAC grants the `anonymous` role read access.

### The credential contract file

The editor is not the only thing that has to find a credential, and none of
the things that do can share the CLI's config: a program started by a desktop
session, a workspace template or a terminal inherits neither an environment
variable nor a login. So there is one file, written by the CLI and read by
everything else ([RFC 0011](/rfc/0011-openvsx-login) §4.1):

```sh
batlehub-cli --server https://hub.example.dev auth write-token-file
batlehub-cli auth status
```

```
REGISTRY                  KIND        TOKEN SOURCE               STATE  EXPIRES  REFRESH
https://hub.example.dev   oidc        inline (written by cli)    ok     4m12s    cli (batlehub-cli)
https://hub.k8s.dev       kubernetes  file /var/run/…/token      ok     —        reresolve
```

`$BATLEHUB_HOME/state/vsx-token.json`, `0600`, `$HOME/.batlehub` by default.
It is keyed by origin, so one laptop pointed at three BatleHubs keeps three
credentials in one file and a login to one is not a logout from the others.
The normative shape is the JSON Schema shipped beside the CLI at
`cli/schema/vsx-token.schema.json`, not this page.

Two properties are worth knowing before you write a consumer:

- **A credential need not be *in* it.** `{"from": "file", "path": "/var/run/…"}`
  records where to read one, which is what you want for a projected
  Kubernetes token something else keeps fresh. Pass `--from-file` to
  `write-token-file` to record one.
- **Nothing here fails loudly.** A missing file, an unreadable one, a source
  a consumer does not implement — all of them mean "no credential", and the
  editor then behaves exactly as it does against an anonymous gallery. That
  is deliberate, and it is why `auth status` exists: it resolves every entry
  at the moment you ask and names the reason when one does not, because
  *nothing was configured* and *the file went away* look identical from the
  editor and want opposite fixes.

`batlehub-cli auth token` prints a credential for scripts and brokers,
refreshing it first when it is close to expiry. It is the only command whose
job is to emit one; `auth status` renders a summary that has no field able to
hold a secret.

## Notes

- The direct VSIX route is `…/proxy/<registry>/{publisher}.{name}/{version}/vsix`.
- Gallery endpoints: `POST …/vscode/gallery/extensionquery`, `GET …/vscode/asset/{publisher}/{name}/{version}/{assetType}`, `GET …/vscode/unpkg/{publisher}/{name}/{version}/{path}`, `GET …/vscode/item`, and `GET …/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage`.
- OpenVSX API endpoints: `GET …/api/{namespace}/{extension}[/{version}]`, `GET …/api/-/search`, `GET …/api/{namespace}/{extension}/{version}/file/{filename}`.
- The manifest, README, changelog, licence and icon are served **out of the cached VSIX**, so one artifact answers every asset request and a private extension behaves exactly like a proxied one. An extension that ships no changelog returns `404` for that asset, which the editor renders as an empty tab.
- An extension's icon is never served as `image/svg+xml`. An SVG served with that type executes script in this origin, which is the same origin the admin console keeps its token in; SVG icons come back as an opaque download and the editor shows no icon.
- For extensions published only to Microsoft's marketplace and not mirrored on open-vsx.org, use the [VS Code Marketplace](/registries/vscode-marketplace) type instead.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
