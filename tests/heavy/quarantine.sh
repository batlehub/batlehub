#!/usr/bin/env bash
# Heavy quarantine integration test — RFC 0018 as a real client meets it.
#
# Every claim RFC 0018 makes about a client was, until this suite, proven
# in-process only: `crates/web/tests/security_registry.rs` drives the gate
# with a fake OSV over `FixedRegistry`, and no heavy suite had a
# `[registries.security]` section. This one puts npm in front of a quarantined
# registry backed by registry.npmjs.org and the real OSV database, with the
# worker embedded in the same process, and asserts what npm *does* — which is
# the thing phase 4's rescan will do to a version it has already installed.
#
# What it proves, in order:
#
#   1. **First contact holds.** `npm install` of a version nobody has cached:
#      the tarball request is answered `403` in npm's own error shape with
#      `SCAN_PENDING`, npm exits non-zero, and what it prints carries the
#      reason code. The *Refuse* column of RFC 0018 §4.4, measured for npm.
#   2. **The worker clears it.** `batlehub wait` returns `allowed`; the same
#      `npm install`, from the same cache, completes, and the installed
#      package.json carries the version. The *Recover* column.
#   3. **A finding refuses.** A version OSV knows to be vulnerable
#      (`minimist@1.2.5`, GHSA-xvch-5gv4-984h, critical) is refused with the
#      finding on the wire, and `batlehub why` names the same advisory.
#   4. **The listing agrees.** `npm view … versions` omits the denied version
#      and lists the served one — the *Hide* axis, RFC 0006's lesson that a
#      block the API believes and the packument does not is not a block.
#   5. **Warn mode serves with headers.** The registry flipped to `warn` by a
#      config reload: the same install succeeds and the tap sees the verdict
#      headers on the tarball.
#   6. **Egress.** A binary scanner under the worker's `bwrap` sandbox could
#      not reach the tap — observed by a scanner that *tries*, not by reading
#      the argv. Needs `bwrap` and user namespaces; on a host without them,
#      HEAVY_QUARANTINE_SKIP_SANDBOX=1 leaves this row unmeasured and says so.
#   7. **The flip.** (RFC 0018 phase 4.) A version scanned clean by an OSV
#      the suite runs, installed, then rescanned after that OSV learned an
#      advisory: the next `npm install` from a clean cache is refused, the
#      admin alert lands at a webhook receiver the suite runs and names the
#      identity that pulled it, and `batlehub verdicts pullers` names the
#      same one. The rescan is the *scheduler's* — `interval_secs = 1`, a
#      tick a minute — not an admin's request.
#
# Run via `task test:quarantine-heavy` or directly. With `COVERAGE=1` the
# server runs under `cargo llvm-cov run --no-report` (see lib.sh).
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8108),
# HEAVY_TAP_PORT (8117), HEAVY_OSV_PORT (8127), HEAVY_SINK_PORT (8137),
# HEAVY_QUARANTINE_SKIP_SANDBOX, COVERAGE. Needs network: registry.npmjs.org
# for the packages and api.osv.dev for the scanner — a fixture OSV would
# prove the mapping, not the quarantine (step 7's fake OSV serves the one
# registry whose point is a database that changes its mind).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init quarantine 8108 8117
heavy_need npm "nodejs"
heavy_need python3 "python3"

REG="npm-quarantine-$HEAVY_RUN"
EGRESS_REG="npm-egress-$HEAVY_RUN"
RESCAN_REG="npm-rescan-$HEAVY_RUN"
OSV_PORT="${HEAVY_OSV_PORT:-8127}"
SINK_PORT="${HEAVY_SINK_PORT:-8137}"
export HEAVY_OSV_URL="http://127.0.0.1:$OSV_PORT"
export HEAVY_WEBHOOK_URL="http://127.0.0.1:$SINK_PORT/hook"
# Step 7's package: dependency-free and years old; the fake OSV is the only
# database that ever judges it here.
FLIP_PKG="left-pad"
FLIP_VERSION="1.3.0"
# No dependencies, years old, and OSV's answer for each is known: 1.2.8 is
# clean, 1.2.5 carries GHSA-xvch-5gv4-984h (prototype pollution, critical —
# above the registry's `max_severity = "high"`). One package for both so
# step 4 can ask one packument which of its versions is served.
PKG="minimist"
CLEAN="1.2.8"
VULN="1.2.5"
ADVISORY="GHSA-xvch-5gv4-984h"
# Step 6's package: also dependency-free and clean, on the egress registry.
EGRESS_PKG="left-pad"
EGRESS_VERSION="1.3.0"

