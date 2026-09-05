#!/usr/bin/env bash
# Heavy upstream-audit integration test — RFC 0014 from the receiving end.
#
# The audit's in-process tests prove the state machine and, with a `mockito`
# receiver, that a confirmation is dispatched through the subscription
# filter. What none of them can prove is the sentence the RFC opens with:
# that when a package leaves its upstream, *somebody is told*. That takes a
# real upstream that really loses a file, a real sweep that really notices,
# and a real HTTP delivery to a process this codebase did not write.
#
# The upstream is a directory this suite serves with `python3 -m http.server`
# as an npm registry — a packument file and a tarball — because a
# disappearance cannot be staged against npmjs.org. The receiver is
# `webhook_sink.py`, the tap pattern applied to the other end of the wire.
# The probe is driven through `POST /api/v1/admin/upstream/recheck` (RFC 0014
# §4.6) rather than waited for: the sweep interval has a five-minute floor
# and a confirmation takes two, and `recheck` runs the same ladder and state
# machine on demand.
#
# What it proves, in order:
#
#   1. **The cache is seeded by a real client.** `npm install` through the
#      proxy; the packument and the tarball are fetched from the served
#      directory and the artifact is in the inventory the sweep reads.
#   2. **A disappearance is confirmed, not believed.** The files are removed.
#      The first probe records a miss and tells nobody; the second confirms.
#   3. **The receiver got one `package_disappeared_upstream`**, with the
#      coordinate, the actor `system:upstream-audit`, the policy and the
#      counts of RFC 0014 §4.5 — asserted on the receiver's own log.
#   4. **A reappearance is reported too.** The files are restored; one
#      probe clears the row and the receiver gets `package_reappeared_upstream`.
#   5. **The block arm reaches a client.** (RFC 0014 phase 6.) Under
#      `on_confirmed = "block"`, a confirmed disappearance is refused: a fresh
#      `npm install` is answered by the packument that no longer lists the
#      version, and the tarball a lockfile pins is `403`. Not `get_status` —
#      the registry's own protocol endpoint, as RFC 0006 requires.
#   6. **…and the unblock does.** After the restore and the probe, the same
#      installs succeed again: the audit lifted its own block.
#
# Run via `task test:upstream-audit-heavy` or directly. With `COVERAGE=1` the
# server runs under `cargo llvm-cov run --no-report` (see lib.sh).
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8109),
# HEAVY_TAP_PORT (8118), HEAVY_UPSTREAM_PORT (8128), HEAVY_SINK_PORT (8138),
# COVERAGE. Needs no network: every host in this suite is 127.0.0.1.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init upstream_audit 8109 8118
heavy_need npm "nodejs"
heavy_need python3 "python3"

REG="npm-audited-$HEAVY_RUN"
PKG="left-pad"
VERSION="1.3.0"
UPSTREAM_PORT="${HEAVY_UPSTREAM_PORT:-8128}"
SINK_PORT="${HEAVY_SINK_PORT:-8138}"
export HEAVY_UPSTREAM_URL="http://127.0.0.1:$UPSTREAM_PORT"
export HEAVY_WEBHOOK_URL="http://127.0.0.1:$SINK_PORT/hook"

# ── The upstream: a directory that is an npm registry ────────────────────────
#
# `GET /left-pad` is the packument, a file with no extension; the tarball
# lives under `tarballs/` because a file and a directory cannot share the
# name `left-pad`. The tarball URL in the packument is on the served origin,
# which the npm client requires (`ensure_same_origin`).
UPSTREAM_DIR="$HEAVY_WORK/upstream"
FIXTURE_DIR="$HEAVY_WORK/fixture"
mkdir -p "$FIXTURE_DIR/tarballs" "$FIXTURE_DIR/package"
cat > "$FIXTURE_DIR/package/package.json" <<EOF
{ "name": "$PKG", "version": "$VERSION", "description": "RFC 0014 heavy fixture", "license": "MIT", "main": "index.js" }
EOF
echo "module.exports = function leftPad(s) { return s; };" > "$FIXTURE_DIR/package/index.js"
tar -C "$FIXTURE_DIR" -czf "$FIXTURE_DIR/tarballs/$PKG-$VERSION.tgz" package
SHASUM="$(sha1sum "$FIXTURE_DIR/tarballs/$PKG-$VERSION.tgz" | cut -d' ' -f1)"
cat > "$FIXTURE_DIR/$PKG" <<EOF
{
  "name": "$PKG",
  "dist-tags": { "latest": "$VERSION" },
  "versions": {
    "$VERSION": {
      "name": "$PKG", "version": "$VERSION", "license": "MIT",
      "dist": { "tarball": "$HEAVY_UPSTREAM_URL/tarballs/$PKG-$VERSION.tgz", "shasum": "$SHASUM" }
    }
  },
  "time": { "created": "2018-03-01T00:00:00.000Z", "modified": "2018-03-01T00:00:00.000Z", "$VERSION": "2018-03-01T00:00:00.000Z" }
}
EOF
rm -rf "$FIXTURE_DIR/package"
cp -r "$FIXTURE_DIR" "$UPSTREAM_DIR"

