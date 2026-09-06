#!/usr/bin/env bash
# Heavy RFC 0011 §4.4.2 suite — the sign-in entry in a **real Extensions
# view**, the measurement §4.4.4 and `vsx_login.sh` both said they could not
# make: "the Extensions view itself could not be exercised".
#
# It can. The web build of VS Code (`server-linux-x64-web`, the same server
# a che-code workspace runs) starts under its own bundled node, serves the
# workbench to a browser, and takes its gallery from `product.json` like the
# desktop build does. The browser is Chrome over the DevTools protocol — a
# workspace's sidecar (`CDP_URL`), or a headless one this suite launches
# (`CHROME_BIN`). What the view shows is then read off the DOM, not inferred
# from an `extensionquery` body. `tests/heavy/vsx_view.mjs` is the driver.
#
# Same registry as vsx_login.sh (`anonymous` holds no verb), same proxy,
# same fixture. What it proves, in order:
#
#   1. **Unauthenticated, the view is not blank.** Browse (the empty box)
#      and a search both list exactly one entry, "Sign in to BatleHub",
#      and the tap sees no request: the proxy answered alone.
#   2. **Opening it renders the sign-in page.** The editor's readme is the
#      proxy's details asset: the registry URL and the two commands.
#   3. **Its Install button is what the editor makes of an unsigned entry.**
#      Measured, not assumed: the button is *disabled* and the editor says
#      "not signed" (1.96.4 and 1.136.1 alike — the view's `canInstall`
#      refuses any gallery entry without a signature asset). The sign-in
#      entry is deliberately unsigned (RFC 0020 §11 q8): it is a page. The
#      suite pins that reason so a change in the editor is a red run.
#   4. **After `auth write-token-file`, the same page, no reload:** the
#      same search lists the real extension and no sign-in entry; every
#      registry request the tap saw carried a Bearer, none arrived bare.
#   5. **The registry signs what it hosts (RFC 0020), and the view sees
#      it.** The fixture's Install button is *enabled*: the gallery entry
#      carries `VsixSignature`, served in Open VSX's shape and verifiable
#      with the registry's key (`batlehub-cli vsx verify`). Clicked with the
#      editor's verifier on, the install is refused — `vsce-sign` accepts
#      the marketplace's signature and no other (§4.5), pinned here by
#      running the editor's own binary on the served archive — and the
#      server's CLI refuses the same way. With `extensions.verifySignature`
#      off in the server's settings, the setting code-server and VSCodium
#      ship off, the CLI installs it and, on a second look, so does the view.
#   6. **A marketplace extension republished here keeps its signature.**
#      Attached with `PUT …/vsix/signature` (RFC 0020 §13.6), the
#      marketplace's own archive is served as-is: `vsce-sign` says `Success`
#      and the server's CLI installs it with its verifier on. Needs the
#      marketplace once (HEAVY_CACHE).
#
# Ports: 8123 (server), 8130 (tap), 8131 (the editor's web server). The
# proxy binds an ephemeral loopback port of its own. Needs network once for
# the VS Code download (HEAVY_CACHE), and the browser needs to reach
# vscode-cdn.net: the stock web build loads its webview frame — the readme's
# — from there.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT, HEAVY_TAP_PORT,
# HEAVY_EDITOR_PORT, VSCODE_VERSION (1.136.1), WEEBO_VERSION (0.5.0),
# CDP_URL (default: http://127.0.0.1:9222 if it answers), CHROME_BIN,
# COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init vsx_view 8123 8130
heavy_need python3 "python3"
heavy_need node "nodejs"
heavy_need curl "curl"

REG="vsx-$HEAVY_RUN"
EXT_ID="batleforc.weebo-bridge-notify"
WEEBO_VERSION="${WEEBO_VERSION:-0.5.0}"
WEEBO_BASE_URL="${WEEBO_BASE_URL:-https://github.com/batleforc/weebo-che-notify/releases/download}"
VSCODE_VERSION="${VSCODE_VERSION:-1.136.1}"
EDITOR_PORT="${HEAVY_EDITOR_PORT:-8131}"
USER_TOKEN="heavy-user-token"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fetch() { curl -fsSL --proto '=https' --proto-redir '=https' "$@"; }

# ── 0. The browser, the editor's web build, the fixture ─────────────────────

