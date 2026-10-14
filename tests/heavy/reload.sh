#!/usr/bin/env bash
# Heavy hot-reload test — RFC 0037 §6.6.
#
# `partial` is a claim about what a component built once at startup does after
# a reload, and only a running server can show it. One server watching a copy
# of config.reload.toml; one mock npm upstream behind two taps, so each tap's
# transcript is "the requests that reached upstream one" and "… upstream two".
#
#   1. `database.url` pointed at a host that does not resolve: the watcher logs
#      one WARN naming it, the pending diff lists it under `restart_required`,
#      and the old debug line "produced no change, nothing pending" is gone.
#   2. The registry's upstream moved to upstream two: the diff lists it under
#      `partially_applied` and `changed_registries`, and a cold `npm install`
#      exits 0 with its packument and tarball reaching upstream two only.
#   3. Warming that registry reaches upstream **one**: the warming service kept
#      its startup client. That is what makes the row `partial`; the day warming
#      follows the reload this turns red, and the row moves to `hot`.
#   4. `search.readmes` flipped: listed under `applied`, no request reaches
#      either upstream, and `restart_required` still lists step 1's
#      `database.url`. An `npm install` of a package nothing cached yet exits
#      0, which it could not if the process had followed the new URL.
#
# Run via `task test:reload-heavy` or directly. Needs DATABASE_URL and npm.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init reload 8190 8191
heavy_need npm "nodejs"
heavy_need python3 "python3"

MOCK_PORT="${RELOAD_MOCK_PORT:-8192}"
UP1_PORT="${RELOAD_UP1_PORT:-8193}"
UP2_PORT="${RELOAD_UP2_PORT:-8194}"
UP1_LOG="$HEAVY_WORK/upstream-1.log"
UP2_LOG="$HEAVY_WORK/upstream-2.log"
: > "$UP1_LOG"
: > "$UP2_LOG"
REG="npm-$HEAVY_RUN"
CONFIG="$HEAVY_WORK/config.toml"

python3 tests/heavy/db_calls.py mock "$MOCK_PORT" &
HEAVY_EXTRA_PIDS+=($!)
# Two taps, one mock: `Host` passes through, so each packument's tarball URL
# names the tap it came through and the server's same-origin check holds.
python3 tests/heavy/http_tap.py "$UP1_LOG" "$UP1_PORT" "$MOCK_PORT" >"$HEAVY_WORK/up1.err" 2>&1 &
HEAVY_EXTRA_PIDS+=($!)
python3 tests/heavy/http_tap.py "$UP2_LOG" "$UP2_PORT" "$MOCK_PORT" >"$HEAVY_WORK/up2.err" 2>&1 &
HEAVY_EXTRA_PIDS+=($!)

export RELOAD_UPSTREAM_PORT="$UP1_PORT"
# The run id written in, not left as `${HEAVY_RUN}`: a placeholder in a
# registry's `name` is one merge_layers refuses (RFC 0037 §4.5), and every
# diff would then be `restart_required: ["*"]` — measured on the first run.
sed "s/\${HEAVY_RUN}/$HEAVY_RUN/" tests/heavy/config.reload.toml > "$CONFIG"
heavy_start_server "$CONFIG"
heavy_start_tap

# `--fail-with-body`: a 4xx/5xx is a failed call, so every `|| heavy_fail`
# below can fire, and the body still reaches the file for the post-mortem.
api() {  # method, path, [body]
  curl -sS --fail-with-body -X "$1" -H "Authorization: Bearer $ADMIN_TOKEN" \
    ${3:+-H "Content-Type: application/json" -d "$3"} "$HEAVY_BASE$2"
}

# Replace the watched file in one rename, as an editor's atomic save does.
edit_config() {  # sed expression
  sed "$1" "$CONFIG" > "$CONFIG.next"
  cmp -s "$CONFIG" "$CONFIG.next" && heavy_fail "edit '$1' changed nothing in the config"
  mv "$CONFIG.next" "$CONFIG"
}

# The watcher stages a pending reload; wait for one whose diff names `$1`.
wait_pending() {  # expected path
  for _ in $(seq 1 30); do
    api GET /api/v1/admin/config/pending > "$HEAVY_WORK/pending.json" 2>/dev/null || true
    grep -qF "\"$1\"" "$HEAVY_WORK/pending.json" && return 0
    sleep 1
  done
  cat "$HEAVY_WORK/pending.json" >&2
  heavy_fail "no pending reload naming '$1' within 30 s"
}

diff_list() {  # file, list → one path per line
  python3 -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))["diff"][sys.argv[2]]))' "$1" "$2"
}

apply_pending() {
  api POST /api/v1/admin/config/pending/apply > "$HEAVY_WORK/applied.json" \
    || heavy_fail "applying the pending reload failed"
}

export NPM_CONFIG_REGISTRY="$HEAVY_TAP_BASE/proxy/$REG/"
export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false