SKIP_SANDBOX="${HEAVY_QUARANTINE_SKIP_SANDBOX:-0}"
if [[ "$SKIP_SANDBOX" != "1" ]]; then
  heavy_need bwrap "bubblewrap — or set HEAVY_QUARANTINE_SKIP_SANDBOX=1 to leave the egress row unmeasured"
fi

# ── The egress probe: a "scanner" that tries to phone home ───────────────────
#
# The worker runs it exactly as it would run postmortem — same `bwrap` argv,
# same cleared environment — and reads its stdout as a postmortem scan
# report. Inside `--unshare-net` there is no route to 127.0.0.1, so the
# connect fails and the report is empty; anywhere else the connect succeeds,
# the tap logs `GET /egress-probe`, and the report carries a critical
# finding that denies the version. Either way the answer is on the wire and
# in the verdict, and neither is this codebase's own reading of its argv.
#
# bash's `/dev/tcp` rather than curl: the sandbox's PATH is three directories
# and nothing in them is promised but a shell.
EGRESS_PROBE="$HEAVY_WORK/egress-probe.sh"
cat > "$EGRESS_PROBE" <<EOF
#!/bin/bash
# Written by tests/heavy/quarantine.sh; runs inside the worker's sandbox.
if (exec 3<>/dev/tcp/127.0.0.1/$HEAVY_TAP_PORT && printf 'GET /egress-probe HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n' >&3 && head -c 1 <&3 >/dev/null) 2>/dev/null; then
  echo '{"findings":[{"dependency":"egress-probe","severity":"critical","category":"ioc","detail":"EGRESS-ESCAPED: the sandbox reached the tap on 127.0.0.1:$HEAVY_TAP_PORT","location":"egress-probe"}]}'
else
  echo '{"findings":[]}'
fi
EOF
chmod +x "$EGRESS_PROBE"
export HEAVY_EGRESS_PROBE="$EGRESS_PROBE"

# Step 7's two fixtures: an OSV that answers clean until `$OSV_FLAG` exists,
# and the receiver the flip alert is asserted at.
OSV_FLAG="$HEAVY_WORK/osv-flip-on"
python3 tests/heavy/osv_fake.py "$OSV_FLAG" "$OSV_PORT" "pkg:npm/$FLIP_PKG@$FLIP_VERSION" \
  >"$HEAVY_WORK/osv.err" 2>&1 &
OSV_PID=$!
SINK_LOG="$HEAVY_WORK/sink.log"
: > "$SINK_LOG"
python3 tests/heavy/webhook_sink.py "$SINK_LOG" "$SINK_PORT" >"$HEAVY_WORK/sink.err" 2>&1 &
SINK_PID=$!
stop_fixtures() { kill "$OSV_PID" "$SINK_PID" 2>/dev/null || true; }
trap 'stop_fixtures; heavy_cleanup' EXIT
for _ in $(seq 1 30); do
  curl -s -o /dev/null -X POST "$HEAVY_OSV_URL/v1/query" -d '{}' && curl -s -o /dev/null "$HEAVY_WEBHOOK_URL" && break
  sleep 1
done
: > "$SINK_LOG"  # the readiness GET is not a delivery

# The server runs on a copy: step 5 edits `mode` and reloads, and the
# checked-in file must not change under a run.
CONFIG="$HEAVY_WORK/config.quarantine.toml"
cp tests/heavy/config.quarantine.toml "$CONFIG"

heavy_start_server "$CONFIG"
heavy_start_tap

# The CLI, for `wait` and `why` — the two verbs RFC 0018 hands a developer
# whose install just stopped.
cargo build --quiet -p batlehub-cli >"$HEAVY_WORK/cli-build.txt" 2>&1 \
  || { cat "$HEAVY_WORK/cli-build.txt" >&2; heavy_fail "batlehub-cli did not build"; }
CLI="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/batlehub-cli"
[[ -x "$CLI" ]] || heavy_fail "no batlehub-cli binary at $CLI"
export BATLEHUB_SERVER="$HEAVY_BASE"
export BATLEHUB_TOKEN="$ADMIN_TOKEN"

REG_URL="$HEAVY_TAP_BASE/proxy/$REG/"
EGRESS_URL="$HEAVY_TAP_BASE/proxy/$EGRESS_REG/"

