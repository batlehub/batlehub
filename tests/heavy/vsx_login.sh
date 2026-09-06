#!/usr/bin/env bash
# Heavy RFC 0011 §4.4 suite — the local gallery proxy, against the real
# VS Code core (the current stable; `VSCODE_VERSION` picks another).
#
# RFC 0011 kept its loopback proxy and its sign-in bootstrap out of §14
# because they "wait on an editor build that cannot repoint its gallery URL".
# This suite is the editor build a test *can* repoint: the stock VS Code
# server build, its `product.json` rewritten to point at the proxy, and its
# CLI driven headlessly — the real `extensionGalleryService`, the real
# `extensionquery` bodies, the real install path. Not the Extensions *view*:
# that is `vsx_view.sh`, which opens the same build's workbench in a browser.
# The IDE this repo does not build (che-code) is not built here either; what
# is measured is the proxy and the editor core it fronts.
#
# The registry (`config.vsx-login.toml`) requires a credential to read:
# `anonymous` holds no verb. What it proves, in order:
#
#   1. **The server refuses an anonymous gallery and answers a user.** Read
#      off `extensionquery` directly, so the proxy below is proven to add
#      something and not to paper over an open registry.
#   2. **Unauthenticated, the proxy shows a way in, not a blank.** A search
#      through the proxy is `200` with exactly one entry, `batlehub.sign-in`,
#      carrying `Microsoft.VisualStudio.Code.Engine` (§4.4.4's mandatory
#      property); an install by id of a real extension fails in the editor's
#      own words and the tap sees **no** request — the proxy forwards nothing
#      it has no credential for; `code --install-extension batlehub.sign-in`
#      is refused as `NotSigned` — 1.136.1 verifies on the CLI path where
#      1.96.4 did not — then installs the package the proxy serves once
#      `extensions.verifySignature` is off. A request outside the session
#      segment is `404`.
#   3. **After `auth write-token-file`, without a restart, the same editor
#      installs the real extension by id.** Every gallery request the tap
#      sees carries `Authorization: Bearer`; none arrives without one; the
#      editor never held the token — its config names only the proxy.
#
# Ports: 8122 (server), 8129 (tap). The proxy binds an ephemeral loopback
# port of its own. Needs network once: the VS Code build and the fixture
# extension are downloaded into HEAVY_CACHE and reused.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT, HEAVY_TAP_PORT,
# VSCODE_VERSION (1.136.1), WEEBO_VERSION (0.5.0), COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init vsx_login 8122 8129
heavy_need python3 "python3"
heavy_need node "nodejs"
heavy_need curl "curl"

REG="vsx-$HEAVY_RUN"
EXT_ID="batleforc.weebo-bridge-notify"
WEEBO_VERSION="${WEEBO_VERSION:-0.5.0}"
WEEBO_BASE_URL="${WEEBO_BASE_URL:-https://github.com/batleforc/weebo-che-notify/releases/download}"
VSCODE_VERSION="${VSCODE_VERSION:-1.136.1}"
USER_TOKEN="heavy-user-token"

# Every download here goes straight into `tar` or is published back through
# the server, so the transport is the only wall between a redirect and code
# execution: `--proto-redir` pins the redirect chain too (marketplace.sh).
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' "$@"; }

# ── 0. The editor core and the fixture, cached across runs ──────────────────

# The server build (`server-linux-x64-web`): the same `extensionGalleryService`
# and `ExtensionManagementCLI` as the desktop, under the node it bundles, with
# `product.json` at its root. The desktop build's `cli.js` was driven under
# `ELECTRON_RUN_AS_NODE` up to 1.96.4; 1.136.1's imports its dependencies as
# ES modules out of `node_modules.asar`, which plain node cannot open, so the
# desktop core is no longer a headless client here. vsx_view.sh shares this
# download.
VSCODE_DIR="$HEAVY_CACHE/vscode-server-web-$VSCODE_VERSION"
CODE_SERVER="$VSCODE_DIR/bin/code-server"
if [[ ! -x "$CODE_SERVER" ]]; then
  heavy_log "Downloading VS Code $VSCODE_VERSION (server-linux-x64-web) into $VSCODE_DIR"
  mkdir -p "$VSCODE_DIR"
  fetch "https://update.code.visualstudio.com/$VSCODE_VERSION/server-linux-x64-web/stable" \
    | tar -xz -C "$VSCODE_DIR" --strip-components=1
