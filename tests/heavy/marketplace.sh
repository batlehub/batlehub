#!/usr/bin/env bash
# Heavy marketplace integration tests.
#
# End-to-end proof that real IDE tooling can install an extension/plugin that
# does NOT exist on any public store — only on this BatleHub instance:
#
#   1. Starts a BatleHub server (Postgres required, `DATABASE_URL` env) with two
#      local-mode registries: `vscode` (vscode-marketplace) and `jbm`
#      (jetbrains-marketplace) — see config.heavy.toml.
#   2. Publishes the weebo-bridge-notify release artifacts
#      (https://github.com/batleforc/weebo-che-notify) to both registries.
#   3. VS Code: two scenarios, on the server build (`server-linux-x64-web`).
#      a. downloads the VSIX back through the proxy and installs it from the
#         file with a headless `code-server --install-extension`;
#      b. points the editor's `product.json` at this instance's VS Code
#         gallery and installs the same extension **by id**, with no file —
#         the only test that proves BatleHub can be an editor's marketplace
#         rather than just a byte cache.
#   4. JetBrains: runs IntelliJ's headless `installPlugins` command pointed at
#      this instance's `updatePlugins.xml`, then asserts the plugin landed in
#      the isolated plugins directory.
#
# Run via `task test:marketplace-heavy` or directly. With `COVERAGE=1` the
# server runs under `cargo llvm-cov run --no-report`, so its execution counts
# toward the merged CI coverage report (the caller runs `cargo llvm-cov report`
# afterwards; the server must exit cleanly for the profile data to flush —
# SIGTERM triggers actix's graceful shutdown).
#
# Environment knobs:
#   DATABASE_URL     (required)  Postgres for the server
#   BASE             (default http://127.0.0.1:8080)
#   WEEBO_VERSION    (default 0.5.0)   release of weebo-che-notify to install
#   VSCODE_VERSION   (default 1.136.2) VS Code server build to download
#   IDEA_VERSION     (default 2026.1.3) IntelliJ IDEA build (unified installer;
#                    since 2025.3 there is no separate Community edition — the
#                    tarball is idea-<version>.tar.gz, not ideaIC-<version>.tar.gz)
#   IDE_CACHE        (default ~/.cache/batlehub-heavy) cacheable IDE downloads
#   COVERAGE=1       instrument the server with cargo-llvm-cov
#   SKIP_VSCODE=1 / SKIP_JETBRAINS=1  skip one scenario
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

BASE="${BASE:-http://127.0.0.1:8080}"
ADMIN_TOKEN="${ADMIN_TOKEN:-heavy-admin-token}"
WEEBO_VERSION="${WEEBO_VERSION:-0.5.0}"
WEEBO_BASE_URL="${WEEBO_BASE_URL:-https://github.com/batleforc/weebo-che-notify/releases/download}"
VSCODE_EXT_ID="batleforc.weebo-bridge-notify"
JB_PLUGIN_ID="fr.batleforc.weebo-bridge-notify"
VSCODE_VERSION="${VSCODE_VERSION:-1.136.2}"
IDEA_VERSION="${IDEA_VERSION:-2026.1.3}"
IDE_CACHE="${IDE_CACHE:-$HOME/.cache/batlehub-heavy}"
COVERAGE="${COVERAGE:-0}"

# Every off-network download in this script either goes straight into `tar` or
# is published back through the server, so the transport is the only thing
# standing between an upstream redirect and code execution here. `--proto-redir`
# pins the *redirect chain* as well as the first request: every one of these
# URLs redirects to a CDN (GitHub releases to `objects.githubusercontent.com`,
# both IDE sites to their own), and `-L` alone would happily follow a downgrade
# to plain HTTP.
#
# A wrapper rather than an array of flags spliced into each call site: a call
# site here names `fetch_https`, so it cannot quietly omit half of the pair, and
# the flags stay lexically attached to the `curl` that uses them — which is also
# how SonarCloud's "not enforcing HTTPS" hotspot reads a script. Splicing in
# `"${HTTPS_ONLY[@]}"` hid them from it and raised three hotspots on code that
# was already doing the right thing (docs/internal/sonar-triage-2026-08-30.md).
fetch_https() {
  curl -fsSL --proto '=https' --proto-redir '=https' "$@"
}