# npm keys auth by `//host/path/`. The token matters here beyond publish: a
# hold answers a caller *without* `quarantine:read` as npm's own not-found
# (RFC 0018 §4.2), so an anonymous npm would see a 404 and this suite would
# measure the wrong branch.
NPMRC="$HEAVY_WORK/npmrc"
cat > "$NPMRC" <<EOF
registry=$REG_URL
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$REG/:_authToken=$ADMIN_TOKEN
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$EGRESS_REG/:_authToken=$ADMIN_TOKEN
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$RESCAN_REG/:_authToken=$ADMIN_TOKEN
EOF
export NPM_CONFIG_USERCONFIG="$NPMRC"
export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false
# npm retries a failed fetch by default (fetch-retries = 2, with a backoff).
# Left at the default deliberately: whether npm retries a `403` is part of
# what the Refuse column records, and the transcript counts the requests.

heavy_log "npm $(npm --version), node $(node --version)"

# sink_wait <event_type> <count> [seconds] — the receiver's log holds
# exactly <count> deliveries of that type within the wait; echoes the last.
sink_wait() {
  local kind="$1" want="$2" secs="${3:-30}" n=0
  for _ in $(seq 1 "$secs"); do
    n="$(grep -c "\"event_type\":\"$kind\"" "$SINK_LOG" || true)"
    [[ "$n" -ge "$want" ]] && break
    sleep 1
  done
  [[ "$n" == "$want" ]] || { cat "$SINK_LOG" >&2; heavy_fail "expected $want delivery(ies) of $kind at the receiver, saw $n"; }
  grep "\"event_type\":\"$kind\"" "$SINK_LOG" | tail -1 | cut -d' ' -f3-
}

new_consumer() {  # dir
  mkdir -p "$1"
  cat > "$1/package.json" <<EOF
{ "name": "consumer", "version": "1.0.0", "private": true }
EOF
}

# ── 1. First contact holds ───────────────────────────────────────────────────

CONSUMER="$HEAVY_WORK/consumer"
new_consumer "$CONSUMER"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache"