fi
[[ -x "$CODE_SERVER" ]] || heavy_fail "no bin/code-server in the VS Code build at $VSCODE_DIR"
PRODUCT_JSON="$VSCODE_DIR/product.json"
cp "$PRODUCT_JSON" "$HEAVY_WORK/product.json.orig"
restore_product_json() { cp "$HEAVY_WORK/product.json.orig" "$PRODUCT_JSON" 2>/dev/null || true; }
trap 'restore_product_json; heavy_cleanup' EXIT

# Its own data directories under the work dir. `env -u VSCODE_IPC_HOOK_CLI`:
# from a terminal inside an editor a CLI would otherwise forward every
# command to that editor over IPC and report success for an install that
# happened somewhere else (heavy-test-env-gotchas).
EDITOR_DATA="$HEAVY_WORK/editor"
mkdir -p "$EDITOR_DATA"
CODE=(env -u VSCODE_IPC_HOOK_CLI "$CODE_SERVER" --server-data-dir "$EDITOR_DATA/server"
      --user-data-dir "$EDITOR_DATA/user" --extensions-dir "$EDITOR_DATA/extensions")

VSIX="$HEAVY_CACHE/weebo-bridge-notify-$WEEBO_VERSION.vsix"
if [[ ! -s "$VSIX" ]]; then
  heavy_log "Downloading weebo-bridge-notify v$WEEBO_VERSION"
  fetch -o "$VSIX" "$WEEBO_BASE_URL/v$WEEBO_VERSION/weebo-bridge-notify-$WEEBO_VERSION.vsix"
fi

cargo build --quiet -p batlehub-cli >"$HEAVY_WORK/cli-build.txt" 2>&1 \
  || { cat "$HEAVY_WORK/cli-build.txt" >&2; heavy_fail "the CLI did not build"; }
CLI="$(cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/batlehub-cli"
[[ -x "$CLI" ]] || heavy_fail "no batlehub-cli binary at $CLI"
export BATLEHUB_HOME="$HEAVY_WORK/home"
mkdir -p "$BATLEHUB_HOME/state"
CONTRACT="$BATLEHUB_HOME/state/vsx-token.json"

# ── 1. The server, the tap, the fixture; and the property under test ────────

heavy_start_server tests/heavy/config.vsx-login.toml
heavy_start_tap
REGISTRY_BASE="$HEAVY_TAP_BASE/proxy/$REG"

heavy_log "Publishing $EXT_ID $WEEBO_VERSION to the local registry"
curl -fsS -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/octet-stream" \
  --data-binary @"$VSIX" "$HEAVY_BASE/proxy/$REG/$EXT_ID/$WEEBO_VERSION/vsix" >/dev/null \
  || heavy_fail "publishing the fixture failed"

SEARCH_BODY='{"filters":[{"criteria":[{"filterType":8,"value":"Microsoft.VisualStudio.Code"},{"filterType":10,"value":"weebo"}],"pageNumber":1,"pageSize":50}],"flags":950}'
count_of() {  # <json> → the number of extensions in the first result
  python3 -c 'import json,sys;d=json.load(sys.stdin);print(len(d["results"][0]["extensions"]))'
}
# With `anonymous = []` the gallery refuses outright — a `403`, not the
# empty `200` of a registry that grants anonymous read — which is exactly
# the status that blanks an Extensions view (§4.4.2), and exactly why the
# proxy below answers an unauthenticated query itself.
ANON_CODE="$(curl -s -o "$HEAVY_WORK/anon-registry.json" -w '%{http_code}' -X POST -H "Content-Type: application/json" -d "$SEARCH_BODY" \
  "$REGISTRY_BASE/vscode/gallery/extensionquery")"
case "$ANON_CODE" in
  403) ANON=0 ;;
  200) ANON="$(count_of <"$HEAVY_WORK/anon-registry.json" || echo "?")" ;;
  *) heavy_fail "the registry answered an anonymous search with $ANON_CODE" ;;
