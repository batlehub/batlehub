#!/usr/bin/env bash
# Heavy RFC 0007-bis §11 q3 suite — the **Fetch** button on the catalogue's
# upstream rows, pressed in a real browser against a real BatleHub.
#
# The unit tests prove the endpoint and the component. Neither proves the thing
# this page's own rule asks for: that a person looking at the console can search
# for a package this instance holds nothing of, press one button, and have the
# bytes arrive. That is three parties agreeing — the built SPA, the API and the
# upstream — and each of the three has broken this once already.
#
# The console and the API are **one origin** here, served by the same process
# from `static_dir`. That is the deployed shape, and it keeps the suite from
# proving anything through a CORS arrangement no real deployment uses.
#
# What it proves, in order:
#
#   1. **Anonymous, the row is there and the button is not.** The registry is
#      open to an anonymous reader, so the catalogue lists the upstream hit —
#      and offers no fetch, because pulling is an authenticated act (§11 q9).
#      A registry the reader could not see would pass this for the wrong
#      reason, which is why `anonymous` holds the read verbs.
#   2. **Signed in, the button names a version.** Not "Fetch": the label
#      carries the version the upstream search returned, which is the whole
#      answer to "the listing has no version".
#   3. **Pressing it fetches that version.** Clicked by its *accessible* name,
#      so the control the suite presses is the one a keyboard user reaches.
#      The row then leaves the upstream half of the table.
#   4. **The bytes are really here.** The version the button named is held
#      afterwards — asked of the API, and read back through the protocol a
#      package manager would use.
#   5. **It went through the front door.** The tap saw the console's
#      `POST /api/v1/explore/packages/…/fetch`, and the audit names the reader
#      who pressed it, not an administrator.
#
# Ports: 8125 (server), 8133 (tap). Needs the network for one npm search and
# one tarball.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT, HEAVY_TAP_PORT,
# SEARCH (default `left-pad`), CDP_URL (default: http://127.0.0.1:9222 if it
# answers), CHROME_BIN, COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init console_fetch 8125 8133
heavy_need python3 "python3"
heavy_need node "nodejs"
heavy_need curl "curl"

REG="npm-$HEAVY_RUN"
SEARCH="${SEARCH:-left-pad}"
USER_TOKEN="heavy-user-token"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SHOTS="$HEAVY_WORK/shots"
mkdir -p "$SHOTS"

# ── 0. The browser and the built console ────────────────────────────────────

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
  [[ -n "$CHROME" ]] || heavy_fail "no browser: set CDP_URL to a running Chrome or CHROME_BIN to a binary"
  BROWSER_ARGS=(--chrome "$CHROME")
fi

# **`VITE_API_BASE_URL` is set empty, and set explicitly.** An absent value is
# not the same as an empty one here: the repo root carries a `.env`, Vite
# prefers `process.env` over its own files, and an inherited base would point
# the built console at whatever front the last dev session was using — which
# answers, so the failure would look like a bug in this suite's assertions
# rather than a misdirected console.
heavy_log "Building the console with an empty API base (same origin as the API)"
( cd "$REPO/ui" && VITE_API_BASE_URL="" pnpm run build ) >"$HEAVY_WORK/ui-build.log" 2>&1 \
  || { tail -30 "$HEAVY_WORK/ui-build.log" >&2; heavy_fail "the console did not build"; }
[[ -f "$REPO/ui/dist/index.html" ]] || heavy_fail "no ui/dist/index.html after the build"
export PROXY_CACHE__SERVER__STATIC_DIR="$REPO/ui/dist"

# ── 1. The server, the tap, and the console behind both ─────────────────────

heavy_start_server tests/heavy/config.console-fetch.toml
heavy_start_tap
CONSOLE="$HEAVY_TAP_BASE"

curl -fsS "$CONSOLE/packages" -o "$HEAVY_WORK/index.html" \
  || heavy_fail "the console's own routes are not served — check static_dir"
grep -q '<div id="app"' "$HEAVY_WORK/index.html" \
  || { head -20 "$HEAVY_WORK/index.html" >&2; heavy_fail "/packages did not answer with the SPA shell"; }