: "${DATABASE_URL:?DATABASE_URL must point at a reachable Postgres}"

WORK="$(mktemp -d)"
SERVER_PID=""

stop_server() {
  if [[ -n "$SERVER_PID" ]]; then
    # SIGTERM the whole process group: $SERVER_PID is the `cargo llvm-cov` /
    # `cargo run` wrapper and the batlehub-server binary is a grandchild.
    # cargo does not forward signals, so signalling the wrapper alone strands
    # the server — no graceful shutdown, no llvm profile flush (the profraw
    # files would be missing at `cargo llvm-cov report` time). `setsid` at
    # launch made $SERVER_PID the process-group id.
    kill -TERM -- "-$SERVER_PID" 2>/dev/null || kill -TERM "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    # The server binary outlives the wrapper by a beat — wait for the whole
    # group to exit so the profile data is fully written before reporting.
    for _ in $(seq 1 60); do
      pgrep -g "$SERVER_PID" >/dev/null 2>&1 || break
      sleep 1
    done
    if pgrep -g "$SERVER_PID" >/dev/null 2>&1; then
      echo "WARNING: server process group $SERVER_PID still alive after 60s;" \
        "llvm coverage profiles may be incomplete" >&2
    fi
  fi
  SERVER_PID=""
}

cleanup() {
  stop_server
  rm -rf "$WORK"
}
trap cleanup EXIT

log() { printf '\n==> %s\n' "$*"; }

# ── 1. Start the server ───────────────────────────────────────────────────────

log "Starting BatleHub server (coverage=$COVERAGE)"
# setsid: own process group, so stop_server can SIGTERM cargo AND the server
# binary it spawns in one shot (see stop_server).
if [[ "$COVERAGE" == "1" ]]; then
  setsid cargo llvm-cov run --no-report -p batlehub-server -- \
    --config tests/heavy/config.heavy.toml >"$WORK/server.log" 2>&1 &
else
  setsid cargo run -p batlehub-server -- \
    --config tests/heavy/config.heavy.toml >"$WORK/server.log" 2>&1 &
fi
SERVER_PID=$!

for i in $(seq 1 180); do
  if curl -sf "$BASE/healthz" >/dev/null 2>&1; then break; fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: server exited early; log follows" >&2
    cat "$WORK/server.log" >&2
    exit 1
  fi
  sleep 2
  if [[ "$i" == 180 ]]; then
    echo "ERROR: server did not become healthy; log follows" >&2
    cat "$WORK/server.log" >&2
    exit 1
  fi
done
log "Server healthy at $BASE"

# ── 2. Fetch the release artifacts and publish them ──────────────────────────

VSIX="$WORK/weebo-bridge-notify-$WEEBO_VERSION.vsix"
JBZIP="$WORK/weebo-bridge-notify-$WEEBO_VERSION.zip"

log "Downloading weebo-bridge-notify v$WEEBO_VERSION release artifacts"
fetch_https -o "$VSIX" \
  "$WEEBO_BASE_URL/v$WEEBO_VERSION/weebo-bridge-notify-$WEEBO_VERSION.vsix"
fetch_https -o "$JBZIP" \
  "$WEEBO_BASE_URL/v$WEEBO_VERSION/weebo-bridge-notify-$WEEBO_VERSION.zip"

log "Publishing the VSIX to the local vscode-marketplace registry"
curl -fsS -X PUT \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @"$VSIX" \
  "$BASE/proxy/vscode/$VSCODE_EXT_ID/$WEEBO_VERSION/vsix" >/dev/null

log "Publishing the plugin to the local jetbrains-marketplace registry"
curl -fsS -X POST \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -F "xmlId=$JB_PLUGIN_ID" \
  -F "file=@$JBZIP" \
  "$BASE/proxy/jbm/api/updates/upload" | tee "$WORK/upload-response.json"
grep -q "\"pluginId\":\"$JB_PLUGIN_ID\"" "$WORK/upload-response.json" \
  || { echo "ERROR: unexpected JetBrains upload response" >&2; exit 1; }

# ── 3. VS Code: headless install from this instance only ─────────────────────