esac
AUTHED="$(curl -sS -X POST -H "Content-Type: application/json" -H "Authorization: Bearer $USER_TOKEN" -d "$SEARCH_BODY" \
  "$REGISTRY_BASE/vscode/gallery/extensionquery" | count_of || echo "?")"
[[ "$ANON" == "0" ]] || heavy_fail "the registry answered an anonymous search with $ANON extension(s); it must answer none for this suite to prove anything"
[[ "$AUTHED" == "1" ]] || heavy_fail "the registry answered a user's search with $AUTHED extension(s), expected the fixture"
heavy_log "REGISTRY-OK (anonymous: $ANON_CODE, nothing; a user: $EXT_ID — the credential is what the proxy has to add)"

# ── 2. The proxy, unauthenticated ───────────────────────────────────────────

rm -f "$CONTRACT"
"$CLI" proxy serve --registry "$REGISTRY_BASE" --contract "$CONTRACT" \
  --state-dir "$BATLEHUB_HOME/state" --print-gallery-url \
  >"$HEAVY_WORK/proxy.url" 2>"$HEAVY_WORK/proxy.err" &
HEAVY_EXTRA_PIDS+=($!)
for _ in $(seq 1 30); do
  [[ -s "$HEAVY_WORK/proxy.url" ]] && break
  sleep 1
done
GALLERY="$(head -1 "$HEAVY_WORK/proxy.url")"
[[ "$GALLERY" == http://127.0.0.1:* ]] || { cat "$HEAVY_WORK/proxy.err" >&2; heavy_fail "the proxy printed no loopback gallery URL"; }
python3 - "$BATLEHUB_HOME/state/gallery-proxy.json" "$GALLERY" <<'PY' || heavy_fail "gallery-proxy.json does not name the URL the proxy printed, or is not private"
import json, os, stat, sys
path, url = sys.argv[1:]
st = json.load(open(path))
assert st["gallery_url"] == url and st["service_url"] == url + "/vscode/gallery", st
assert stat.S_IMODE(os.stat(path).st_mode) == 0o600, oct(os.stat(path).st_mode)
PY
heavy_log "Gallery proxy at $GALLERY (session in the path, state file 0600)"

heavy_log "Pointing product.json at the proxy"
python3 - "$PRODUCT_JSON" "$GALLERY" <<'PY'
import json, sys
path, base = sys.argv[1:]
product = json.load(open(path, encoding="utf-8"))
product["extensionsGallery"] = {
    "serviceUrl": f"{base}/vscode/gallery",
    "itemUrl": f"{base}/vscode/item",
    "resourceUrlTemplate": f"{base}/vscode/unpkg/{{publisher}}/{{name}}/{{version}}/{{path}}",
}
json.dump(product, open(path, "w", encoding="utf-8"))
PY

# A wrong session segment is a 404 (§4.4.1): the port protects nothing.
PROXY_ORIGIN="${GALLERY%%/vsx}"; PROXY_ORIGIN="${PROXY_ORIGIN%/*}"
WRONG="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Content-Type: application/json" -d "$SEARCH_BODY" \
  "$PROXY_ORIGIN/not-the-session/vsx/vscode/gallery/extensionquery")"
[[ "$WRONG" == "404" ]] || heavy_fail "a request outside the session segment answered $WRONG, expected 404"

heavy_mark "anon-search"
curl -sS -X POST -H "Content-Type: application/json" -d "$SEARCH_BODY" \
  "$GALLERY/vscode/gallery/extensionquery" >"$HEAVY_WORK/anon-search.json"
python3 - "$HEAVY_WORK/anon-search.json" <<'PY' || { cat "$HEAVY_WORK/anon-search.json" >&2; heavy_fail "an unauthenticated search through the proxy is not the one sign-in entry with Code.Engine"; }
import json, sys
d = json.load(open(sys.argv[1]))
exts = d["results"][0]["extensions"]
assert len(exts) == 1, len(exts)
e = exts[0]
assert e["publisher"]["publisherName"] == "batlehub" and e["extensionName"] == "sign-in", e
props = {p["key"]: p["value"] for p in e["versions"][0]["properties"]}
assert props.get("Microsoft.VisualStudio.Code.Engine"), props
types = {f["assetType"] for f in e["versions"][0]["files"]}
assert {"Microsoft.VisualStudio.Code.Manifest", "Microsoft.VisualStudio.Services.VSIXPackage",
        "Microsoft.VisualStudio.Services.Content.Details"} <= types, types
PY
[[ "$(heavy_wire_count_after anon-search 'extensionquery')" == "0" ]] \
  || heavy_fail "the unauthenticated search reached the registry — the proxy must answer it itself"
heavy_log "ANON-SEARCH-OK (one entry, batlehub.sign-in, Code.Engine set, nothing forwarded)"

heavy_mark "anon-install-by-id"
# `if`, not `set +e`: the ERR trap still fires under `set +e` and prints a
# "died at line" for a failure that is the measurement.
if "${CODE[@]}" --install-extension "$EXT_ID" >"$HEAVY_WORK/anon-install.txt" 2>&1; then RC=0; else RC=$?; fi
[[ $RC -ne 0 ]] || { cat "$HEAVY_WORK/anon-install.txt" >&2; heavy_fail "the editor installed $EXT_ID with no credential"; }
grep -qi "not found" "$HEAVY_WORK/anon-install.txt" \
  || { cat "$HEAVY_WORK/anon-install.txt" >&2; heavy_fail "the editor failed for a reason other than 'not found' — the by-name lookup must be an empty 200, not an error"; }
[[ "$(heavy_wire_count_after anon-install-by-id "/proxy/$REG/")" == "0" ]] \
  || heavy_fail "the editor's unauthenticated lookup reached the registry through the proxy"
heavy_log "ANON-BY-ID-OK (the editor says '$(grep -io "extension '[^']*' not found" "$HEAVY_WORK/anon-install.txt" | head -1)', and the registry was never asked)"