python3 -m http.server "$UPSTREAM_PORT" --bind 127.0.0.1 --directory "$UPSTREAM_DIR" \
  >"$HEAVY_WORK/upstream.log" 2>&1 &
UPSTREAM_PID=$!
SINK_LOG="$HEAVY_WORK/sink.log"
: > "$SINK_LOG"
python3 tests/heavy/webhook_sink.py "$SINK_LOG" "$SINK_PORT" >"$HEAVY_WORK/sink.err" 2>&1 &
SINK_PID=$!
# lib.sh's cleanup stops the server and the tap; these two are this suite's.
stop_fixtures() { kill "$UPSTREAM_PID" "$SINK_PID" 2>/dev/null || true; }
trap 'stop_fixtures; heavy_cleanup' EXIT
for _ in $(seq 1 30); do
  curl -sf "$HEAVY_UPSTREAM_URL/$PKG" >/dev/null 2>&1 && curl -s -o /dev/null "$HEAVY_WEBHOOK_URL" && break
  sleep 1
done
curl -sf "$HEAVY_UPSTREAM_URL/$PKG" >/dev/null || heavy_fail "the served upstream never came up on $UPSTREAM_PORT"
curl -s -o /dev/null -w '' "$HEAVY_WEBHOOK_URL" || heavy_fail "the webhook receiver never came up on $SINK_PORT"
: > "$SINK_LOG"  # the readiness GET above is not a delivery

heavy_start_server tests/heavy/config.upstream-audit.toml
heavy_start_tap

REG_URL="$HEAVY_TAP_BASE/proxy/$REG/"
NPMRC="$HEAVY_WORK/npmrc"
cat > "$NPMRC" <<EOF
registry=$REG_URL
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$REG/:_authToken=$ADMIN_TOKEN
EOF
export NPM_CONFIG_USERCONFIG="$NPMRC"
export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache"

heavy_log "npm $(npm --version), node $(node --version)"

admin_post() {  # path, json → body on stdout, status in ADMIN_CODE
  ADMIN_CODE="$(curl -sS -o "$HEAVY_WORK/admin.json" -w '%{http_code}' -X POST "$HEAVY_BASE$1" \
    -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" -d "$2")"
  cat "$HEAVY_WORK/admin.json"
}

recheck() {  # → the response JSON on stdout
  admin_post /api/v1/admin/upstream/recheck "{\"registry\":\"$REG\",\"package_name\":\"$PKG\"}"
  [[ "$ADMIN_CODE" == "200" ]] || { cat "$HEAVY_WORK/admin.json" >&2; heavy_fail "recheck answered $ADMIN_CODE"; }
}

# sink_wait <event_type> <count> — the receiver's log holds exactly <count>
# deliveries of that type within 30 s; echoes the last one's body.
sink_wait() {
  local kind="$1" want="$2" n=0
  for _ in $(seq 1 30); do
    n="$(grep -c "\"event_type\":\"$kind\"" "$SINK_LOG" || true)"
    [[ "$n" -ge "$want" ]] && break
    sleep 1
  done
  [[ "$n" == "$want" ]] || { cat "$SINK_LOG" >&2; heavy_fail "expected $want delivery(ies) of $kind at the receiver, saw $n"; }
  grep "\"event_type\":\"$kind\"" "$SINK_LOG" | tail -1 | cut -d' ' -f3-
}

# ── 0. Subscribe, the way an operator does ───────────────────────────────────

heavy_log "Subscribing the webhook to the audit's events"
admin_post /api/v1/admin/notifications/subscriptions \
  "{\"registry\":\"$REG\",\"package_name\":null,\"event_types\":[\"package_disappeared_upstream\",\"package_reappeared_upstream\",\"upstream_unreachable\"],\"channel_name\":\"sink\"}" \
  >"$HEAVY_WORK/subscription.json"