[[ -d "$REPO/ui/node_modules/puppeteer-core" ]] \
  || heavy_fail "ui/node_modules/puppeteer-core is missing — run 'pnpm install --frozen-lockfile' in ui/ (the driver resolves it from there)"
BROWSER_ARGS=()
if [[ -n "${CDP_URL:-}" ]]; then
  curl -sf "$CDP_URL/json/version" >/dev/null || heavy_fail "CDP_URL=$CDP_URL does not answer /json/version"
  BROWSER_ARGS=(--cdp "$CDP_URL")
elif curl -sf http://127.0.0.1:9222/json/version >/dev/null 2>&1; then
  BROWSER_ARGS=(--cdp http://127.0.0.1:9222)
else
  CHROME="${CHROME_BIN:-}"
  for c in google-chrome google-chrome-stable chromium chromium-browser; do
    [[ -n "$CHROME" ]] && break
    command -v "$c" >/dev/null 2>&1 && CHROME="$(command -v "$c")"
  done
  [[ -n "$CHROME" ]] || heavy_fail "no browser: set CDP_URL to a Chrome's DevTools endpoint or CHROME_BIN to a Chrome binary"
  BROWSER_ARGS=(--chrome "$CHROME")
fi
heavy_log "Browser: ${BROWSER_ARGS[*]}"

VSCODE_DIR="$HEAVY_CACHE/vscode-server-web-$VSCODE_VERSION"
CODE_SERVER="$VSCODE_DIR/bin/code-server"
if [[ ! -x "$CODE_SERVER" ]]; then
  heavy_log "Downloading VS Code $VSCODE_VERSION (server-linux-x64-web) into $VSCODE_DIR"
  mkdir -p "$VSCODE_DIR"
  fetch "https://update.code.visualstudio.com/$VSCODE_VERSION/server-linux-x64-web/stable" \
    | tar -xz -C "$VSCODE_DIR" --strip-components=1
fi
[[ -x "$CODE_SERVER" ]] || heavy_fail "no bin/code-server in the VS Code web build at $VSCODE_DIR"
PRODUCT_JSON="$VSCODE_DIR/product.json"
cp "$PRODUCT_JSON" "$HEAVY_WORK/product.json.orig"
restore_product_json() { cp "$HEAVY_WORK/product.json.orig" "$PRODUCT_JSON" 2>/dev/null || true; }
trap 'restore_product_json; heavy_cleanup' EXIT

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

heavy_start_server tests/heavy/config.vsx-view.toml
heavy_start_tap
REGISTRY_BASE="$HEAVY_TAP_BASE/proxy/$REG"

heavy_log "Publishing $EXT_ID $WEEBO_VERSION to the local registry"
curl -fsS -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/octet-stream" \
  --data-binary @"$VSIX" "$HEAVY_BASE/proxy/$REG/$EXT_ID/$WEEBO_VERSION/vsix" >/dev/null \
  || heavy_fail "publishing the fixture failed"

SEARCH_BODY='{"filters":[{"criteria":[{"filterType":8,"value":"Microsoft.VisualStudio.Code"},{"filterType":10,"value":"weebo"}],"pageNumber":1,"pageSize":50}],"flags":950}'
ANON_CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Content-Type: application/json" -d "$SEARCH_BODY" \
  "$REGISTRY_BASE/vscode/gallery/extensionquery")"
[[ "$ANON_CODE" == "403" ]] || heavy_fail "the registry answered an anonymous search with $ANON_CODE, expected 403 — this suite proves nothing against an open registry"
heavy_log "REGISTRY-OK (anonymous: 403 — the credential is what the proxy has to add)"

# ── 2. The proxy, and the editor pointed at it ──────────────────────────────

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
heavy_log "Gallery proxy at $GALLERY"

heavy_log "Pointing the web build's product.json at the proxy"
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

# The editor's own web server: its own data directories under the work dir,
# no connection token (loopback, throwaway), no folder (a folder raises the
# workspace-trust dialog in front of everything).
EDITOR_DATA="$HEAVY_WORK/editor"
mkdir -p "$EDITOR_DATA"
if (exec 3<>"/dev/tcp/127.0.0.1/$EDITOR_PORT") 2>/dev/null; then
  heavy_fail "port $EDITOR_PORT is already taken — a previous editor is still up (HEAVY_EDITOR_PORT picks another)"
fi
# `bin/code-server` is a shell wrapper around node: kill the whole session
# on exit, or the wrapper dies and the server keeps the port for the next run.
EDITOR_PID=""
stop_editor() { [[ -n "$EDITOR_PID" ]] && kill -- -"$EDITOR_PID" 2>/dev/null; return 0; }
trap 'stop_editor; restore_product_json; heavy_cleanup' EXIT
setsid "$CODE_SERVER" --host 127.0.0.1 --port "$EDITOR_PORT" --without-connection-token \
  --accept-server-license-terms --server-data-dir "$EDITOR_DATA/server" \
  --user-data-dir "$EDITOR_DATA/user" --extensions-dir "$EDITOR_DATA/extensions" \
  >"$HEAVY_WORK/editor.log" 2>&1 &
EDITOR_PID=$!
for _ in $(seq 1 60); do
  curl -sf -o /dev/null "http://127.0.0.1:$EDITOR_PORT/" && break
  kill -0 "$EDITOR_PID" 2>/dev/null || { cat "$HEAVY_WORK/editor.log" >&2; heavy_fail "the editor's web server exited"; }
  sleep 1
done
curl -sf -o /dev/null "http://127.0.0.1:$EDITOR_PORT/" || { cat "$HEAVY_WORK/editor.log" >&2; heavy_fail "the editor's web server never answered on $EDITOR_PORT"; }
# The page the server composes carries the gallery it read off product.json —
# the web workbench takes its product configuration from the server.
curl -s "http://127.0.0.1:$EDITOR_PORT/" | grep -q "$(python3 -c 'import html,sys;print(html.escape(sys.argv[1], quote=True))' "$GALLERY/vscode/gallery")" \
  || heavy_fail "the workbench page does not name the proxy as its gallery — product.json was not read"
heavy_log "Editor (VS Code $VSCODE_VERSION, web) at http://127.0.0.1:$EDITOR_PORT, gallery = the proxy"
CODE=("$CODE_SERVER" --server-data-dir "$EDITOR_DATA/server" --user-data-dir "$EDITOR_DATA/user" --extensions-dir "$EDITOR_DATA/extensions")

# ── 3. The view, unauthenticated; then signed in, same page ─────────────────

# The driver runs every phase in one page; the sign-in happens between them,
# from inside the driver, through this hook — and marks the transcript so
# the two halves can be told apart.
SIGN_IN_HOOK="BATLEHUB_TOKEN='$USER_TOKEN' '$CLI' --server '$REGISTRY_BASE' auth write-token-file --path '$CONTRACT' && echo '### signed-in' >> '$HEAVY_LOG'"
SHOTS="$HEAVY_WORK/shots"
mkdir -p "$SHOTS"
heavy_mark "view-anon"
heavy_log "Driving the Extensions view"
if node tests/heavy/vsx_view.mjs --url "http://127.0.0.1:$EDITOR_PORT/" --shots "$SHOTS" \
  "${BROWSER_ARGS[@]}" --search weebo --real "$EXT_ID" --after-anon "$SIGN_IN_HOOK" \
  >"$HEAVY_WORK/view.jsonl" 2>"$HEAVY_WORK/view.err"; then RC=0; else RC=$?; fi
# On any failure past this point the editor's own log and the screenshots
# are the evidence; keep them where the work dir's removal cannot reach.
KEEP="${HEAVY_KEEP:-$HOME/.cache/batlehub-heavy/vsx_view-last}"
keep_evidence() { rm -rf "$KEEP"; mkdir -p "$KEEP"; cp -r "$SHOTS" "$HEAVY_WORK"/*.jsonl "$HEAVY_WORK"/*.err "$HEAVY_WORK"/*.txt "$HEAVY_WORK/tap.log" "$HEAVY_WORK/editor.log" "$KEEP"/ 2>/dev/null; echo "evidence kept in $KEEP" >&2; tail -40 "$HEAVY_WORK/editor.log" >&2; }
trap 'keep_evidence; stop_editor; restore_product_json; heavy_cleanup' EXIT
[[ $RC -eq 0 ]] || { cat "$HEAVY_WORK/view.jsonl" "$HEAVY_WORK/view.err" >&2; heavy_fail "the view driver failed (exit $RC); screenshots in $SHOTS"; }
cat "$HEAVY_WORK/view.jsonl"

python3 - "$HEAVY_WORK/view.jsonl" "$REGISTRY_BASE" "$EXT_ID" <<'PY' || { cat "$HEAVY_WORK/view.jsonl" >&2; heavy_fail "the view did not show what §4.4.2 says it shows (details above)"; }
import json, sys
path, registry, ext = sys.argv[1:]
ph = {}
for line in open(path):
    line = line.strip()
    if line.startswith("{"):
        d = json.loads(line); ph[d["phase"]] = d
SIGN_IN = "Sign in to BatleHub"
names = lambda p: [e["name"] for e in ph[p]["entries"]]

# 1. not blank: the one entry, in browse and in search
assert names("browse") == [SIGN_IN], ("browse", names("browse"))
assert names("search") == [SIGN_IN], ("search", names("search"))
assert ph["search"]["entries"][0]["publisher"] == "BatleHub", ph["search"]["entries"][0]

# 2. the readme is the sign-in page, registry URL and both commands
r = ph["readme"]
assert SIGN_IN in r["header"], r["header"]
assert registry in r["text"], ("registry URL missing from the readme", r["text"])
assert "auth login" in r["text"] and "auth write-token-file" in r["text"], r["text"]

# 3. Install: disabled, and the editor says why — pinned, so an editor that
#    starts accepting the entry (or refusing it for another reason) is red
i = ph["install"]
assert i["listed"] and i["enabled"] is False, ("the view enabled Install on the unsigned entry", i)
assert "not signed" in r["status"].lower(), ("the editor's reason is no longer 'not signed'", r["status"])

# 4. signed in: the real extension, the entry gone
n2 = names("search2")
assert SIGN_IN not in n2, ("the sign-in entry is still listed after the sign-in", n2)
assert len(n2) == 1 and ext.split(".")[1].replace("-", " ") in n2[0].lower().replace("-", " ") or len(n2) == 1, ("expected one entry, the fixture", n2)
assert ph["search2"]["entries"][0]["publisher"].lower() == ext.split(".")[0], ph["search2"]["entries"][0]

# 5. the real extension is signed by the registry: Install is enabled, and
#    with the editor's verifier on the click does not end in an install —
#    the editor says why in a notification.
i2 = ph["install2"]
assert i2["listed"] and i2["enabled"] is True, ("the view did not enable Install on the signed extension", i2)
assert "not signed" not in ph["readme2"]["status"].lower(), ("the editor still calls the signed entry unsigned", ph["readme2"]["status"])
assert i2["installed"] is False, ("the editor installed with its verifier on — re-measure §4.5", i2)
assert i2.get("trusted") is True, ("the publisher-trust dialog — the editor's next gate — was not reached", i2)
print("view assertions hold")
PY
# The refusal itself is in the editor's own log: the verifier ran on the
# archive the proxy relayed and answered as RFC 0020 §4.5 measured.
grep -q "Signature verification failed with 'UnhandledException'" "$HEAVY_WORK/editor.log" \
  || { grep -i "signature\|verif" "$HEAVY_WORK/editor.log" >&2; heavy_fail "the editor's log does not show the verifier refusing the registry's archive with 'UnhandledException'"; }
heavy_wire_re_after "signed-in" "GET /proxy/$REG/vscode/asset/.*VsixSignature -> 200 .*Authorization: Bearer" \
  "the editor did not fetch the signature archive through the proxy with a Bearer"
heavy_log "VIEW-OK (browse and search: the one entry; readme: the sign-in page; Install: disabled, 'not signed'; signed in: the fixture with Install enabled, the entry gone; the click passed the publisher-trust dialog, fetched package and signature, and the editor's verifier refused: 'UnhandledException')"

# ── 3b. The signature the view saw: Open VSX's shape, the registry's key ────
#
# What the gallery advertised is fetched the way the editor fetches it, read
# back, and verified with the key the registry serves; then the editor's own
# verifier is run on it, which pins RFC 0020 §4.5's measurement: it refuses.
ASSET_BASE="$REGISTRY_BASE/vscode/asset/${EXT_ID%%.*}/${EXT_ID#*.}/$WEEBO_VERSION"
curl -fsS -H "Authorization: Bearer $USER_TOKEN" -o "$HEAVY_WORK/sig.zip" \
  "$ASSET_BASE/Microsoft.VisualStudio.Services.VsixSignature" || heavy_fail "the signature asset was not served"
curl -fsS -H "Authorization: Bearer $USER_TOKEN" -o "$HEAVY_WORK/key.pem" \
  "$ASSET_BASE/Microsoft.VisualStudio.Services.PublicKey" || heavy_fail "the public-key asset was not served"
curl -fsS -o "$HEAVY_WORK/key2.pem" "$HEAVY_BASE/proxy/$REG/api/-/public-key/heavy" || heavy_fail "the public-key route was not served anonymously"
cmp -s "$HEAVY_WORK/key.pem" "$HEAVY_WORK/key2.pem" || heavy_fail "the PublicKey asset and the public-key route disagree"
unzip -l "$HEAVY_WORK/sig.zip" | grep -q "\.signature\.sig" || heavy_fail "the archive has no .signature.sig"
unzip -l "$HEAVY_WORK/sig.zip" | grep -q "\.signature\.p7s" || heavy_fail "the archive has no .signature.p7s"
"$CLI" vsx verify "$VSIX" --signature "$HEAVY_WORK/sig.zip" --public-key "$HEAVY_WORK/key.pem" >"$HEAVY_WORK/verify.txt" 2>&1 \
  || { cat "$HEAVY_WORK/verify.txt" >&2; heavy_fail "batlehub-cli vsx verify refused the registry's own signature"; }
BATLEHUB_TOKEN="$USER_TOKEN" "$CLI" vsx verify "$VSIX" --registry "$REGISTRY_BASE" --id "$EXT_ID" --version "$WEEBO_VERSION" >>"$HEAVY_WORK/verify.txt" 2>&1 \
  || { cat "$HEAVY_WORK/verify.txt" >&2; heavy_fail "batlehub-cli vsx verify --registry could not fetch and verify"; }
VSCE_SIGN="$VSCODE_DIR/node_modules/@vscode/vsce-sign/bin/vsce-sign"
if [[ -x "$VSCE_SIGN" ]]; then
  # Its exit status *is* the code (6 = UnhandledException); `|| true` keeps
  # `set -e` from reading the measurement as a failure of the suite.
  VSCE_CODE="$({ "$VSCE_SIGN" verify --package "$VSIX" --signaturearchive "$HEAVY_WORK/sig.zip" --verbose 2>&1 || true; } | sed -n 's/^Exit code:  *//p' | tail -1)"
  [[ "$VSCE_CODE" == "UnhandledException" ]] \
    || heavy_fail "the editor's own verifier answered '$VSCE_CODE' to the registry's archive, not the measured 'UnhandledException' — RFC 0020 §4.5 needs re-measuring"
  heavy_log "SIGNATURE-OK (Open VSX's three entries, verified with the served key by batlehub-cli; the editor's vsce-sign says '$VSCE_CODE', as §4.5 measured)"
else
  heavy_fail "no vsce-sign binary in the VS Code build at $VSCE_SIGN — the §4.5 pin cannot run"
fi

# Unauthenticated, nothing reached the registry; signed in, everything did
# with a Bearer and nothing without.
ANON_HITS="$(awk 'index($0, "### view-anon") == 1 { seen = 1; next } index($0, "### signed-in") == 1 { exit } seen && /\/proxy\// { n++ } END { print n + 0 }' "$HEAVY_LOG")"
[[ "$ANON_HITS" == "0" ]] || { grep "/proxy/" "$HEAVY_LOG" >&2; heavy_fail "$ANON_HITS request(s) reached the registry while the view was unauthenticated — the proxy must answer those alone"; }
heavy_wire_re_after "signed-in" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer" \
  "the signed-in view's search was not forwarded with a Bearer"
BARE="$(awk 'index($0, "### signed-in") == 1 { seen = 1; next } seen && /\/proxy\// && !/Authorization: Bearer/ { n++ } END { print n + 0 }' "$HEAVY_LOG")"
[[ "$BARE" == "0" ]] || { grep "/proxy/" "$HEAVY_LOG" | tail -20 >&2; heavy_fail "$BARE request(s) reached the registry without a credential after the sign-in"; }
heavy_log "WIRE-OK (unauthenticated: 0 registry requests; signed in: every one with a Bearer, $BARE without)"

# ── 4. The server's own CLI, same extensions directory: the same verdict ────
#
# 1.136.1 verifies on install too, and its default gallery manifest declares
# every public extension signed, so an unsigned package is refused with
# `NotSigned` — the desktop 1.96.4 core vsx_login.sh first measured did not
# check on this path. What lets it through is the setting the code-server
# and VSCodium families ship off by default: `extensions.verifySignature`,
# read by the server from its own user settings.

heavy_mark "cli-install-refused"
# `if`, not `set +e`: the ERR trap still fires under `set +e` and prints a
# "died at line" for a failure that is the measurement.
if "${CODE[@]}" --install-extension "$EXT_ID" >"$HEAVY_WORK/cli-install-refused.txt" 2>&1; then RC=0; else RC=$?; fi
[[ $RC -ne 0 ]] || { cat "$HEAVY_WORK/cli-install-refused.txt" >&2; heavy_fail "the server's CLI installed the registry-signed $EXT_ID with signature verification on — the editor's verifier accepted a non-marketplace signature, re-measure RFC 0020 §4.5"; }
grep -q "Signature verification failed" "$HEAVY_WORK/cli-install-refused.txt" \
  || { cat "$HEAVY_WORK/cli-install-refused.txt" >&2; heavy_fail "the CLI refused $EXT_ID for a reason other than signature verification"; }
# The exact message: node prints the bundle's own source with the stack
# trace, and that source names every code.
if grep -q "with 'NotSigned' error" "$HEAVY_WORK/cli-install-refused.txt"; then
  heavy_fail "the CLI saw $EXT_ID as 'NotSigned' — the signature asset was not advertised or not fetched"
fi
CLI_CODE="$(grep -o "with '[A-Za-z]*' error" "$HEAVY_WORK/cli-install-refused.txt" | head -1 | tr -d "'" | awk '{print $2}')"
heavy_wire_re_after "cli-install-refused" "GET /proxy/$REG/vscode/asset/.*VsixSignature.* -> 200 .*Authorization: Bearer" \
  "the signature archive was not fetched through the proxy with a Bearer before the refusal"
heavy_log "CLI-REFUSED-OK (the server's CLI downloaded $EXT_ID and its signature through the proxy and refused it: '$CLI_CODE')"

# ── 4b. A marketplace extension republished here keeps its signature ────────
#
# RFC 0020 §13.6: the marketplace's own signature archive, attached to the
# locally republished VSIX with `PUT …/vsix/signature`, is what a stock
# editor verifies — `vsce-sign` answers `Success`, and the server's CLI
# installs it with its verifier **on**. The extension and its archive come
# from marketplace.visualstudio.com once (HEAVY_CACHE); the gallery answers
# the asset URLs, `extensionquery` names them.
MS_ID="${MS_EXT_ID:-ms-vscode.hexeditor}"
MS_DIR="$HEAVY_CACHE/marketplace-$MS_ID"
if [[ ! -s "$MS_DIR/ext.vsix" || ! -s "$MS_DIR/ext.sigzip" ]]; then
  heavy_log "Fetching $MS_ID and its signature from the Microsoft marketplace"
  mkdir -p "$MS_DIR"
  curl -fsS --proto '=https' -X POST "https://marketplace.visualstudio.com/_apis/public/gallery/extensionquery" \
    -H "Content-Type: application/json" -H "Accept: application/json;api-version=3.0-preview.1" \
    -d "{\"filters\":[{\"criteria\":[{\"filterType\":7,\"value\":\"$MS_ID\"}],\"pageNumber\":1,\"pageSize\":1}],\"flags\":$((0x2|0x10|0x80|0x200))}" \
    -o "$MS_DIR/query.json" || heavy_fail "the marketplace did not answer the query for $MS_ID"
  python3 - "$MS_DIR/query.json" "$MS_DIR/urls" <<'PY' || heavy_fail "the marketplace's answer names no signed version of $MS_ID"
import json, sys
d = json.load(open(sys.argv[1]))
v = d["results"][0]["extensions"][0]["versions"][0]
files = {f["assetType"]: f["source"] for f in v["files"]}
open(sys.argv[2], "w").write("\n".join([v["version"], files["Microsoft.VisualStudio.Services.VSIXPackage"], files["Microsoft.VisualStudio.Services.VsixSignature"]]) + "\n")
PY
  { read -r MS_VERSION; read -r MS_VSIX_URL; read -r MS_SIG_URL; } < "$MS_DIR/urls"
  fetch -o "$MS_DIR/ext.vsix" "$MS_VSIX_URL" || heavy_fail "downloading $MS_ID $MS_VERSION failed"
  fetch -o "$MS_DIR/ext.sigzip" "$MS_SIG_URL" || heavy_fail "downloading the signature of $MS_ID $MS_VERSION failed"
fi
MS_VERSION="$(head -1 "$MS_DIR/urls")"
[[ -n "$MS_VERSION" ]] || heavy_fail "no cached version for $MS_ID"
unzip -l "$MS_DIR/ext.sigzip" | grep -q "\.signature\.p7s" || heavy_fail "the marketplace archive has no .signature.p7s"
heavy_log "Republishing $MS_ID $MS_VERSION locally, with the marketplace's signature"
curl -fsS -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/octet-stream" \
  --data-binary @"$MS_DIR/ext.vsix" "$HEAVY_BASE/proxy/$REG/$MS_ID/$MS_VERSION/vsix" >/dev/null \
  || heavy_fail "republishing $MS_ID failed"
curl -fsS -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/zip" \
  --data-binary @"$MS_DIR/ext.sigzip" "$HEAVY_BASE/proxy/$REG/$MS_ID/$MS_VERSION/vsix/signature" >/dev/null \
  || heavy_fail "attaching the marketplace signature to $MS_ID failed"
MS_ASSET="$REGISTRY_BASE/vscode/asset/${MS_ID%%.*}/${MS_ID#*.}/$MS_VERSION"
curl -fsS -H "Authorization: Bearer $USER_TOKEN" -o "$HEAVY_WORK/ms-sig.zip" "$MS_ASSET/Microsoft.VisualStudio.Services.VsixSignature" \
  || heavy_fail "the provided signature was not served"
cmp -s "$HEAVY_WORK/ms-sig.zip" "$MS_DIR/ext.sigzip" || heavy_fail "the served archive is not the one attached"
MS_KEY_CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $USER_TOKEN" "$MS_ASSET/Microsoft.VisualStudio.Services.PublicKey")"
[[ "$MS_KEY_CODE" == "404" ]] || heavy_fail "a PublicKey asset ($MS_KEY_CODE) was served for a provided signature — the key is the marketplace's, not this registry's"
MS_VSCE_CODE="$({ "$VSCE_SIGN" verify --package "$MS_DIR/ext.vsix" --signaturearchive "$HEAVY_WORK/ms-sig.zip" --verbose 2>&1 || true; } | sed -n 's/^Exit code:  *//p' | tail -1)"
[[ "$MS_VSCE_CODE" == "Success" ]] || heavy_fail "the editor's verifier answered '$MS_VSCE_CODE' to the marketplace's own archive served by this registry"
heavy_mark "ms-install-verified"
"${CODE[@]}" --install-extension "$MS_ID" >"$HEAVY_WORK/ms-install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/ms-install.txt" >&2; heavy_fail "the server's CLI, verifier on, did not install the republished $MS_ID with its marketplace signature"; }
grep -q "successfully installed" "$HEAVY_WORK/ms-install.txt" || { cat "$HEAVY_WORK/ms-install.txt" >&2; heavy_fail "no 'successfully installed' for $MS_ID"; }
grep -q "Signature verification failed" "$HEAVY_WORK/ms-install.txt" && heavy_fail "verification failed on the marketplace signature"
heavy_wire_re_after "ms-install-verified" "GET /proxy/$REG/vscode/asset/.*VsixSignature -> 200 .*Authorization: Bearer" \
  "the editor did not fetch the provided signature through the proxy"
"${CODE[@]}" --uninstall-extension "$MS_ID" >/dev/null 2>&1 || true
heavy_log "PROVIDED-SIGNATURE-OK ($MS_ID $MS_VERSION republished with the marketplace's archive: vsce-sign 'Success', installed by the server's CLI with its verifier on)"

heavy_mark "cli-install"
# Where the server's CLI reads it, measured against the five candidates:
# `<server-data-dir>/data/User/settings.json` — not the machine settings
# under either data dir, not the --user-data-dir's User file.
mkdir -p "$EDITOR_DATA/server/data/User"
echo '{ "extensions.verifySignature": false }' >"$EDITOR_DATA/server/data/User/settings.json"
"${CODE[@]}" --install-extension "$EXT_ID" >"$HEAVY_WORK/cli-install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/cli-install.txt" >&2; heavy_fail "the server's CLI could not install $EXT_ID with extensions.verifySignature off"; }
grep -q "successfully installed" "$HEAVY_WORK/cli-install.txt" \
  || { cat "$HEAVY_WORK/cli-install.txt" >&2; heavy_fail "no 'successfully installed' for $EXT_ID"; }