heavy_mark "first-contact"
heavy_log "npm install $PKG@$CLEAN — nobody has scanned it yet"
set +e
(cd "$CONSUMER" && npm install "$PKG@$CLEAN" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-1.out" 2>"$HEAVY_WORK/install-1.err"
RC=$?
set -e
if [[ $RC -eq 0 ]]; then
  cat "$HEAVY_WORK/install-1.err" >&2
  heavy_fail "npm install succeeded on first contact — the version was served without a verdict, or npm found the tarball somewhere this server did not give it"
fi
heavy_wire_re_after "first-contact" "GET /proxy/$REG/$PKG/$CLEAN/tarball -> 403 .*X-BatleHub-Verdict: quarantined.*X-BatleHub-Reason: SCAN_PENDING" \
  "the tarball was not refused with a quarantined/SCAN_PENDING verdict on the wire"
heavy_wire_after "first-contact" "GET /proxy/$REG/$PKG -> 200" \
  "the packument was not fetched through the proxy"
# What npm prints is the measurement. A developer whose install stopped has
# npm's stderr and nothing else; the reason code has to be in it.
if ! grep -q "SCAN_PENDING" "$HEAVY_WORK/install-1.err"; then
  cat "$HEAVY_WORK/install-1.err" >&2
  heavy_fail "npm exited $RC but printed no SCAN_PENDING — the reason code did not reach the developer (record this in RFC 0018 §4.4 before building anything else)"
fi
grep -q "batlehub why" "$HEAVY_WORK/install-1.err" \
  || { cat "$HEAVY_WORK/install-1.err" >&2; heavy_fail "npm's output does not tell the developer to run 'batlehub why'"; }
[[ ! -d "$CONSUMER/node_modules/$PKG" ]] \
  || heavy_fail "npm left $PKG in node_modules after a refused tarball"
TARBALL_REQUESTS="$(heavy_wire_count_after first-contact "GET /proxy/$REG/$PKG/$CLEAN/tarball -> 403")"
heavy_log "HOLD-OK (npm exit $RC, $TARBALL_REQUESTS request(s) for the held tarball, SCAN_PENDING printed)"

# ── 2. The worker clears it ──────────────────────────────────────────────────

heavy_log "batlehub wait $REG:$PKG@$CLEAN"
set +e
"$CLI" wait "$REG:$PKG@$CLEAN" --timeout 5m --interval 3s >"$HEAVY_WORK/wait-1.out" 2>&1
RC=$?
set -e
[[ $RC -eq 0 ]] || { cat "$HEAVY_WORK/wait-1.out" >&2; heavy_fail "batlehub wait exited $RC — the embedded worker did not clear $PKG@$CLEAN inside 5 minutes"; }
grep -q "is allowed" "$HEAVY_WORK/wait-1.out" \
  || { cat "$HEAVY_WORK/wait-1.out" >&2; heavy_fail "batlehub wait returned, but not with 'allowed'"; }

heavy_mark "recover"
heavy_log "npm install $PKG@$CLEAN again, same cache"
(cd "$CONSUMER" && npm install "$PKG@$CLEAN" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-2.out" 2>"$HEAVY_WORK/install-2.err" \
  || { cat "$HEAVY_WORK/install-2.err" >&2; heavy_fail "npm install failed after the verdict cleared — npm remembered the refusal (the Recover axis)"; }
INSTALLED="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$CONSUMER/node_modules/$PKG/package.json" 2>/dev/null || true)"
[[ "$INSTALLED" == "$CLEAN" ]] \
  || heavy_fail "node_modules/$PKG/package.json says '$INSTALLED', expected $CLEAN"
heavy_wire_re_after "recover" "GET /proxy/$REG/$PKG/$CLEAN/tarball -> 200" \
  "the tarball was not served through the proxy after the verdict cleared"
heavy_log "RECOVER-OK ($PKG@$CLEAN installed, verdict allowed)"

# ── 3. A finding refuses ─────────────────────────────────────────────────────
#
# Two ways a client can meet a denied version, and they are different
# answers (RFC 0018 §4.4, the Hide column's two halves). A *fresh* resolve
# reads the packument, from which the denied version has been removed, and
# stops there: npm says ETARGET, and the download gate is never reached.
# A *pinned* resolve — a lockfile naming the version and its tarball URL —
# skips the packument and hits the gate, which is where the finding is on
# the wire. Step 3 is the pinned half; step 4 is the fresh one.

VULN_CONSUMER="$HEAVY_WORK/consumer-vuln"
new_consumer "$VULN_CONSUMER"
heavy_mark "vuln-first-contact"
heavy_log "npm install $PKG@$VULN — first contact, then wait for the scan"
set +e
(cd "$VULN_CONSUMER" && npm install "$PKG@$VULN" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-3.out" 2>"$HEAVY_WORK/install-3.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm install $PKG@$VULN succeeded on first contact"
heavy_wire_re_after "vuln-first-contact" "GET /proxy/$REG/$PKG/$VULN/tarball -> 403 .*X-BatleHub-Reason: SCAN_PENDING" \
  "the vulnerable version's first contact was not held on SCAN_PENDING"

set +e
"$CLI" wait "$REG:$PKG@$VULN" --timeout 5m --interval 3s >"$HEAVY_WORK/wait-2.out" 2>&1
RC=$?
set -e
# Exit 1 is "waiting cannot help": the scan came back and the verdict is
# terminal. Exit 2 (timed out) or 0 (served) would both be wrong here.
[[ $RC -eq 1 ]] || { cat "$HEAVY_WORK/wait-2.out" >&2; heavy_fail "batlehub wait exited $RC on a version with a critical advisory; expected 1 (waiting cannot help)"; }

# The pinned resolve: a lockfile that already names the version and the
# tarball this server wrote into the packument. `npm ci` goes straight to it.
PINNED="$HEAVY_WORK/consumer-pinned"
mkdir -p "$PINNED"
cat > "$PINNED/package.json" <<EOF
{ "name": "consumer", "version": "1.0.0", "private": true, "dependencies": { "$PKG": "$VULN" } }
EOF
cat > "$PINNED/package-lock.json" <<EOF
{
  "name": "consumer", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0", "dependencies": { "$PKG": "$VULN" } },
    "node_modules/$PKG": { "version": "$VULN", "resolved": "$REG_URL$PKG/$VULN/tarball", "license": "MIT" }
  }
}
EOF
heavy_mark "vuln-refused"
heavy_log "npm ci with a lockfile pinning $PKG@$VULN — the download gate"
set +e
(cd "$PINNED" && npm ci --registry "$REG_URL") \
  >"$HEAVY_WORK/install-4.out" 2>"$HEAVY_WORK/install-4.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm ci installed $PKG@$VULN over a denied verdict"
heavy_wire_re_after "vuln-refused" "GET /proxy/$REG/$PKG/$VULN/tarball -> 403 .*X-BatleHub-Verdict: denied.*X-BatleHub-Reason: VULNERABILITY" \
  "the pinned tarball was not refused as denied/VULNERABILITY on the wire"
grep -q "VULNERABILITY" "$HEAVY_WORK/install-4.err" \
  || { cat "$HEAVY_WORK/install-4.err" >&2; heavy_fail "npm printed no VULNERABILITY reason code for the denied version"; }
[[ ! -d "$PINNED/node_modules/$PKG" ]] \
  || heavy_fail "npm ci left $PKG in node_modules after a refused tarball"

# The body names the finding; so must `why`. The tap does not keep bodies, so
# the body is read once more directly — after npm, not instead of it.
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_BASE/proxy/$REG/$PKG/$VULN/tarball" >"$HEAVY_WORK/refusal.json"
grep -q "$ADVISORY" "$HEAVY_WORK/refusal.json" \
  || { cat "$HEAVY_WORK/refusal.json" >&2; heavy_fail "the refusal body does not name $ADVISORY"; }

heavy_log "batlehub why $REG:$PKG@$VULN"
set +e
"$CLI" why "$REG:$PKG@$VULN" >"$HEAVY_WORK/why.out" 2>&1
RC=$?
set -e
grep -q "$ADVISORY" "$HEAVY_WORK/why.out" \
  || { cat "$HEAVY_WORK/why.out" >&2; heavy_fail "batlehub why (exit $RC) does not name $ADVISORY, the finding the wire carried"; }
grep -q "denied" "$HEAVY_WORK/why.out" \
  || { cat "$HEAVY_WORK/why.out" >&2; heavy_fail "batlehub why does not say the verdict is denied"; }
heavy_log "FINDING-OK ($ADVISORY on the wire, printed by npm, and in 'why')"

# ── 4. The listing agrees ────────────────────────────────────────────────────
#
# A fresh npm cache: the packument npm cached in step 3 was fetched before
# the verdict existed and lists $VULN. What is measured is the server's
# current answer, not npm's memory of the old one.

export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-view"
heavy_mark "listing"
heavy_log "npm view $PKG versions"
npm view "$PKG" versions --json --registry "$REG_URL" >"$HEAVY_WORK/versions.json" \
  || heavy_fail "npm view failed"
python3 - "$HEAVY_WORK/versions.json" "$CLEAN" "$VULN" <<'PY' || { cat "$HEAVY_WORK/versions.json" >&2; heavy_fail "the listing does not agree with the verdicts: $CLEAN must be listed and $VULN must not"; }
import json, sys
versions = json.load(open(sys.argv[1]))
if isinstance(versions, str):
    versions = [versions]
sys.exit(0 if sys.argv[2] in versions and sys.argv[3] not in versions else 1)
PY
heavy_wire_after "listing" "GET /proxy/$REG/$PKG -> 200" \
  "npm view did not fetch the packument through the proxy"

# …and the fresh resolve stops at the packument: no tarball request, and
# npm's own "no matching version" — the developer sees ETARGET, not the
# finding, which is why `batlehub why` exists (and what §4.4 records).
FRESH="$HEAVY_WORK/consumer-fresh"
new_consumer "$FRESH"
heavy_mark "fresh-resolve"
heavy_log "npm install $PKG@$VULN from a fresh resolve — hidden, so ETARGET"
set +e
(cd "$FRESH" && npm install "$PKG@$VULN" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-fresh.out" 2>"$HEAVY_WORK/install-fresh.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "a fresh resolve installed the hidden version"
grep -q "ETARGET" "$HEAVY_WORK/install-fresh.err" \
  || { cat "$HEAVY_WORK/install-fresh.err" >&2; heavy_fail "npm did not report ETARGET for the hidden version — it found it somewhere"; }
if heavy_wire_seen_after "fresh-resolve" "GET /proxy/$REG/$PKG/$VULN/tarball"; then
  heavy_fail "a fresh resolve requested the hidden version's tarball — the packument still lists it"
fi
heavy_log "LISTING-OK ($VULN hidden from 'npm view' and from a fresh resolve, $CLEAN listed)"

# ── 5. Warn mode serves with headers ─────────────────────────────────────────

heavy_log "Flipping $REG to mode = \"warn\" and reloading"
# Written beside and renamed over: one change event for the file watcher,
# not the truncate-then-write pair an in-place rewrite produces (the watcher
# reads on each, and a reload asked for while it is mid-load answers 400).
python3 - "$CONFIG" <<'PY' || heavy_fail "could not rewrite the config copy"
import os, sys
path = sys.argv[1]
text = open(path).read()
# The first `mode = "block"` after the quarantined registry's name is its
# [registries.security] block; the egress registry keeps its own.
head, sep, tail = text.partition('name = "npm-quarantine-${HEAVY_RUN}"')
assert sep, "registry stanza not found"
assert 'mode = "block"' in tail, "no block mode to flip"
open(path + ".next", "w").write(head + sep + tail.replace('mode = "block"', 'mode = "warn"', 1))
os.replace(path + ".next", path)
PY
sleep 2
RELOAD_CODE="$(curl -sS -o "$HEAVY_WORK/reload.json" -w '%{http_code}' -X POST \
  "$HEAVY_BASE/api/v1/admin/config/reload" -H "Authorization: Bearer $ADMIN_TOKEN")"
[[ "$RELOAD_CODE" == "200" ]] \
  || { cat "$HEAVY_WORK/reload.json" >&2; heavy_fail "config reload answered $RELOAD_CODE"; }

WARN_CONSUMER="$HEAVY_WORK/consumer-warn"
new_consumer "$WARN_CONSUMER"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-warn"
heavy_mark "warn"
heavy_log "npm install $PKG@$VULN under warn"
(cd "$WARN_CONSUMER" && npm install "$PKG@$VULN" --registry "$REG_URL") \
  >"$HEAVY_WORK/install-5.out" 2>"$HEAVY_WORK/install-5.err" \
  || { cat "$HEAVY_WORK/install-5.err" >&2; heavy_fail "npm install failed under mode = \"warn\" — the reload did not take, or the finding still denies"; }
INSTALLED="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$WARN_CONSUMER/node_modules/$PKG/package.json" 2>/dev/null || true)"
[[ "$INSTALLED" == "$VULN" ]] || heavy_fail "under warn, node_modules/$PKG says '$INSTALLED', expected $VULN"
heavy_wire_re_after "warn" "GET /proxy/$REG/$PKG/$VULN/tarball -> 200 .*X-BatleHub-Verdict: warned.*X-BatleHub-Reason: VULNERABILITY" \
  "the tarball was served under warn without the verdict headers on the wire"
heavy_log "WARN-OK (served, X-BatleHub-Verdict: warned on the tarball)"

# ── 6. Egress ────────────────────────────────────────────────────────────────

if [[ "$SKIP_SANDBOX" == "1" ]]; then
  heavy_log "EGRESS-UNMEASURED (HEAVY_QUARANTINE_SKIP_SANDBOX=1: no bwrap on this host; the sandbox row is CI's)"
else
  EGRESS_CONSUMER="$HEAVY_WORK/consumer-egress"
  new_consumer "$EGRESS_CONSUMER"
  export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-egress"
  heavy_mark "egress"
  heavy_log "npm install $EGRESS_PKG@$EGRESS_VERSION through $EGRESS_REG — first contact, then the probe runs"
  set +e
  (cd "$EGRESS_CONSUMER" && npm install "$EGRESS_PKG@$EGRESS_VERSION" --registry "$EGRESS_URL") \
    >"$HEAVY_WORK/install-6.out" 2>"$HEAVY_WORK/install-6.err"
  RC=$?
  set -e
  [[ $RC -ne 0 ]] || heavy_fail "npm install through $EGRESS_REG succeeded on first contact"
  set +e
  "$CLI" wait "$EGRESS_REG:$EGRESS_PKG@$EGRESS_VERSION" --timeout 5m --interval 3s >"$HEAVY_WORK/wait-3.out" 2>&1
  RC=$?
  set -e
  if [[ $RC -ne 0 ]]; then
    cat "$HEAVY_WORK/wait-3.out" >&2
    "$CLI" why "$EGRESS_REG:$EGRESS_PKG@$EGRESS_VERSION" >&2 || true
    if grep -q "EGRESS-ESCAPED" "$HEAVY_WORK/wait-3.out" || "$CLI" why "$EGRESS_REG:$EGRESS_PKG@$EGRESS_VERSION" 2>/dev/null | grep -q "EGRESS-ESCAPED"; then
      heavy_fail "the probe reached the tap from inside the sandbox — bwrap --unshare-net is not in effect"
    fi
    heavy_fail "batlehub wait exited $RC on the egress registry — the probe scanner did not answer (is bwrap able to create user namespaces here?)"
  fi
  # The two halves of the claim: the scanner ran (the version is served, and
  # `egress` is a required scanner), and nothing it did showed up on the wire.
  heavy_wire_not "GET /egress-probe" \
    "the tap saw the probe's connection — the scanner sandbox has network access"
  (cd "$EGRESS_CONSUMER" && npm install "$EGRESS_PKG@$EGRESS_VERSION" --registry "$EGRESS_URL") \
    >"$HEAVY_WORK/install-7.out" 2>"$HEAVY_WORK/install-7.err" \
    || { cat "$HEAVY_WORK/install-7.err" >&2; heavy_fail "npm install through $EGRESS_REG failed after the probe answered"; }
  curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" \
    "$HEAVY_BASE/api/v1/verdicts/$EGRESS_REG/$EGRESS_PKG/$EGRESS_VERSION" >"$HEAVY_WORK/egress-verdict.json" \
    || heavy_fail "could not read the egress verdict"
  python3 - "$HEAVY_WORK/egress-verdict.json" <<'PY' || { cat "$HEAVY_WORK/egress-verdict.json" >&2; heavy_fail "the egress verdict does not record both scanners as done, or is not allowed"; }
import json, sys
v = json.load(open(sys.argv[1]))
sys.exit(0 if v.get("state") == "allowed" and set(v.get("scanners_done", [])) >= {"osv", "egress"} else 1)
PY
  heavy_log "EGRESS-OK (probe ran under bwrap, no connection observed, version served)"
fi

# ── 7. The flip (RFC 0018 phase 4) ───────────────────────────────────────────
#
# Scanned clean, served, pulled; then the database learns something and the
# *scheduler* rescans it. What a client sees afterwards, and what the admin
# is told, are the two things this phase promised.

RESCAN_URL="$HEAVY_TAP_BASE/proxy/$RESCAN_REG/"
heavy_log "Subscribing the webhook to verdict_changed on $RESCAN_REG"
SUB_CODE="$(curl -sS -o "$HEAVY_WORK/sub.json" -w '%{http_code}' -X POST "$HEAVY_BASE/api/v1/admin/notifications/subscriptions" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
  -d "{\"registry\":\"$RESCAN_REG\",\"package_name\":null,\"event_types\":[\"verdict_changed\",\"artifact_released\"],\"channel_name\":\"sink\"}")"
