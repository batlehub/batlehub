#!/usr/bin/env bash
# Example: install a tool from GitHub releases using the batlehub proxy.
# Usage:  PROXY_TOKEN=xxx ./install.sh

set -euo pipefail

PROXY="${PROXY_URL:-http://localhost:8080/proxy/my-github}"
TOKEN="${PROXY_TOKEN:-change-me-user-token}"
OWNER="cli"
REPO="cli"
VERSION="${GH_VERSION:-v2.101.0}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')"
FILE="gh_${VERSION#v}_${OS}_${ARCH}.tar.gz"

curl -fsSL \
  -H "Authorization: Bearer ${TOKEN}" \
  "${PROXY}/${OWNER}/${REPO}/releases/download/${VERSION}/${FILE}" \
  | tar -xz --strip-components=1 -C /usr/local/bin gh_*/bin/gh

echo "gh ${VERSION} installed."