if [[ "${SKIP_VSCODE:-0}" != "1" ]]; then
  # The server build (`server-linux-x64-web`): the same `extensionGalleryService`
  # and `ExtensionManagementCLI` as the desktop, under the node it bundles, with
  # `product.json` at its root — and no Electron, so no xvfb and no GTK on the
  # runner. The desktop build was driven here up to 1.96.4; from 1.136 its
  # `cli.js` imports its dependencies as ES modules out of `node_modules.asar`,
  # which plain node cannot open (vsx_login.sh). vsx_login.sh and vsx_view.sh
  # share this download.
  VSCODE_DIR="$IDE_CACHE/vscode-server-web-$VSCODE_VERSION"
  CODE_SERVER="${CODE_SERVER:-$VSCODE_DIR/bin/code-server}"
  if [[ ! -x "$CODE_SERVER" ]]; then
    log "Downloading VS Code $VSCODE_VERSION (server-linux-x64-web)"
    mkdir -p "$VSCODE_DIR"
    fetch_https \
      "https://update.code.visualstudio.com/$VSCODE_VERSION/server-linux-x64-web/stable" \
      | tar -xz -C "$VSCODE_DIR" --strip-components=1
  fi
  [[ -x "$CODE_SERVER" ]] || { echo "ERROR: no bin/code-server in $VSCODE_DIR" >&2; exit 1; }

  log "Downloading the extension back through the proxy (public stores don't have it)"
  curl -fsS "$BASE/proxy/vscode/$VSCODE_EXT_ID/$WEEBO_VERSION/vsix" -o "$WORK/from-proxy.vsix"
  head -c 2 "$WORK/from-proxy.vsix" | grep -q "PK" \
    || { echo "ERROR: proxied VSIX is not a ZIP" >&2; exit 1; }

  # `env -u VSCODE_IPC_HOOK_CLI`: when this script runs from a terminal *inside*
  # VS Code, the CLI forwards every command to that editor over the IPC socket
  # instead of running locally. The install would then happen in the developer's
  # own editor, against the developer's own gallery, and `--list-extensions`
  # would report that editor's extensions — a pass that proves nothing about
  # BatleHub. Unset, so the editor is always this build, talking to this server.
  #
  # Each scenario gets its own `--server-data-dir`: that is where the server's
  # CLI reads `extensions.verifySignature` from (`data/User/settings.json`,
  # measured against the five candidate files in vsx_view.sh). Since 1.136 the
  # CLI verifies on install and refuses a package the Microsoft marketplace did
  # not sign with `NotSigned` — RFC 0020 §4.5: the setting is the one lever a
  # stock build has, and the one VSCodium and code-server ship off by default.
  vscode_profile() {  # <dir> → sets CODE to the CLI over a fresh profile under <dir>
    local dir="$1"
    mkdir -p "$dir/server/data/User"
    echo '{ "extensions.verifySignature": false }' >"$dir/server/data/User/settings.json"
    CODE=(env -u VSCODE_IPC_HOOK_CLI "$CODE_SERVER" --server-data-dir "$dir/server"
          --user-data-dir "$dir/user" --extensions-dir "$dir/extensions")
  }

  log "Installing into headless VS Code (from the proxied file)"
  vscode_profile "$WORK/vscode-file"
  "${CODE[@]}" --install-extension "$WORK/from-proxy.vsix" --force
  "${CODE[@]}" --list-extensions | grep -qix "$VSCODE_EXT_ID" \
    || { echo "ERROR: $VSCODE_EXT_ID not listed after install" >&2; exit 1; }
  log "VSCODE-MARKETPLACE-HEAVY-OK ($VSCODE_EXT_ID installed from this instance)"

  # ── 3b. Install by id through the VS Code gallery ──────────────────────────
  #
  # The scenario above proves the bytes flow; this one proves the *protocol*.
  # `--install-extension <id>` makes the editor resolve the extension through
  # `extensionsGallery.serviceUrl` — an `extensionquery` POST, then an asset
  # fetch — which is exactly what a developer configuring BatleHub as their
  # marketplace does. It also validates the `product.json` snippet the console
  # publishes, rather than a hand-written variant of it.
  PRODUCT_JSON="$VSCODE_DIR/product.json"
  [[ -f "$PRODUCT_JSON" ]] || { echo "ERROR: no product.json at $PRODUCT_JSON" >&2; exit 1; }

  log "Pointing product.json at this instance's VS Code gallery"
  cp "$PRODUCT_JSON" "$WORK/product.json.orig"
  BASE="$BASE" python3 tests/heavy/patch_product_json.py "$PRODUCT_JSON"

  # A fresh profile, so the file install above cannot be what makes this pass.
  vscode_profile "$WORK/vscode-gallery"
  log "Installing $VSCODE_EXT_ID by id through the gallery"
  if "${CODE[@]}" --install-extension "$VSCODE_EXT_ID" --force; then
    "${CODE[@]}" --list-extensions | grep -qix "$VSCODE_EXT_ID" \
      || { echo "ERROR: $VSCODE_EXT_ID not listed after gallery install" >&2; exit 1; }
    log "VSCODE-GALLERY-HEAVY-OK ($VSCODE_EXT_ID installed by id from this instance)"
  else
    echo "ERROR: gallery install failed; server log tail follows" >&2
    tail -50 "$WORK/server.log" >&2
    cp "$WORK/product.json.orig" "$PRODUCT_JSON"
    exit 1
  fi

  cp "$WORK/product.json.orig" "$PRODUCT_JSON"