heavy_log "CONSOLE-OK (the SPA and the API on one origin at $CONSOLE)"

# The upstream search has to answer, or every assertion below is about an empty
# table. Asked of the API directly first, so a network failure names itself.
heavy_mark "upstream-search"
curl -fsS -H "Authorization: Bearer $USER_TOKEN" \
  "$CONSOLE/api/v1/explore/upstream?name=$SEARCH&limit=10&registry=$REG" \
  -o "$HEAVY_WORK/upstream.json" || heavy_fail "the upstream search endpoint did not answer"
python3 - "$HEAVY_WORK/upstream.json" <<'PY' || { cat "$HEAVY_WORK/upstream.json" >&2; heavy_fail "the upstream search returned nothing fetchable"; }
import json, sys
items = json.load(open(sys.argv[1]))["items"]
assert items, "no upstream hits — the registry's search did not answer"
offered = [i for i in items if i.get("fetch", {}).get("offered")]
assert offered, ("no hit offers a fetch", items[:3])
print(f"{len(items)} upstream hit(s), {len(offered)} offering a fetch; first: {offered[0]['name']} {offered[0]['latest_version']}")
PY
heavy_log "UPSTREAM-OK (the search answers, and the rows carry the offer the listing draws from)"

# ── 2. Anonymous: the row, and no button ────────────────────────────────────

heavy_mark "anon-view"
if node tests/heavy/console_fetch.mjs --base "$CONSOLE" --token "" --search "$SEARCH" \
  --shots "$SHOTS" "${BROWSER_ARGS[@]}" \
  >"$HEAVY_WORK/anon.jsonl" 2>"$HEAVY_WORK/anon.err"; then RC=0; else RC=$?; fi
[[ $RC -eq 0 ]] || { cat "$HEAVY_WORK/anon.jsonl" "$HEAVY_WORK/anon.err" >&2; heavy_fail "the anonymous driver run failed (exit $RC)"; }
cat "$HEAVY_WORK/anon.jsonl"
python3 tests/heavy/check_console_fetch.py "$HEAVY_WORK/anon.jsonl" --anonymous \
  || { cat "$HEAVY_WORK/anon.jsonl" >&2; heavy_fail "the anonymous catalogue offered a fetch it cannot honour"; }
heavy_log "ANON-OK (the upstream row is listed and carries no Fetch button — pulling is an authenticated act)"

# ── 3. Signed in: the button, the click, and the row that changes ───────────

heavy_mark "user-view"
if node tests/heavy/console_fetch.mjs --base "$CONSOLE" --token "$USER_TOKEN" --search "$SEARCH" \
  --shots "$SHOTS" "${BROWSER_ARGS[@]}" \
  >"$HEAVY_WORK/user.jsonl" 2>"$HEAVY_WORK/user.err"; then RC=0; else RC=$?; fi
[[ $RC -eq 0 ]] || { cat "$HEAVY_WORK/user.jsonl" "$HEAVY_WORK/user.err" >&2; heavy_fail "the signed-in driver run failed (exit $RC)"; }
cat "$HEAVY_WORK/user.jsonl"
python3 tests/heavy/check_console_fetch.py "$HEAVY_WORK/user.jsonl" \
  || { cat "$HEAVY_WORK/user.jsonl" >&2; heavy_fail "the console did not fetch the version its button named"; }

FETCHED_NAME="$(python3 tests/heavy/check_console_fetch.py "$HEAVY_WORK/user.jsonl" --print name)"
FETCHED_VERSION="$(python3 tests/heavy/check_console_fetch.py "$HEAVY_WORK/user.jsonl" --print version)"
FETCHED_LABEL="$(python3 tests/heavy/check_console_fetch.py "$HEAVY_WORK/user.jsonl" --print label)"
# The name percent-encoded as *one path segment*, for the explore route, whose
# `{name}` is a single segment. `--print name` decodes on purpose (it is what
# the reader sees), and an upstream relevance search routinely answers with a
# scoped package — `left-pad` returns `@stdlib/string-left-pad`. Interpolated
# raw, such a name splits the segment and the route cannot match it: the suite
# would fail with "the package page's API did not answer" and blame the server
# for the driver's URL. npm's own tarball path below is *not* encoded, because
# a slash is how npm addresses a scope there.
FETCHED_NAME_ENC="$(python3 -c 'import sys, urllib.parse as u; print(u.quote(sys.argv[1], safe=""))' "$FETCHED_NAME")"
heavy_log "CLICK-OK (the button read '$FETCHED_LABEL'; the row for $FETCHED_NAME left the upstream half)"

