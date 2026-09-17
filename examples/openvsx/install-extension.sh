#!/usr/bin/env bash
# Install a VS Code / VSCodium extension from the batlehub Open VSX proxy.
#
# Proxy VSIX download URL pattern:
#   http://localhost:8080/proxy/{registry}/{publisher}.{name}/{version}/vsix

set -euo pipefail

PROXY="${PROXY_URL:-http://localhost:8080/proxy/my-openvsx}"
TOKEN="${PROXY_TOKEN:-change-me-user-token}"

install_extension() {
  local id="$1" version="$2"          # e.g. "rust-lang.rust-analyzer" "0.4.3052"
  local publisher="${id%%.*}"
  local name="${id#*.}"
  local vsix="${publisher}.${name}-${version}.vsix"
  local url="${PROXY}/${publisher}.${name}/${version}/vsix"

  echo "Fetching ${url}"
  curl -fsSL -H "Authorization: Bearer ${TOKEN}" -o "/tmp/${vsix}" "${url}"
  code --install-extension "/tmp/${vsix}" --force
  rm "/tmp/${vsix}"
}

# Install extensions listed in .vscode/extensions.json through the proxy.
install_extension "rust-lang.rust-analyzer"    "0.4.3052"
install_extension "tamasfe.even-better-toml"   "0.21.2"
install_extension "EditorConfig.EditorConfig"  "0.18.2"