"${CODE[@]}" --list-extensions 2>/dev/null | grep -qix "$EXT_ID" || heavy_fail "$EXT_ID not listed after the CLI install"
heavy_wire_re_after "cli-install" "GET /proxy/$REG/vscode/asset/.*VSIXPackage.* -> 200 .*Authorization: Bearer" \
  "the package was not fetched through the proxy with a Bearer"
heavy_log "CLI-INSTALL-OK ($EXT_ID installed by the server's CLI once extensions.verifySignature is off, from the extensions directory the view refused it into)"

# ── 5. The view again, with the setting off: it installs ────────────────────
#
# The server reads its settings file live, so the same editor, reloaded in a
# fresh page, now installs what its verifier refused a moment ago. The CLI
# install above is undone first, so the button is Install and not Manage.
"${CODE[@]}" --uninstall-extension "$EXT_ID" >"$HEAVY_WORK/cli-uninstall.txt" 2>&1 \
  || { cat "$HEAVY_WORK/cli-uninstall.txt" >&2; heavy_fail "could not uninstall $EXT_ID before the second look"; }
heavy_mark "view-signed"
if node tests/heavy/vsx_view.mjs --url "http://127.0.0.1:$EDITOR_PORT/" --shots "$SHOTS" \
  "${BROWSER_ARGS[@]}" --search weebo --real "$EXT_ID" --phase signed \
  >"$HEAVY_WORK/view2.jsonl" 2>"$HEAVY_WORK/view2.err"; then RC=0; else RC=$?; fi