[[ "$SUB_CODE" == "200" || "$SUB_CODE" == "201" ]] || { cat "$HEAVY_WORK/sub.json" >&2; heavy_fail "creating the subscription answered $SUB_CODE"; }

FLIP_CONSUMER="$HEAVY_WORK/consumer-flip"
new_consumer "$FLIP_CONSUMER"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-flip"
heavy_mark "flip-first-contact"
heavy_log "npm install $FLIP_PKG@$FLIP_VERSION through $RESCAN_REG — held, then cleared by the fake OSV"
set +e
(cd "$FLIP_CONSUMER" && npm install "$FLIP_PKG@$FLIP_VERSION" --registry "$RESCAN_URL") \
  >"$HEAVY_WORK/install-8.out" 2>"$HEAVY_WORK/install-8.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm install through $RESCAN_REG succeeded on first contact"
"$CLI" wait "$RESCAN_REG:$FLIP_PKG@$FLIP_VERSION" --timeout 5m --interval 3s >"$HEAVY_WORK/wait-4.out" 2>&1 \
  || { cat "$HEAVY_WORK/wait-4.out" >&2; heavy_fail "the fake OSV did not clear $FLIP_PKG@$FLIP_VERSION"; }
(cd "$FLIP_CONSUMER" && npm install "$FLIP_PKG@$FLIP_VERSION" --registry "$RESCAN_URL") \
  >"$HEAVY_WORK/install-9.out" 2>"$HEAVY_WORK/install-9.err" \
  || { cat "$HEAVY_WORK/install-9.err" >&2; heavy_fail "npm install through $RESCAN_REG failed after the clean scan"; }
