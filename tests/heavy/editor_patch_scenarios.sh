# The che-code credential patch (RFC 0011 §4.2, §6.4, §14.13) in a real
# editor: case A of §5.5 — **the editor itself sends the credential**, no
# gallery proxy, no extension. Sourced by an entry script that has run
# `heavy_init` and defined the editor:
#
#   EDITOR_LABEL      "VS Code 1.136.2" — for the log
#   EDITOR_DIR        the build's root; `product.json` lives there
#   GALLERY_ENV_NAME  the variable the patch reads the gallery from:
#                     VSX_REGISTRY_URL (stock), OPENVSX_REGISTRY_URL (che-code)
#   editor_node …     run the build's own node
#   editor_cli <profile> [VAR=value …] -- <args>
#                     run the build's CLI over a fresh profile, with the patch
#                     preloaded (tests/heavy/editor_patch_preload.mjs) — the
#                     entry script owns how its build is started
#
# `product.json` is pointed at a registry whose `anonymous` holds no verb,
# through the logging tap, so what the tap records is the editor core's own
# request, header included:
#
#   1. No credential anywhere: an install by id fails, the registry saw the
#      query and answered 403, and no request carried an Authorization.
#   2. `VSX_REGISTRY_AUTH_TOKEN` with the gallery named by $GALLERY_ENV_NAME:
#      the install succeeds; every request that reached the registry carried
#      `Authorization: Bearer`.
#   3. The contract file `batlehub-cli auth write-token-file` writes, at the
#      default path under `BATLEHUB_HOME`, keyed by the gallery's origin: the
#      install succeeds with no variable at all — the file the CLI writes is
#      the file the patch reads, looked up by the request's own origin.
#   4. Resolution order: the file wins over a wrong environment variable.
#   5. A contract entry that is a token *source* (`--from-file`) is no
#      credential to the patch — refused without the variable, installed with
#      it: the documented fall-through, on the real editor.
#   6. The credential never appears in anything the editor printed.

: "${EDITOR_LABEL:?}" "${EDITOR_DIR:?}" "${GALLERY_ENV_NAME:?}"
declare -f editor_node >/dev/null || heavy_fail "the entry script must define editor_node"
declare -f editor_cli >/dev/null || heavy_fail "the entry script must define editor_cli"

REG="vsx-$HEAVY_RUN"
EXT_ID="batleforc.weebo-bridge-notify"
WEEBO_VERSION="${WEEBO_VERSION:-0.5.0}"
WEEBO_BASE_URL="${WEEBO_BASE_URL:-https://github.com/batleforc/weebo-che-notify/releases/download}"
USER_TOKEN="heavy-user-token"
HDR_JSON="Content-Type: application/json"
PRELOAD="$HEAVY_ROOT/tests/heavy/editor_patch_preload.mjs"
PRODUCT_JSON="$EDITOR_DIR/product.json"
[[ -f "$PRODUCT_JSON" ]] || heavy_fail "no product.json at $PRODUCT_JSON"

fetch() { curl -fsSL --proto '=https' --proto-redir '=https' "$@"; return $?; }

cp "$PRODUCT_JSON" "$HEAVY_WORK/product.json.orig"
restore_product_json() { cp "$HEAVY_WORK/product.json.orig" "$PRODUCT_JSON" 2>/dev/null || true; return $?; }
trap 'restore_product_json; heavy_cleanup' EXIT

# ── 0. The module's own tests under the editor's node, the fixture, the CLI ──

editor_node --test "$HEAVY_ROOT/patches/che-code/vsxRegistryAuth.test.ts" >"$HEAVY_WORK/patch-unit.txt" 2>&1 \
  || { cat "$HEAVY_WORK/patch-unit.txt" >&2; heavy_fail "the patch's unit tests fail under $EDITOR_LABEL's node"; }
heavy_log "PATCH-UNIT-OK ($(grep -c '^✔' "$HEAVY_WORK/patch-unit.txt") properties, under $EDITOR_LABEL's own node $(editor_node --version))"

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

# ── 1. The server, the tap, the fixture; and what the registry answers ──────

heavy_start_server tests/heavy/config.editor-patch.toml
heavy_start_tap
REGISTRY_BASE="$HEAVY_TAP_BASE/proxy/$REG"
GALLERY_URL="$REGISTRY_BASE/vscode/gallery"