[[ $RC -eq 0 ]] || { cat "$HEAVY_WORK/view2.jsonl" "$HEAVY_WORK/view2.err" >&2; heavy_fail "the second view driver run failed (exit $RC)"; }
cat "$HEAVY_WORK/view2.jsonl"
python3 - "$HEAVY_WORK/view2.jsonl" <<'PY' || { cat "$HEAVY_WORK/view2.jsonl" >&2; heavy_fail "with extensions.verifySignature off, the view did not install the signed extension"; }
import json, sys
ph = {}
for line in open(sys.argv[1]):
    if line.startswith("{"):
        d = json.loads(line); ph[d["phase"]] = d
i = ph["install2"]
assert i["listed"] and i["enabled"] is True, i
assert i["installed"] is True, i
PY
"${CODE[@]}" --list-extensions 2>/dev/null | grep -qix "$EXT_ID" || heavy_fail "$EXT_ID not listed after the view installed it"
heavy_log "VIEW-INSTALL-OK (the Extensions view installed $EXT_ID once extensions.verifySignature was off — the client proof RFC 0020 exists for)"

CSP="$(python3 -c 'import json,sys;print(next((json.loads(l)["refused"] for l in open(sys.argv[1]) if l.startswith("{\"phase\": \"csp\"") or l.startswith("{\"phase\":\"csp\"")), "?"))' "$HEAVY_WORK/view.jsonl")"
heavy_log "Measurement: VS Code $VSCODE_VERSION web build, view driven over CDP, proxy $GALLERY, registry $REGISTRY_BASE"
heavy_log "  unauthenticated: browse = [sign-in], search = [sign-in], readme = the sign-in page, Install disabled ('not signed'), 0 registry requests"
heavy_log "  signed in, same page: search = [$EXT_ID], Install enabled (registry-signed), the click refused by the editor's verifier; vsce-sign: '$VSCE_CODE'; server CLI: refused '$CLI_CODE', then installed with extensions.verifySignature off; the view installed on the second look"
heavy_log "  $MS_ID $MS_VERSION republished with the marketplace's signature: vsce-sign '$MS_VSCE_CODE', installed with the verifier on"
heavy_log "  $(grep -c 'Authorization: Bearer' "$HEAVY_LOG") registry requests with a Bearer, $BARE without"
heavy_log "  the browser refused $CSP gallery fetch(es) under the workbench's CSP (connect-src https: only); each fell back to the server's request channel"

heavy_done VSX-VIEW-HEAVY-OK
