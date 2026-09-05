# VS Code Marketplace

Proxy and cache VS Code extension VSIX downloads from Microsoft's [Visual Studio Marketplace](https://marketplace.visualstudio.com) via its Gallery API. Use it for extensions only on the Microsoft marketplace and not mirrored on open-vsx.org; it can also host private extensions.

## At a glance

| | |
|---|---|
| **Config type** | `vscode-marketplace` |
| **Default upstream** | `marketplace.visualstudio.com` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ VSIX upload (`PUT …/vsix`) |
| **Air gap** | no composed listing offline: a gallery answers by query |

## Proxy setup

### Use BatleHub as your extension gallery

Same protocol, same routes as the [OpenVSX page](/registries/openvsx#use-batlehub-as-your-extension-gallery) — the gallery is the *client* side, so it does not depend on which upstream sits behind the registry:

```jsonc
{
  "extensionsGallery": {
    "serviceUrl": "https://batlehub.example.com/proxy/<registry>/vscode/gallery",
    "itemUrl": "https://batlehub.example.com/proxy/<registry>/vscode/item",
    "resourceUrlTemplate": "https://batlehub.example.com/proxy/<registry>/vscode/unpkg/{publisher}/{name}/{version}/{path}"
  }
}
```

The editor sends no credentials to its gallery, so this registry needs `anonymous = ["releases:read", "source:read"]` under `[registries.rbac]`, or an authenticating ingress. See the warning on the [OpenVSX page](/registries/openvsx#use-batlehub-as-your-extension-gallery).

### Download a VSIX directly

Download a VSIX directly by coordinate and install it. Replace `<registry>` with your configured registry name; use `latest` as the version to fetch the newest release:

```sh
# Pinned version
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/ms-python.python/2024.2.1/vsix" \
  -o ms-python.python-2024.2.1.vsix

# Or the latest version
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/ms-python.python/latest/vsix" \
  -o ms-python.python.vsix

code --install-extension ms-python.python-2024.2.1.vsix
```

## Publishing (local / hybrid)

The registry must be in `local` or `hybrid` mode. A `vscode-marketplace` registry shares the VSIX upload/download endpoint with [OpenVSX](/registries/openvsx); point the `PUT …/{publisher}.{name}/{version}/vsix` upload at your `vscode-marketplace` registry name:

```sh
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-org.my-extension-1.0.0.vsix \
  "https://batlehub.example.com/proxy/<registry>/my-org.my-extension/1.0.0/vsix"
```

## Authentication

Pass a BatleHub token as a Bearer header on the VSIX request. Anonymous access works only when the registry's RBAC grants the `anonymous` role read access.

## Notes

- The direct VSIX route is `…/proxy/<registry>/{publisher}.{name}/{version}/vsix`, identical to OpenVSX — the two types share the same handler.
- Prefer [OpenVSX](/registries/openvsx) for extensions that are mirrored on open-vsx.org; use this type only for Microsoft-marketplace-exclusive extensions.

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