heavy_log "Publishing $EXT_ID $WEEBO_VERSION to the local registry"
curl -fsS -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/octet-stream" \
  --data-binary @"$VSIX" "$HEAVY_BASE/proxy/$REG/$EXT_ID/$WEEBO_VERSION/vsix" >/dev/null \
  || heavy_fail "publishing the fixture failed"

SEARCH_BODY='{"filters":[{"criteria":[{"filterType":8,"value":"Microsoft.VisualStudio.Code"},{"filterType":10,"value":"weebo"}],"pageNumber":1,"pageSize":50}],"flags":950}'
ANON_CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$HDR_JSON" -d "$SEARCH_BODY" "$GALLERY_URL/extensionquery")"
[[ "$ANON_CODE" == "403" ]] || heavy_fail "the registry answered an anonymous search with $ANON_CODE, expected 403 — it must require a credential for this suite to prove anything"
AUTHED="$(curl -sS -X POST -H "$HDR_JSON" -H "Authorization: Bearer $USER_TOKEN" -d "$SEARCH_BODY" "$GALLERY_URL/extensionquery" \
  | python3 -c 'import sys,json;d=json.load(sys.stdin);print(len(d["results"][0]["extensions"]))' || echo "?")"
[[ "$AUTHED" == "1" ]] || heavy_fail "the registry answered a user's search with $AUTHED extension(s), expected the fixture"
heavy_log "REGISTRY-OK (anonymous: 403; a user: $EXT_ID — the credential is what the editor has to send)"

heavy_log "Pointing $EDITOR_LABEL's product.json at $GALLERY_URL"
python3 - "$PRODUCT_JSON" "$REGISTRY_BASE" <<'PY'
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