[[ "$ADMIN_CODE" == "200" || "$ADMIN_CODE" == "201" ]] \
  || { cat "$HEAVY_WORK/subscription.json" >&2; heavy_fail "creating the subscription answered $ADMIN_CODE"; }

# ── 1. Seed the cache with a real install ────────────────────────────────────

CONSUMER="$HEAVY_WORK/consumer"
mkdir -p "$CONSUMER"
echo '{ "name": "consumer", "version": "1.0.0", "private": true }' > "$CONSUMER/package.json"
heavy_mark "seed"
heavy_log "npm install $PKG@$VERSION through the proxy"
(cd "$CONSUMER" && npm install "$PKG@$VERSION" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-1.out" 2>"$HEAVY_WORK/install-1.err" \
  || { cat "$HEAVY_WORK/install-1.err" >&2; heavy_fail "npm install through the served upstream failed"; }
heavy_wire_after "seed" "GET /proxy/$REG/$PKG -> 200" "the packument was not fetched through the proxy"
heavy_wire_after "seed" "GET /proxy/$REG/$PKG/$VERSION/tarball -> 200" "the tarball was not fetched through the proxy"
grep -q "GET /$PKG " "$HEAVY_WORK/upstream.log" \
  || { cat "$HEAVY_WORK/upstream.log" >&2; heavy_fail "the served upstream was never asked for the packument"; }
heavy_log "SEED-OK ($PKG@$VERSION cached from the served directory)"

# ── 2. The upstream loses it; two probes confirm ─────────────────────────────

heavy_log "Removing $PKG from the served directory"
rm -f "$UPSTREAM_DIR/$PKG" "$UPSTREAM_DIR/tarballs/$PKG-$VERSION.tgz"
curl -s -o /dev/null -w '%{http_code}\n' "$HEAVY_UPSTREAM_URL/$PKG" | grep -q '^404$' \
  || heavy_fail "the served upstream still answers for $PKG after the rm"

heavy_log "recheck #1 — a miss, recorded, nobody told"
FIRST="$(recheck)"
python3 - "$FIRST" <<'PY' || { echo "$FIRST" >&2; heavy_fail "the first probe did not record a single unconfirmed miss"; }
import json, sys
r = json.loads(sys.argv[1])
ok = r["probed"] == 1 and r["missing"] == 1 and r["transitions"] == [] \
    and r["status"] is not None and r["status"]["state"] == "missing" and r["status"]["consecutive_misses"] == 1
sys.exit(0 if ok else 1)
PY
sleep 2
if grep -q "package_disappeared_upstream" "$SINK_LOG"; then
  cat "$SINK_LOG" >&2
  heavy_fail "one miss was reported as a disappearance — the confirmation window did not hold"
fi

heavy_log "recheck #2 — confirmed"
SECOND="$(recheck)"
python3 - "$SECOND" <<'PY' || { echo "$SECOND" >&2; heavy_fail "the second probe did not confirm the disappearance"; }
import json, sys
r = json.loads(sys.argv[1])
ok = r["transitions"] == ["confirmed"] and r["status"]["state"] == "disappeared" \
    and r["status"]["version"] is None and r["status"]["confirmed_at"] is not None
sys.exit(0 if ok else 1)
PY
heavy_log "CONFIRM-OK (missing after one probe, disappeared after two)"

# ── 3. The receiver was told, once, with the payload ─────────────────────────

heavy_log "Waiting for package_disappeared_upstream at the receiver"
EVENT="$(sink_wait package_disappeared_upstream 1)"
python3 - "$EVENT" "$REG" "$PKG" "$VERSION" <<'PY' || { echo "$EVENT" >&2; heavy_fail "the delivered event does not carry RFC 0014 §4.5's payload"; }
import json, sys
e = json.loads(sys.argv[1]); reg, pkg, ver = sys.argv[2:5]
m = e["metadata"]
checks = [
    e["event_type"] == "package_disappeared_upstream",
    e["registry"] == reg,
    e["package_name"] == pkg,
    e["version"] is None,                       # the whole package went: one event, not one per version
    e["actor"] == "system:upstream-audit",
    m["versions"] == [ver],
    m["consecutive_misses"] == 2,
    m["probe"] == "package",
    m["policy"] == "block",
    m["blocked"] is True,
    m["held_from_eviction"] is True,
    isinstance(m["first_missed_at"], str) and isinstance(m["confirmed_at"], str),
    m["sweep"]["probed"] == 1 and m["sweep"]["missing"] == 1,
]
for i, c in enumerate(checks):
    if not c:
        print(f"check {i} failed", file=sys.stderr)
sys.exit(0 if all(checks) else 1)
PY
grep -q "upstream_unreachable" "$SINK_LOG" \
  && { cat "$SINK_LOG" >&2; heavy_fail "a one-package registry was reported unreachable — the ratio gate fired under the population floor"; }
heavy_log "NOTIFY-OK (one package_disappeared_upstream delivered, payload complete)"

# ── 4. It comes back, and so does the news ───────────────────────────────────

heavy_log "Restoring $PKG to the served directory"
cp "$FIXTURE_DIR/$PKG" "$UPSTREAM_DIR/$PKG"
cp "$FIXTURE_DIR/tarballs/$PKG-$VERSION.tgz" "$UPSTREAM_DIR/tarballs/"
THIRD="$(recheck)"
python3 - "$THIRD" <<'PY' || { echo "$THIRD" >&2; heavy_fail "the probe after the restore did not clear the row"; }
import json, sys
r = json.loads(sys.argv[1])
sys.exit(0 if r["transitions"] == ["reappeared"] and r["status"] is None and r["missing"] == 0 else 1)
PY
EVENT="$(sink_wait package_reappeared_upstream 1)"
python3 - "$EVENT" "$REG" "$PKG" <<'PY' || { echo "$EVENT" >&2; heavy_fail "the reappearance event is not RFC 0014 §4.5's"; }
import json, sys
e = json.loads(sys.argv[1])
ok = e["registry"] == sys.argv[2] and e["package_name"] == sys.argv[3] and e["version"] is None \
    and e["actor"] == "system:upstream-audit" and e["metadata"]["unblocked"] is True
sys.exit(0 if ok else 1)
PY
DELIVERIES="$(grep -c '^POST /hook ' "$SINK_LOG" || true)"
[[ "$DELIVERIES" == "2" ]] || { cat "$SINK_LOG" >&2; heavy_fail "the receiver saw $DELIVERIES deliveries; expected exactly the two transitions"; }
heavy_log "REAPPEAR-OK (row cleared, package_reappeared_upstream delivered with unblocked=true; 2 deliveries in all)"

# ── 5 and 6. The block reached a client, and so did the unblock ──────────────
#
# Steps 2–4 ran under "block" too, so the row that was confirmed above was
# blocked between the two probes and unblocked by the restore. The
# assertions that matter are the client's, so the whole cycle is run once
# more with npm in front of it: refused while blocked, served after.

heavy_log "Removing $PKG again; two probes confirm and block"
rm -f "$UPSTREAM_DIR/$PKG" "$UPSTREAM_DIR/tarballs/$PKG-$VERSION.tgz"
recheck >/dev/null
BLOCKED="$(recheck)"
python3 - "$BLOCKED" <<'PY' || { echo "$BLOCKED" >&2; heavy_fail "the second cycle did not confirm"; }
import json, sys
r = json.loads(sys.argv[1])
sys.exit(0 if r["transitions"] == ["confirmed"] else 1)
PY
sink_wait package_disappeared_upstream 2 >/dev/null

# A fresh resolve: the packument no longer names the version, and npm says
# so in its own words.
BLOCKED_CONSUMER="$HEAVY_WORK/consumer-blocked"
mkdir -p "$BLOCKED_CONSUMER"
echo '{ "name": "consumer", "version": "1.0.0", "private": true }' > "$BLOCKED_CONSUMER/package.json"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-blocked"
heavy_mark "blocked-resolve"
heavy_log "npm install $PKG@$VERSION while blocked — a fresh resolve"
set +e
(cd "$BLOCKED_CONSUMER" && npm install "$PKG@$VERSION" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-blocked.out" 2>"$HEAVY_WORK/install-blocked.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm installed a version the audit had blocked"
# The upstream has no packument any more either, so npm meets one of two
# refusals: the pinned metadata with the version filtered out (ETARGET) or,
# once the pin has lapsed, the upstream's own 404. Both stop the install;
# neither hands out the tarball, and the tarball is what the block guards.
grep -q "ETARGET\|E404" "$HEAVY_WORK/install-blocked.err" \
  || { cat "$HEAVY_WORK/install-blocked.err" >&2; heavy_fail "npm failed for a reason that is neither the filtered packument nor the vanished upstream"; }
if heavy_wire_seen_after "blocked-resolve" "GET /proxy/$REG/$PKG/$VERSION/tarball"; then
  heavy_fail "a fresh resolve asked for the blocked tarball — the packument still lists it"
fi
PACKUMENT_CODE="$(curl -sS -o "$HEAVY_WORK/packument-blocked.json" -w '%{http_code}' -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_BASE/proxy/$REG/$PKG")"
if [[ "$PACKUMENT_CODE" == "200" ]]; then
  python3 - "$HEAVY_WORK/packument-blocked.json" "$VERSION" <<'PY' || { cat "$HEAVY_WORK/packument-blocked.json" >&2; heavy_fail "the served packument still lists the blocked version"; }
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if sys.argv[2] not in d.get("versions", {}) else 1)
PY
fi
# A pinned resolve: the tarball itself is refused.
PINNED="$HEAVY_WORK/consumer-blocked-pinned"
mkdir -p "$PINNED"
cat > "$PINNED/package.json" <<EOF
{ "name": "consumer", "version": "1.0.0", "private": true, "dependencies": { "$PKG": "$VERSION" } }
EOF
cat > "$PINNED/package-lock.json" <<EOF
{
  "name": "consumer", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0", "dependencies": { "$PKG": "$VERSION" } },
    "node_modules/$PKG": { "version": "$VERSION", "resolved": "$REG_URL$PKG/$VERSION/tarball", "license": "MIT" }
  }
}
EOF
heavy_mark "blocked-pinned"
heavy_log "npm ci with a lockfile pinning $PKG@$VERSION while blocked — the download gate"
set +e
(cd "$PINNED" && npm ci --registry "$REG_URL") >"$HEAVY_WORK/ci-blocked.out" 2>"$HEAVY_WORK/ci-blocked.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm ci installed a version the audit had blocked"
heavy_wire_after "blocked-pinned" "GET /proxy/$REG/$PKG/$VERSION/tarball -> 403" \
  "the pinned tarball was not refused with 403 while blocked"