npm_install() {  # package, dir
  mkdir -p "$2"
  (cd "$2" && echo '{}' > package.json \
    && NPM_CONFIG_CACHE="$2/.npm" npm install "$1" --no-package-lock) \
    || heavy_fail "npm install $1 failed"
}

# ── 1. A restart-only key: reported, at warn, and never "no change" ──────────

heavy_log "1. database.url -> a host that does not resolve"
edit_config 's|^url = "${DATABASE_URL}"$|url = "postgresql://batlehub@db.invalid/batlehub"|'
wait_pending "database.url"
diff_list "$HEAVY_WORK/pending.json" restart_required | grep -qx "database.url" \
  || heavy_fail "database.url is not under restart_required"
grep -q "config change needs a restart to take effect.*database.url" "$HEAVY_WORK/server.log" \
  || heavy_fail "the watcher did not log the restart owed at WARN"
[[ "$(grep -c "config change needs a restart" "$HEAVY_WORK/server.log")" == 1 ]] \
  || heavy_fail "expected exactly one restart WARN for one edit"
# The "no change" line is at debug, below this server's filter; the INFO line
# is what the watcher logs only when `is_noop()` is false.
grep -q "pending reload ready" "$HEAVY_WORK/server.log" \
  || heavy_fail "the watcher took the change for nothing (is_noop)"
apply_pending

# ── 2. A partial key: requests follow it at once ─────────────────────────────

heavy_log "2. registries[$REG].upstreams -> upstream two"
edit_config "s|\${RELOAD_UPSTREAM_PORT}|$UP2_PORT|"
wait_pending "registries[$REG].upstreams"
diff_list "$HEAVY_WORK/pending.json" partially_applied | grep -qxF "registries[$REG].upstreams" \
  || heavy_fail "the upstream change is not under partially_applied"
python3 - "$HEAVY_WORK/pending.json" "$REG" <<'PY' || heavy_fail "changed_registries does not name the upstream change"
import json, sys
diff = json.load(open(sys.argv[1]))["diff"]
assert any(c["name"] == sys.argv[2] and "upstreams" in c["fields"] for c in diff["changed_registries"]), diff
PY
apply_pending

PKG_A="reload-a-$HEAVY_RUN"
heavy_log "npm install $PKG_A (cold) through the reloaded registry"
npm_install "$PKG_A" "$HEAVY_WORK/install-a"
grep -q "GET /npm/$PKG_A -> 200" "$UP2_LOG" || heavy_fail "the packument did not reach upstream two"
grep -q "GET /npm/$PKG_A/-/$PKG_A-1.0.0.tgz -> 200" "$UP2_LOG" \
  || heavy_fail "the tarball did not reach upstream two"
grep -q "$PKG_A" "$UP1_LOG" && heavy_fail "a request for $PKG_A reached upstream one"

# ── 3. …and warming does not: the startup client ─────────────────────────────

PKG_B="reload-b-$HEAVY_RUN"
heavy_log "3. warm $PKG_B — the warming service still holds the startup client"
api POST "/api/v1/admin/registries/$REG/warm" "{\"package\":\"$PKG_B\"}" > "$HEAVY_WORK/warm.json" \
  || heavy_fail "the warm call failed"
grep -q "GET /npm/$PKG_B" "$UP1_LOG" \
  || { cat "$HEAVY_WORK/warm.json" >&2; heavy_fail "warming did not reach upstream one — if it now follows the reload, move registries[*] to hot"; }
grep -q "$PKG_B" "$UP2_LOG" && heavy_fail "warming reached upstream two: the row is no longer partial"

# ── 4. A hot key, and the restart still owed ─────────────────────────────────

heavy_log "4. search.readmes -> true"
UP1_BEFORE="$(wc -l < "$UP1_LOG")"
UP2_BEFORE="$(wc -l < "$UP2_LOG")"
edit_config 's|^readmes = false$|readmes = true|'
wait_pending "search.readmes"
diff_list "$HEAVY_WORK/pending.json" applied | grep -qx "search.readmes" \
  || heavy_fail "search.readmes is not under applied"
diff_list "$HEAVY_WORK/pending.json" restart_required | grep -qx "database.url" \
  || heavy_fail "the database.url restart owed since step 1 was forgotten"
apply_pending
diff_list "$HEAVY_WORK/applied.json" applied | grep -qx "search.readmes" \
  || heavy_fail "the applied reload does not name search.readmes"
[[ "$(wc -l < "$UP1_LOG")" == "$UP1_BEFORE" && "$(wc -l < "$UP2_LOG")" == "$UP2_BEFORE" ]] \
  || heavy_fail "a search-only reload sent requests upstream"

PKG_C="reload-c-$HEAVY_RUN"
heavy_log "npm install $PKG_C — the process still talks to the database it started on"
npm_install "$PKG_C" "$HEAVY_WORK/install-c"

cat "$UP1_LOG" "$UP2_LOG" | sed "s/^/[upstream] /"
heavy_done "reload: all four RFC 0037 §6.6 cases held"