else
  log "SKIP_VSCODE=1 — VS Code scenario skipped"
fi

# ── 4. JetBrains: headless installPlugins against updatePlugins.xml ──────────

if [[ "${SKIP_JETBRAINS:-0}" != "1" ]]; then
  IDEA_DIR="${IDEA_DIR:-$IDE_CACHE/idea-$IDEA_VERSION}"
  if [[ ! -x "$IDEA_DIR/bin/idea.sh" ]]; then
    log "Downloading IntelliJ IDEA $IDEA_VERSION"
    mkdir -p "$IDEA_DIR"
    # Unified installer naming since 2025.3; fall back to the old Community
    # edition tarball for overrides pinning an older IDEA_VERSION.
    if ! fetch_https \
        "https://download.jetbrains.com/idea/idea-$IDEA_VERSION.tar.gz" \
        | tar -xz -C "$IDEA_DIR" --strip-components=1; then
      rm -rf "$IDEA_DIR"
      mkdir -p "$IDEA_DIR"
      fetch_https \
        "https://download.jetbrains.com/idea/ideaIC-$IDEA_VERSION.tar.gz" \
        | tar -xz -C "$IDEA_DIR" --strip-components=1
    fi
  fi

  mkdir -p "$WORK/idea/config" "$WORK/idea/system" "$WORK/idea/plugins" "$WORK/idea/log"
  cat > "$WORK/idea.properties" <<EOF
idea.config.path=$WORK/idea/config
idea.system.path=$WORK/idea/system
idea.plugins.path=$WORK/idea/plugins
idea.log.path=$WORK/idea/log
EOF

  log "Installing $JB_PLUGIN_ID via headless installPlugins from this instance"
  IDEA_PROPERTIES="$WORK/idea.properties" \
    "$IDEA_DIR/bin/idea.sh" installPlugins "$JB_PLUGIN_ID" "$BASE/proxy/jbm/updatePlugins.xml"

  if [[ -z "$(ls -A "$WORK/idea/plugins" 2>/dev/null)" ]]; then
    echo "ERROR: no plugin was installed into $WORK/idea/plugins" >&2
    exit 1
  fi
  ls -R "$WORK/idea/plugins" | grep -qi "weebo" \
    || { echo "ERROR: installed plugins directory does not contain weebo-bridge-notify" >&2; ls -R "$WORK/idea/plugins" >&2; exit 1; }
  log "JETBRAINS-MARKETPLACE-HEAVY-OK ($JB_PLUGIN_ID installed from this instance)"
else
  log "SKIP_JETBRAINS=1 — JetBrains scenario skipped"
fi

# ── 5. Clean shutdown (flushes llvm coverage profiles) ───────────────────────

log "Stopping the server"
stop_server
log "MARKETPLACE-HEAVY-OK"