heavy_log "BLOCK-OK (fresh resolve: ETARGET, no tarball request; pinned: 403)"

heavy_log "Restoring $PKG; one probe clears the row and lifts the block"
cp "$FIXTURE_DIR/$PKG" "$UPSTREAM_DIR/$PKG"
cp "$FIXTURE_DIR/tarballs/$PKG-$VERSION.tgz" "$UPSTREAM_DIR/tarballs/"
LIFTED="$(recheck)"
python3 - "$LIFTED" <<'PY' || { echo "$LIFTED" >&2; heavy_fail "the probe after the restore did not clear the row"; }
import json, sys
r = json.loads(sys.argv[1])
sys.exit(0 if r["transitions"] == ["reappeared"] and r["status"] is None else 1)
PY
EVENT="$(sink_wait package_reappeared_upstream 2)"
python3 - "$EVENT" <<'PY' || { echo "$EVENT" >&2; heavy_fail "the reappearance event does not say the block was lifted"; }
import json, sys
e = json.loads(sys.argv[1])
sys.exit(0 if e["metadata"]["unblocked"] is True and "unblock_skipped_reason" not in e["metadata"] else 1)
PY
heavy_mark "unblocked"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-unblocked"
(cd "$BLOCKED_CONSUMER" && npm install "$PKG@$VERSION" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-unblocked.out" 2>"$HEAVY_WORK/install-unblocked.err" \
  || { cat "$HEAVY_WORK/install-unblocked.err" >&2; heavy_fail "npm install failed after the audit lifted its block"; }
heavy_wire_after "unblocked" "GET /proxy/$REG/$PKG/$VERSION/tarball -> 200" \
  "the tarball was not served after the unblock"
(cd "$PINNED" && npm ci --registry "$REG_URL") >"$HEAVY_WORK/ci-unblocked.out" 2>"$HEAVY_WORK/ci-unblocked.err" \
  || { cat "$HEAVY_WORK/ci-unblocked.err" >&2; heavy_fail "npm ci failed after the audit lifted its block"; }
heavy_log "UNBLOCK-OK (fresh resolve and pinned install both served again)"

heavy_done UPSTREAM-AUDIT-HEAVY-OK
