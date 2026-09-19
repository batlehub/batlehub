#!/usr/bin/env bash
# Install a VS Code extension from the batlehub VS Code Marketplace proxy.
#
# Proxy VSIX download URL pattern:
#   http://localhost:8080/proxy/{registry}/{publisher}.{name}/{version}/vsix
#
# This uses the official VS Code Marketplace adapter (different from Open VSX).

set -euo pipefail

PROXY="${PROXY_URL:-http://localhost:8080/proxy/my-vscode-marketplace}"
TOKEN="${PROXY_TOKEN:-change-me-user-token}"

install_extension() {
  local id="$1" version="$2"
  local publisher="${id%%.*}"
  local name="${id#*.}"
  local vsix="${publisher}.${name}-${version}.vsix"
  local url="${PROXY}/${publisher}.${name}/${version}/vsix"

  echo "Fetching ${url}"
  curl -fsSL -H "Authorization: Bearer ${TOKEN}" -o "/tmp/${vsix}" "${url}"
  code --install-extension "/tmp/${vsix}" --force
  rm "/tmp/${vsix}"
}

install_extension "ms-python.python"        "2026.7.2026082601"
install_extension "charliermarsh.ruff"      "2026.82.0"
install_extension "ms-toolsai.jupyter"      "2026.6.2026071501"