# editor <profile> [VAR=value …] -- <args> — the entry's CLI over a fresh
# profile, with the preload, both credential variables cleared first so the
# caller's environment cannot leak one in, and the gallery named the way the
# build's own launcher names it. Since 1.136 the CLI verifies signatures on
# install and the fixture is not marketplace-signed, so the setting the
# VSCodium family ships off is off here (RFC 0020 §4.5).
editor() {
  local profile="$1"; shift
  local -a vars=()
  local arg
  while [[ $# -gt 0 ]]; do
    arg="$1"; shift
    [[ "$arg" == "--" ]] && break
    vars+=("$arg")
  done
  mkdir -p "$profile/server/data/User" "$profile/home/state"
  echo '{ "extensions.verifySignature": false }' >"$profile/server/data/User/settings.json"
  (
    unset VSCODE_IPC_HOOK_CLI VSX_REGISTRY_AUTH_TOKEN VSX_REGISTRY_AUTH_TOKEN_FILE VSX_REGISTRY_URL OPENVSX_REGISTRY_URL
    export BATLEHUB_HOME="$profile/home"
    export NODE_OPTIONS="--import=$PRELOAD --disable-warning=ExperimentalWarning"
    local v
    for v in "${vars[@]}"; do export "${v?}"; done
    editor_cli "$profile" "$@"
  )
  return $?
}

bare_requests_after() {  # <mark>
  local mark="$1"
  awk -v mark="### $mark" 'index($0, mark) == 1 { seen = 1; next } seen && /\/proxy\// && !/Authorization: Bearer/ { n++ } END { print n + 0 }' "$HEAVY_LOG"
  return $?
}
credentialed_after() {  # <mark>
  local mark="$1"
  awk -v mark="### $mark" 'index($0, mark) == 1 { seen = 1; next } seen && /\/proxy\// && /Authorization: Bearer/ { n++ } END { print n + 0 }' "$HEAVY_LOG"
  return $?
}

# ── 2. No credential: the patch is armed, and sends nothing ─────────────────

P="$HEAVY_WORK/anon"
heavy_mark "anon"
if editor "$P" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/anon.txt" 2>&1; then RC=0; else RC=$?; fi
grep -q "gallery credential patch armed" "$HEAVY_WORK/anon.txt" \
  || { cat "$HEAVY_WORK/anon.txt" >&2; heavy_fail "the preload did not run inside $EDITOR_LABEL (no 'patch armed' line)"; }
[[ $RC -ne 0 ]] || { cat "$HEAVY_WORK/anon.txt" >&2; heavy_fail "$EDITOR_LABEL installed $EXT_ID with no credential anywhere"; }
heavy_wire_re_after "anon" "POST /proxy/$REG/vscode/gallery/extensionquery -> 403" \
  "the editor's lookup did not reach the registry, or was not refused"
[[ "$(credentialed_after anon)" == "0" ]] || heavy_fail "a request carried a credential the editor was never given"
heavy_log "ANON-OK (patch armed, nothing to send: 403 on the editor's own query, $(bare_requests_after anon) request(s), none with a credential)"

# ── 3. The environment variable, with the gallery named the launcher's way ──

P="$HEAVY_WORK/env"
heavy_mark "env"
editor "$P" "$GALLERY_ENV_NAME=$REGISTRY_BASE" VSX_REGISTRY_AUTH_TOKEN="$USER_TOKEN" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/env.txt" 2>&1 \
  || { cat "$HEAVY_WORK/env.txt" >&2; tail -30 "$HEAVY_WORK/server.log" >&2; heavy_fail "$EDITOR_LABEL could not install $EXT_ID with VSX_REGISTRY_AUTH_TOKEN and $GALLERY_ENV_NAME set"; }
grep -q "successfully installed" "$HEAVY_WORK/env.txt" || { cat "$HEAVY_WORK/env.txt" >&2; heavy_fail "no 'successfully installed' for $EXT_ID"; }
editor "$P" -- --list-extensions 2>/dev/null | grep -qix "$EXT_ID" || heavy_fail "$EXT_ID not listed after the install"
heavy_wire_re_after "env" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer" \
  "the editor's lookup did not carry a Bearer"
heavy_wire_re_after "env" "GET /proxy/$REG/vscode/(asset|gallery)/.* -> 200 .*Authorization: Bearer" \
  "the package was not fetched with a Bearer"
BARE="$(bare_requests_after env)"
[[ "$BARE" == "0" ]] || { grep "/proxy/" "$HEAVY_LOG" | tail -20 >&2; heavy_fail "$BARE request(s) reached the registry without a credential"; }
heavy_log "ENV-OK ($EXT_ID installed by id; $(credentialed_after env) registry requests, every one with a Bearer, from VSX_REGISTRY_AUTH_TOKEN scoped by $GALLERY_ENV_NAME)"

# The variable without the gallery named: the patch has no origin to scope
# it to, so it must send nothing rather than send it everywhere.
P="$HEAVY_WORK/env-unscoped"
heavy_mark "env-unscoped"
if editor "$P" VSX_REGISTRY_AUTH_TOKEN="$USER_TOKEN" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/env-unscoped.txt" 2>&1; then RC=0; else RC=$?; fi
[[ $RC -ne 0 ]] || heavy_fail "VSX_REGISTRY_AUTH_TOKEN was sent with no gallery to scope it to"
[[ "$(credentialed_after env-unscoped)" == "0" ]] || heavy_fail "an unscoped variable was attached to a request"
heavy_log "ENV-SCOPE-OK (the variable alone, no gallery named: nothing sent)"

# ── 4. The contract file, at the default path, keyed by the origin ──────────

P="$HEAVY_WORK/file"
mkdir -p "$P/home/state"
CONTRACT="$P/home/state/vsx-token.json"
heavy_log "auth write-token-file for $REGISTRY_BASE → $CONTRACT"
BATLEHUB_TOKEN="$USER_TOKEN" "$CLI" --server "$REGISTRY_BASE" auth write-token-file --path "$CONTRACT" \
  >"$HEAVY_WORK/write-token.txt" 2>&1 || { cat "$HEAVY_WORK/write-token.txt" >&2; heavy_fail "auth write-token-file failed"; }
python3 - "$CONTRACT" "$GALLERY_URL" <<'PY' || heavy_fail "the contract file is not keyed by the gallery's origin with a literal token, or is not private"
import json, os, stat, sys
from urllib.parse import urlsplit
path, gallery = sys.argv[1:]
u = urlsplit(gallery)
origin = f"{u.scheme}://{u.netloc}"
assert stat.S_IMODE(os.stat(path).st_mode) == 0o600, oct(os.stat(path).st_mode)
d = json.load(open(path))
entry = d["registries"].get(origin) or d["registries"].get(origin + "/")
assert entry, (origin, list(d["registries"]))
assert isinstance(entry.get("token"), str) and entry["token"], {k: v for k, v in entry.items() if k != "token"}
PY
heavy_mark "file"
editor "$P" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/file.txt" 2>&1 \
  || { cat "$HEAVY_WORK/file.txt" >&2; heavy_fail "$EDITOR_LABEL could not install $EXT_ID from the contract file alone"; }
grep -q "successfully installed" "$HEAVY_WORK/file.txt" || { cat "$HEAVY_WORK/file.txt" >&2; heavy_fail "no 'successfully installed' for $EXT_ID"; }
heavy_wire_re_after "file" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer" \
  "the editor's lookup did not carry the credential read from the contract file"
BARE="$(bare_requests_after file)"
[[ "$BARE" == "0" ]] || heavy_fail "$BARE request(s) reached the registry without a credential"
heavy_log "FILE-OK (no variable set: the file the CLI wrote, at the patch's default path, keyed by the origin, authenticated $(credentialed_after file) requests)"

# ── 5. Resolution order: the file wins over a wrong variable ────────────────

P2="$HEAVY_WORK/precedence"
mkdir -p "$P2/home/state"
cp "$CONTRACT" "$P2/home/state/vsx-token.json"
heavy_mark "precedence"
editor "$P2" "$GALLERY_ENV_NAME=$REGISTRY_BASE" VSX_REGISTRY_AUTH_TOKEN="not-a-credential" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/precedence.txt" 2>&1 \
  || { cat "$HEAVY_WORK/precedence.txt" >&2; heavy_fail "with a good file and a wrong variable, the install failed — the variable won"; }
[[ "$(bare_requests_after precedence)" == "0" ]] || heavy_fail "a request reached the registry without a credential"
heavy_wire_re_after "precedence" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer"
heavy_log "PRECEDENCE-OK (a wrong VSX_REGISTRY_AUTH_TOKEN beside a good file: the file was sent, 200)"

# ── 6. A token source is not a credential to the patch ──────────────────────

P3="$HEAVY_WORK/source"
mkdir -p "$P3/home/state"
SOURCE_CONTRACT="$P3/home/state/vsx-token.json"
printf '%s' "$USER_TOKEN" >"$HEAVY_WORK/token-file"
"$CLI" --server "$REGISTRY_BASE" auth write-token-file --from-file "$HEAVY_WORK/token-file" --path "$SOURCE_CONTRACT" \
  >"$HEAVY_WORK/write-source.txt" 2>&1 || { cat "$HEAVY_WORK/write-source.txt" >&2; heavy_fail "auth write-token-file --from-file failed"; }
python3 - "$SOURCE_CONTRACT" <<'PY' || heavy_fail "--from-file did not write a token source entry"
import json, sys
d = json.load(open(sys.argv[1]))
entry = next(iter(d["registries"].values()))
assert not isinstance(entry.get("token"), str), entry
PY
heavy_mark "source-anon"
if editor "$P3" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/source-anon.txt" 2>&1; then RC=0; else RC=$?; fi
[[ $RC -ne 0 ]] || { cat "$HEAVY_WORK/source-anon.txt" >&2; heavy_fail "the editor resolved a token *source* — the patch must not"; }
heavy_wire_re_after "source-anon" "POST /proxy/$REG/vscode/gallery/extensionquery -> 403"
[[ "$(credentialed_after source-anon)" == "0" ]] || heavy_fail "a source entry produced a credential"
heavy_mark "source-env"
editor "$P3" "$GALLERY_ENV_NAME=$REGISTRY_BASE" VSX_REGISTRY_AUTH_TOKEN="$USER_TOKEN" -- --install-extension "$EXT_ID" >"$HEAVY_WORK/source-env.txt" 2>&1 \
  || { cat "$HEAVY_WORK/source-env.txt" >&2; heavy_fail "the fall-through to VSX_REGISTRY_AUTH_TOKEN did not happen"; }
heavy_wire_re_after "source-env" "POST /proxy/$REG/vscode/gallery/extensionquery -> 200 .*Authorization: Bearer"
heavy_log "SOURCE-OK (a source entry: refused without the variable, installed with it — the fall-through, on the editor)"

# ── 7. The credential is in no output ───────────────────────────────────────

if grep -l -- "$USER_TOKEN" "$HEAVY_WORK"/*.txt 2>/dev/null | grep -v "write-token\|write-source"; then
  heavy_fail "the credential appears in the editor's output"
fi
heavy_log "REDACTION-OK (the credential is in none of the editor's output)"

heavy_log "Measurement: $EDITOR_LABEL with patches/che-code loaded at the process boundary, gallery $GALLERY_URL named by $GALLERY_ENV_NAME"
heavy_log "  no credential: 403 on the editor's own query, 0 credentialed requests; the variable alone: nothing sent"
heavy_log "  variable / file / file-over-variable / source-then-variable: installed by id, $(grep -c 'Authorization: Bearer' "$HEAVY_LOG") registry requests with a Bearer in the run"
