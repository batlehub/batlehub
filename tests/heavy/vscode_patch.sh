#!/usr/bin/env bash
# The che-code credential patch in **stock VS Code** — the server build
# Microsoft publishes (`server-linux-x64-web`), driven through its CLI. The
# scenarios are tests/heavy/editor_patch_scenarios.sh; this file only says
# which editor. Its sibling, che_code_patch.sh, runs the same scenarios on
# the che-code build.
#
# Ports: 8126 (server), 8134 (tap). Needs network once for the VS Code
# download (HEAVY_CACHE, shared with vsx_login.sh / vsx_view.sh /
# marketplace.sh) and the fixture. Environment knobs: DATABASE_URL
# (required), HEAVY_PORT, HEAVY_TAP_PORT, VSCODE_VERSION (1.136.2),
# WEEBO_VERSION (0.5.0), COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init vscode_patch 8126 8134
heavy_need python3 "python3"
heavy_need curl "curl"

VSCODE_VERSION="${VSCODE_VERSION:-1.136.2}"
VSCODE_DIR="$HEAVY_CACHE/vscode-server-web-$VSCODE_VERSION"
CODE_SERVER="$VSCODE_DIR/bin/code-server"
if [[ ! -x "$CODE_SERVER" ]]; then
  heavy_log "Downloading VS Code $VSCODE_VERSION (server-linux-x64-web) into $VSCODE_DIR"
  mkdir -p "$VSCODE_DIR"
  curl -fsSL --proto '=https' --proto-redir '=https' \
    "https://update.code.visualstudio.com/$VSCODE_VERSION/server-linux-x64-web/stable" \
    | tar -xz -C "$VSCODE_DIR" --strip-components=1
fi
[[ -x "$CODE_SERVER" ]] || heavy_fail "no bin/code-server in the VS Code build at $VSCODE_DIR"

EDITOR_LABEL="VS Code $VSCODE_VERSION"
EDITOR_DIR="$VSCODE_DIR"
# The stock build has no launcher naming a registry; the patch's own variable.
GALLERY_ENV_NAME="VSX_REGISTRY_URL"
editor_node() { "$VSCODE_DIR/node" "$@"; return $?; }
editor_cli() {  # <profile> <args…>
  local profile="$1"; shift
  "$CODE_SERVER" --server-data-dir "$profile/server" --user-data-dir "$profile/user" \
    --extensions-dir "$profile/extensions" "$@"
  return $?
}

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/editor_patch_scenarios.sh"
heavy_done VSCODE-PATCH-HEAVY-OK