heavy_wire_re_after "flip-first-contact" "GET /proxy/$RESCAN_REG/$FLIP_PKG/$FLIP_VERSION/tarball -> 200" \
  "the tarball was not served after the clean scan"
# The hold's release reached the receiver: the identity refused on first
# contact is the one named.
RELEASED="$(sink_wait artifact_released 1)"
python3 - "$RELEASED" "$FLIP_PKG" <<'PY' || { echo "$RELEASED" >&2; heavy_fail "artifact_released does not name ci-admin, the identity refused on first contact"; }
import json, sys
e = json.loads(sys.argv[1])
ok = e["package_name"] == sys.argv[2] and any(r["identity"] == "ci-admin" for r in e["metadata"]["recipients"])
sys.exit(0 if ok else 1)
PY

heavy_log "The fake OSV learns an advisory; the scheduler rescans within its tick"
touch "$OSV_FLAG"
heavy_mark "flip"
# The scheduler ticks once a minute (RESCAN_TICK) and the verdict is a
# second old: the flip is observed at the receiver, not requested.
ALERT="$(sink_wait verdict_changed 1 90)"
python3 - "$ALERT" "$RESCAN_REG" "$FLIP_PKG" "$FLIP_VERSION" <<'PY' || { echo "$ALERT" >&2; heavy_fail "the flip alert is not RFC 0018 decision 23's"; }
import json, sys
e = json.loads(sys.argv[1]); reg, pkg, ver = sys.argv[2:5]
m = e["metadata"]
checks = [
    e["registry"] == reg and e["package_name"] == pkg and e["version"] == ver,
    e["actor"] == "system:security-worker",
    m["from"] in ("allowed", "warned") and m["to"] == "denied",
    m["trigger"] == "rescan",                       # the scheduler's, not an admin's
    "VULNERABILITY" in m["reason_codes"],
    any(f["reference"] == "BATLEHUB-HEAVY-FLIP" or "BATLEHUB-HEAVY-FLIP" in f["summary"] for f in m["findings"]),
    m["pullers_known"] is True,
    any(p["identity"] == "ci-admin" for p in m["pullers"]),   # the two installs above
]
for i, c in enumerate(checks):
    if not c:
        print(f"check {i} failed", file=sys.stderr)