# 1.96.4 installed an unsigned package from a custom gallery without a
# word (§4.4.4's "holds"). 1.136.1 verifies on this path too and its default
# gallery manifest declares every public extension signed, so the package
# is refused with `NotSigned` until `extensions.verifySignature` is off in
# the editor's user settings — the setting the code-server and VSCodium
# families ship off. Measure the refusal, then set it and go on.
heavy_mark "anon-install-signin-refused"
if "${CODE[@]}" --install-extension batlehub.sign-in >"$HEAVY_WORK/signin-refused.txt" 2>&1; then RC=0; else RC=$?; fi
[[ $RC -ne 0 ]] || { cat "$HEAVY_WORK/signin-refused.txt" >&2; heavy_fail "VS Code $VSCODE_VERSION installed the unsigned sign-in package with signature verification on — re-measure"; }
grep -q "NotSigned" "$HEAVY_WORK/signin-refused.txt" \
  || { cat "$HEAVY_WORK/signin-refused.txt" >&2; heavy_fail "the editor refused batlehub.sign-in for a reason other than 'NotSigned'"; }
heavy_log "ANON-SIGNIN-REFUSED-OK (VS Code $VSCODE_VERSION: 'NotSigned' — an unsigned package needs extensions.verifySignature off)"
# Where the server's CLI reads its settings (measured against the five
# candidate files): `<server-data-dir>/data/User/settings.json`.
mkdir -p "$EDITOR_DATA/server/data/User"
echo '{ "extensions.verifySignature": false }' >"$EDITOR_DATA/server/data/User/settings.json"

heavy_mark "anon-install-signin"
"${CODE[@]}" --install-extension batlehub.sign-in >"$HEAVY_WORK/signin-install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/signin-install.txt" >&2; heavy_fail "stock VS Code refused the sign-in package the proxy serves, with extensions.verifySignature off"; }
grep -q "successfully installed" "$HEAVY_WORK/signin-install.txt" \
  || { cat "$HEAVY_WORK/signin-install.txt" >&2; heavy_fail "no 'successfully installed' for batlehub.sign-in"; }
"${CODE[@]}" --list-extensions 2>/dev/null | grep -qix "batlehub.sign-in" \
  || heavy_fail "batlehub.sign-in not listed after install"
heavy_log "ANON-SIGNIN-OK (the sign-in entry installs on stock VS Code, unsigned, from the proxy, once extensions.verifySignature is off)"

# ── 3. Sign in; the same editor, the same proxy, no restart ─────────────────