# ── 4. The bytes are really here ────────────────────────────────────────────
#
# Two independent answers, because the console's own table is the thing under
# test and cannot also be the proof: the explore API, and the protocol a
# package manager speaks.

curl -fsS -H "Authorization: Bearer $USER_TOKEN" \
  "$CONSOLE/api/v1/explore/packages/$REG/$FETCHED_NAME_ENC?version=$FETCHED_VERSION" \
  -o "$HEAVY_WORK/detail.json" || heavy_fail "the package page's API did not answer for $FETCHED_NAME"
python3 - "$HEAVY_WORK/detail.json" "$FETCHED_VERSION" <<'PY' || { cat "$HEAVY_WORK/detail.json" >&2; heavy_fail "the version the button named is still not held"; }
import json, sys
doc = json.load(open(sys.argv[1])); want = sys.argv[2]
row = next((v for v in doc["versions"] if v["version"] == want), None)
assert row is not None, (want, [v["version"] for v in doc["versions"]][:5])
assert row["source"] != "upstream", ("the row still says upstream-only", row)
print(f"{doc['name']} {want}: source={row['source']}")
PY

# The tarball, through npm's own route. `source != "upstream"` is the
# catalogue's word for it; this is the bytes. The path is the one BatleHub
# writes into the packument it renders — `{name}/{version}/tarball`, not npm's
# own `{name}/-/{name}-{version}.tgz`, which is the upstream's shape and 404s
# here.
TARBALL="$FETCHED_NAME/$FETCHED_VERSION/tarball"
curl -fsS -H "Authorization: Bearer $USER_TOKEN" -o "$HEAVY_WORK/pkg.tgz" \
  "$CONSOLE/proxy/$REG/$TARBALL" || heavy_fail "the artifact does not download after the fetch"
[[ -s "$HEAVY_WORK/pkg.tgz" ]] || heavy_fail "the artifact downloaded empty"
python3 -c "import gzip,sys; gzip.open(sys.argv[1]).read(1)" "$HEAVY_WORK/pkg.tgz" \
  || heavy_fail "what came back is not a gzip tarball"
heavy_log "HELD-OK ($FETCHED_NAME $FETCHED_VERSION is held: the API says so, and $(stat -c%s "$HEAVY_WORK/pkg.tgz") bytes come back through npm's own path)"

# ── 5. Through the front door, as the reader ────────────────────────────────

heavy_wire_re_after "user-view" \
  "POST /api/v1/explore/packages/$REG/.*/fetch -> 200" \
  "the tap never saw the console's fetch — the bytes did not arrive by the route under test"

curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$CONSOLE/api/v1/admin/audit-log?registry=$REG&per_page=100" -o "$HEAVY_WORK/events.json" \
  || heavy_fail "the audit endpoint did not answer"
python3 tests/heavy/check_console_fetch_audit.py "$HEAVY_WORK/events.json" \
  "$FETCHED_NAME" "$FETCHED_VERSION" ci-user \
  || { cat "$HEAVY_WORK/events.json" >&2; heavy_fail "no audit row names the reader who pressed the button"; }
heavy_log "AUDIT-OK (the fetch is attributed to ci-user, the reader who pressed the button — not to an administrator)"

heavy_log "Measurement: the built console and the API on one origin at $CONSOLE, driven in a real browser"
heavy_log "  anonymous: the upstream row listed, no Fetch button"
heavy_log "  signed in: the button read '$FETCHED_LABEL', clicked by its accessible name"
heavy_log "  after the click: $FETCHED_NAME $FETCHED_VERSION held, and downloadable through npm's own path"

heavy_done CONSOLE-FETCH-HEAVY-OK