sys.exit(0 if all(checks) else 1)
PY

heavy_log "npm install $FLIP_PKG@$FLIP_VERSION from a clean cache — refused now"
FLIP_AGAIN="$HEAVY_WORK/consumer-flip-again"
new_consumer "$FLIP_AGAIN"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-flip-again"
set +e
(cd "$FLIP_AGAIN" && npm install "$FLIP_PKG@$FLIP_VERSION" --registry "$RESCAN_URL") \
  >"$HEAVY_WORK/install-10.out" 2>"$HEAVY_WORK/install-10.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm installed $FLIP_PKG@$FLIP_VERSION after the rescan denied it"
grep -q "ETARGET\|VULNERABILITY" "$HEAVY_WORK/install-10.err" \
  || { cat "$HEAVY_WORK/install-10.err" >&2; heavy_fail "npm failed for a reason that is neither the hidden version nor the refusal"; }

heavy_log "batlehub verdicts pullers $RESCAN_REG:$FLIP_PKG@$FLIP_VERSION"
"$CLI" verdicts pullers "$RESCAN_REG:$FLIP_PKG@$FLIP_VERSION" >"$HEAVY_WORK/pullers.out" 2>&1 \
  || { cat "$HEAVY_WORK/pullers.out" >&2; heavy_fail "batlehub verdicts pullers failed"; }
grep -q "ci-admin" "$HEAVY_WORK/pullers.out" \
  || { cat "$HEAVY_WORK/pullers.out" >&2; heavy_fail "batlehub verdicts pullers does not name ci-admin"; }
"$CLI" verdicts pullers "$RESCAN_REG:$FLIP_PKG@$FLIP_VERSION" --csv >"$HEAVY_WORK/pullers.csv" 2>&1 \
  || heavy_fail "batlehub verdicts pullers --csv failed"
head -1 "$HEAVY_WORK/pullers.csv" | grep -q '^identity,role,first_pull,last_pull,count' \
  || { cat "$HEAVY_WORK/pullers.csv" >&2; heavy_fail "the CSV has no header"; }
grep -q '^ci-admin,admin,' "$HEAVY_WORK/pullers.csv" \
  || { cat "$HEAVY_WORK/pullers.csv" >&2; heavy_fail "the CSV does not name ci-admin"; }
heavy_log "FLIP-OK (scheduled rescan denied a served version; alert, pullers and the refusal all seen)"

heavy_done QUARANTINE-HEAVY-OK