heavy_log "auth write-token-file for $REGISTRY_BASE"
BATLEHUB_TOKEN="$USER_TOKEN" "$CLI" --server "$REGISTRY_BASE" auth write-token-file --path "$CONTRACT" \
  >"$HEAVY_WORK/write-token.txt" 2>&1 || { cat "$HEAVY_WORK/write-token.txt" >&2; heavy_fail "auth write-token-file failed"; }
"$CLI" auth status --path "$CONTRACT" >"$HEAVY_WORK/status.txt" 2>&1 || true
grep -q "ok" "$HEAVY_WORK/status.txt" || { cat "$HEAVY_WORK/status.txt" >&2; heavy_fail "auth status does not report the entry as ok"; }
if grep -q "$USER_TOKEN" "$HEAVY_WORK/status.txt"; then heavy_fail "auth status printed the credential"; fi

heavy_mark "authed-search"
curl -sS -X POST -H "Content-Type: application/json" -d "$SEARCH_BODY" \
  "$GALLERY/vscode/gallery/extensionquery" >"$HEAVY_WORK/authed-search.json"
python3 - "$HEAVY_WORK/authed-search.json" "$GALLERY" "$EXT_ID" <<'PY' || { cat "$HEAVY_WORK/authed-search.json" >&2; heavy_fail "an authenticated search through the proxy is not the fixture with its assets on the proxy"; }
import json, sys
d, base, ext = json.load(open(sys.argv[1])), sys.argv[2], sys.argv[3]
exts = d["results"][0]["extensions"]
names = [f'{e["publisher"]["publisherName"]}.{e["extensionName"]}' for e in exts]
assert names == [ext], names
v = exts[0]["versions"][0]
assert v["assetUri"].startswith(base + "/"), v["assetUri"]
assert v["fallbackAssetUri"].startswith(base + "/"), v["fallbackAssetUri"]
PY
heavy_wire_re_after "authed-search" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer" \
  "the forwarded search did not carry a Bearer credential"
heavy_log "AUTHED-SEARCH-OK (the fixture, its assets rewritten onto the proxy, forwarded with a Bearer)"

heavy_mark "authed-install"
"${CODE[@]}" --install-extension "$EXT_ID" --force >"$HEAVY_WORK/authed-install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/authed-install.txt" >&2; heavy_fail "the editor could not install $EXT_ID through the proxy once signed in"; }
grep -q "successfully installed" "$HEAVY_WORK/authed-install.txt" \
  || { cat "$HEAVY_WORK/authed-install.txt" >&2; heavy_fail "no 'successfully installed' for $EXT_ID"; }
"${CODE[@]}" --list-extensions 2>/dev/null | grep -qix "$EXT_ID" || heavy_fail "$EXT_ID not listed after install"
heavy_wire_re_after "authed-install" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer" \
  "the editor's lookup was not forwarded with a Bearer"
heavy_wire_re_after "authed-install" "GET /proxy/$REG/vscode/(asset|gallery)/.* -> 200 .*Authorization: Bearer" \
  "the package was not fetched through the proxy with a Bearer"
# Nothing reached the registry without a credential after the sign-in: the
# rewriting of §4.4.3 is what keeps the download on the proxy's side.
BARE="$(awk -v mark="### authed-search" 'index($0, mark) == 1 { seen = 1; next } seen && /\/proxy\// && !/Authorization: Bearer/ { n++ } END { print n + 0 }' "$HEAVY_LOG")"
[[ "$BARE" == "0" ]] || { grep "/proxy/" "$HEAVY_LOG" | tail -20 >&2; heavy_fail "$BARE request(s) reached the registry without a credential after the sign-in"; }
heavy_log "AUTHED-INSTALL-OK ($EXT_ID installed by id; every registry request carried a Bearer, the editor's config names only the proxy)"

heavy_log "Measurement: VS Code $VSCODE_VERSION (server build, its CLI), proxy $GALLERY, registry $REGISTRY_BASE"
heavy_log "  anonymous: registry search $ANON_CODE, proxy search 1 (batlehub.sign-in), install-by-id refused with 'not found' and 0 registry requests, sign-in package installed"
heavy_log "  signed in: proxy search = [$EXT_ID], install by id succeeded, $(grep -c 'Authorization: Bearer' "$HEAVY_LOG") registry requests with a Bearer, $BARE without"

heavy_done VSX-LOGIN-HEAVY-OK
