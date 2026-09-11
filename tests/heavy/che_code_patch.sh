#!/usr/bin/env bash
# The che-code credential patch in **che-code** — the build Eclipse Che runs
# (`quay.io/che-incubator/che-code`), driven through its CLI. The scenarios
# are tests/heavy/editor_patch_scenarios.sh; this file only says which editor
# and how to start it. Its sibling, vscode_patch.sh, runs the same scenarios
# on the stock VS Code server build.
#
# Where the build comes from, in order:
#   CHE_CODE_DIR      a `checode-linux-libc/ubi9` tree (node, out/, product.json)
#   /checode/…        the editor of the Che workspace this runs in, copied once
#                     into HEAVY_CACHE — the live tree is never written to
#   the image         `docker cp` out of quay.io/che-incubator/che-code:$CHE_CODE_TAG
#                     (CI; needs docker)
#
# che-code names its registry through `OPENVSX_REGISTRY_URL` — the launcher
# rewrites `product.json` from it at start-up — and that is the variable the
# patch reads on this build, so the scenarios name the gallery that way.
# The tree's own `node` needs its `ld_libs` (libnode.so); `bin/che-code`
# does not set them.
#
# Ports: 8135 (server), 8136 (tap). Environment knobs: DATABASE_URL
# (required), CHE_CODE_DIR, CHE_CODE_TAG (latest), HEAVY_PORT, HEAVY_TAP_PORT,
# WEEBO_VERSION (0.5.0), COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init che_code_patch 8135 8136
heavy_need python3 "python3"
heavy_need curl "curl"

CHE_CODE_TAG="${CHE_CODE_TAG:-latest}"
POD_TREE="/checode/checode-linux-libc/ubi9"

che_code_version() { python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$1/product.json"; }

if [[ -n "${CHE_CODE_DIR:-}" ]]; then
  CHE_DIR="$CHE_CODE_DIR"
elif [[ -x "$POD_TREE/node" && -f "$POD_TREE/product.json" ]]; then
  CHE_DIR="$HEAVY_CACHE/che-code-$(che_code_version "$POD_TREE")"
  if [[ ! -f "$CHE_DIR/product.json" ]]; then
    heavy_log "Copying the workspace's che-code ($(che_code_version "$POD_TREE")) into $CHE_DIR"
    rm -rf "$CHE_DIR.partial"; cp -r "$POD_TREE" "$CHE_DIR.partial" && mv "$CHE_DIR.partial" "$CHE_DIR"
  fi
else
  heavy_need docker "docker (to copy the build out of quay.io/che-incubator/che-code)"
  CHE_DIR="$HEAVY_CACHE/che-code-image-$CHE_CODE_TAG"
  if [[ ! -f "$CHE_DIR/product.json" ]]; then
    heavy_log "Copying che-code out of quay.io/che-incubator/che-code:$CHE_CODE_TAG"
    docker rm -f "batlehub-che-code-src" >/dev/null 2>&1 || true
    docker create --name "batlehub-che-code-src" "quay.io/che-incubator/che-code:$CHE_CODE_TAG" >/dev/null
    rm -rf "$CHE_DIR.partial"
    docker cp "batlehub-che-code-src:/checode/checode-linux-libc/ubi9" "$CHE_DIR.partial" && mv "$CHE_DIR.partial" "$CHE_DIR"
    docker rm -f "batlehub-che-code-src" >/dev/null
  fi
fi
[[ -x "$CHE_DIR/node" && -f "$CHE_DIR/out/server-main.js" && -f "$CHE_DIR/product.json" ]] \
  || heavy_fail "$CHE_DIR is not a che-code build (node, out/server-main.js, product.json expected)"
CHE_LIBS="$CHE_DIR/ld_libs/openssl:$CHE_DIR/ld_libs/core"

EDITOR_LABEL="che-code $(che_code_version "$CHE_DIR")"
EDITOR_DIR="$CHE_DIR"
GALLERY_ENV_NAME="OPENVSX_REGISTRY_URL"
editor_node() { LD_LIBRARY_PATH="$CHE_LIBS" "$CHE_DIR/node" "$@"; return $?; }
editor_cli() {  # <profile> <args…>
  local profile="$1"; shift
  LD_LIBRARY_PATH="$CHE_LIBS" "$CHE_DIR/node" "$CHE_DIR/out/server-main.js" \
    --server-data-dir "$profile/server" --user-data-dir "$profile/user" \
    --extensions-dir "$profile/extensions" "$@"
  return $?
}

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/editor_patch_scenarios.sh"
heavy_done CHE-CODE-PATCH-HEAVY-OK
