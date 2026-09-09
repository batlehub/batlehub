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
#   8. **A pushed flag.** (RFC 0002.) A version scanned clean, served and
#      pulled, then the SOC pushes a signed `hard_block` for it: the next
#      `npm install` from a clean cache is refused, `batlehub why` names the
#      flag, and the exposure report says who already had it and that the
#      pull came *before* the flag. The revoke lifts it and npm installs
#      again — the lifecycle of §4.5, driven by npm rather than asserted.
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
FLAGS_REG="npm-flags-$HEAVY_RUN"
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
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$FLAGS_REG/:_authToken=$ADMIN_TOKEN
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
  local dir="$1"
  mkdir -p "$dir"
  cat > "$dir/package.json" <<EOF
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

# ── 8. A pushed flag (RFC 0002) ──────────────────────────────────────────────
#
# The other producer of a denial. Everything above is a scanner's answer;
# this is a human organisation's, pushed over the wire and signed, and RFC
# 0002 §13.1 decision 1 makes it a finding of kind `SocVerdict` on a
# `[security]` registry rather than a second engine. What a client sees is
# therefore the same refusal — which is the claim: one producer, one gate.
#
# The order matters. The version is installed *first*, so the pull is on the
# record before the flag exists and the exposure report has a retroactive
# row to classify (§4.8's `before_flag`). That row is the question this RFC
# exists to answer: who already has the thing you just learned about.

FLAGS_URL="$HEAVY_TAP_BASE/proxy/$FLAGS_REG/"
FLAG_PKG="left-pad"
FLAG_VERSION="1.3.0"
FLAG_ID="BATLEHUB-HEAVY-SOC-$HEAVY_RUN"
# `%{http_code}` is the whole of every status assertion in this step.
CURL_CODE='%{http_code}'

# The push credential, as `[[flag_sources]] soc` holds it.
SOC_SECRET="heavy-soc-secret"
# The two credentials this step computes, as `soc` would. Both defined in
# lib.sh and gated against the server's own formatter by
# `crates/web/tests/flag_revoke_canonical.rs`: a `POST` signs the raw body, a
# `DELETE` signs `DELETE\n/api/v1/flags/{source}/{external_id}`, and the
# endpoint answers a stale signature as an unknown source rather than saying so.
sign() { local body="$1"; heavy_flag_sign "$SOC_SECRET" "$body"; }
sign_revoke() { local external_id="$1"; heavy_flag_sign_revoke "$SOC_SECRET" soc "$external_id"; }

FLAG_CONSUMER="$HEAVY_WORK/consumer-flags"
new_consumer "$FLAG_CONSUMER"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-flags"
heavy_mark "flags-pull"
heavy_log "npm install $FLAG_PKG@$FLAG_VERSION through $FLAGS_REG — the pull the report will find"
set +e
(cd "$FLAG_CONSUMER" && npm install "$FLAG_PKG@$FLAG_VERSION" --registry "$FLAGS_URL") \
  >"$HEAVY_WORK/install-11.out" 2>"$HEAVY_WORK/install-11.err"
set -e
"$CLI" wait "$FLAGS_REG:$FLAG_PKG@$FLAG_VERSION" --timeout 5m --interval 3s >"$HEAVY_WORK/wait-5.out" 2>&1 \
  || { cat "$HEAVY_WORK/wait-5.out" >&2; heavy_fail "OSV did not clear $FLAG_PKG@$FLAG_VERSION on $FLAGS_REG"; }
(cd "$FLAG_CONSUMER" && npm install "$FLAG_PKG@$FLAG_VERSION" --registry "$FLAGS_URL") \
  >"$HEAVY_WORK/install-12.out" 2>"$HEAVY_WORK/install-12.err" \
  || { cat "$HEAVY_WORK/install-12.err" >&2; heavy_fail "npm install through $FLAGS_REG failed after the clean scan"; }
[[ -f "$FLAG_CONSUMER/node_modules/$FLAG_PKG/package.json" ]] \
  || heavy_fail "$FLAG_PKG is not installed, so there is no pull for the report to find"
heavy_wire_re_after "flags-pull" "GET /proxy/$FLAGS_REG/$FLAG_PKG/$FLAG_VERSION/tarball -> 200" \
  "the tarball was not served before the flag"

heavy_log "POST /api/v1/flags/soc — a signed hard_block for $FLAG_PKG@$FLAG_VERSION"
PUSH_BODY="$(python3 - "$FLAGS_REG" "$FLAG_PKG" "$FLAG_VERSION" "$FLAG_ID" <<'PY'
import json, sys
reg, pkg, ver, ext = sys.argv[1:5]
print(json.dumps({"flags": [{
    "external_id": ext,
    "registry": reg,
    "package_name": pkg,
    "version": ver,
    "kind": "malware",
    "effect": "hard_block",
    "severity": "critical",
    "summary": "BATLEHUB-HEAVY-SOC: pushed by tests/heavy/quarantine.sh",
    "url": "https://example.invalid/soc/heavy",
}]}, separators=(",", ":")))
PY
)"
PUSH_CODE="$(curl -sS -o "$HEAVY_WORK/push.json" -w "$CURL_CODE" -X POST "$HEAVY_BASE/api/v1/flags/soc" \
  -H "Content-Type: application/json" -H "X-Hub-Signature-256: $(sign "$PUSH_BODY")" \
  --data-raw "$PUSH_BODY")"
[[ "$PUSH_CODE" == "200" ]] || { cat "$HEAVY_WORK/push.json" >&2; heavy_fail "the flag push answered $PUSH_CODE"; }
python3 - "$HEAVY_WORK/push.json" <<'PY' || { cat "$HEAVY_WORK/push.json" >&2; heavy_fail "the push was not accepted uncapped"; }
import json, sys
r = json.load(open(sys.argv[1]))
item = r["items"][0] if isinstance(r.get("items"), list) and r["items"] else {}
ok = r.get("accepted") == 1 and r.get("rejected", 0) == 0 and item.get("status") == "accepted" \
     and not item.get("effect_capped", False)
sys.exit(0 if ok else 1)
PY

# An unsigned push must not be a way in, and must not confirm the name.
BAD_CODE="$(curl -sS -o /dev/null -w "$CURL_CODE" -X POST "$HEAVY_BASE/api/v1/flags/soc" \
  -H "Content-Type: application/json" -H "X-Hub-Signature-256: sha256=$(printf 'f%.0s' {1..64})" \
  --data-raw "$PUSH_BODY")"
[[ "$BAD_CODE" == "404" ]] || heavy_fail "a bad signature answered $BAD_CODE, expected 404"

# A denied version is hidden from the packument (step 4), so a plain
# `npm install` would stop at the resolve. The lockfile is what makes npm
# ask for the tarball, which is the gate this step is about — the same
# shape step 3 uses for a scanner's denial.
heavy_mark "flags-refused"
FLAG_PINNED="$HEAVY_WORK/consumer-flags-pinned"
mkdir -p "$FLAG_PINNED"
# `npm ci` refuses a package.json that does not declare what the lock holds,
# and refusing for *that* reason would look exactly like the refusal this
# step is trying to observe. Both files name the dependency.
cat > "$FLAG_PINNED/package.json" <<EOF
{ "name": "consumer", "version": "1.0.0", "private": true, "dependencies": { "$FLAG_PKG": "$FLAG_VERSION" } }
EOF
cat > "$FLAG_PINNED/package-lock.json" <<EOF
{
  "name": "consumer", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0", "dependencies": { "$FLAG_PKG": "$FLAG_VERSION" } },
    "node_modules/$FLAG_PKG": { "version": "$FLAG_VERSION", "resolved": "$FLAGS_URL$FLAG_PKG/$FLAG_VERSION/tarball", "license": "WTFPL" }
  }
}
EOF
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-flags-again"
heavy_log "npm ci with a lockfile pinning $FLAG_PKG@$FLAG_VERSION — the flag refuses the download"
set +e
(cd "$FLAG_PINNED" && npm ci --registry "$FLAGS_URL") \
  >"$HEAVY_WORK/install-13.out" 2>"$HEAVY_WORK/install-13.err"
RC=$?
set -e
[[ $RC -ne 0 ]] || heavy_fail "npm ci installed $FLAG_PKG@$FLAG_VERSION over a pushed hard_block"
heavy_wire_re_after "flags-refused" "GET /proxy/$FLAGS_REG/$FLAG_PKG/$FLAG_VERSION/tarball -> 403 .*X-BatleHub-Verdict: denied.*X-BatleHub-Reason: SOC_VERDICT" \
  "the flagged tarball was not refused as denied/SOC_VERDICT on the wire"
[[ ! -d "$FLAG_PINNED/node_modules/$FLAG_PKG" ]] \
  || heavy_fail "npm ci left $FLAG_PKG in node_modules after a refused tarball"
# The body names the flag, not just the code: the tap keeps no bodies, so it
# is read once more directly — after npm, not instead of it.
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_BASE/proxy/$FLAGS_REG/$FLAG_PKG/$FLAG_VERSION/tarball" >"$HEAVY_WORK/flag-refusal.json"
grep -q "BATLEHUB-HEAVY-SOC" "$HEAVY_WORK/flag-refusal.json" \
  || { head -c 400 "$HEAVY_WORK/flag-refusal.json" >&2; heavy_fail "the refusal body does not name the pushed flag"; }

heavy_log "batlehub why $FLAGS_REG:$FLAG_PKG@$FLAG_VERSION"
"$CLI" why "$FLAGS_REG:$FLAG_PKG@$FLAG_VERSION" >"$HEAVY_WORK/why-2.out" 2>&1 || true
grep -q "BATLEHUB-HEAVY-SOC" "$HEAVY_WORK/why-2.out" \
  || { cat "$HEAVY_WORK/why-2.out" >&2; heavy_fail "'batlehub why' does not name the pushed flag"; }

heavy_log "GET /api/v1/admin/exposure — who already pulled it, and when"
curl -fsS -o "$HEAVY_WORK/exposure.json" -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$HEAVY_BASE/api/v1/admin/exposure?registry=$FLAGS_REG" \
  || heavy_fail "the exposure report was not served"
python3 - "$HEAVY_WORK/exposure.json" "$FLAG_PKG" "$FLAG_VERSION" "$FLAG_ID" <<'PY' || { head -c 600 "$HEAVY_WORK/exposure.json" >&2; heavy_fail "the exposure report does not name ci-admin's pull before the flag"; }
import json, sys
r = json.load(open(sys.argv[1])); pkg, ver, ext = sys.argv[2:5]
rows = [x for x in r["rows"] if x["package_name"] == pkg and x["version"] == ver and x["external_id"] == ext]
if not rows:
    print("no row for the flagged coordinate", file=sys.stderr); sys.exit(1)
row = rows[0]
checks = [
    row["consumer"] == "ci-admin",
    row["effect"] == "hard_block",
    row["source"] == "soc",
    row["pulls"] >= 1,
    row["pulls_before_flag"] >= 1,          # the install above predates the push
]
# The coverage block is the other half of §4.9: it says what the report could
# not see, and this registry has a security profile and a scan behind it.
cov = r.get("coverage") or {}
checks.append(cov.get("registries_total", 0) >= 1)
for i, c in enumerate(checks):
    if not c:
        print(f"check {i} failed: {json.dumps(row)[:300]}", file=sys.stderr)
sys.exit(0 if all(checks) else 1)
PY

"$CLI" admin flags list --registry "$FLAGS_REG" >"$HEAVY_WORK/flags-list.out" 2>&1 \
  || { cat "$HEAVY_WORK/flags-list.out" >&2; heavy_fail "batlehub admin flags list failed"; }
grep -q "$FLAG_ID" "$HEAVY_WORK/flags-list.out" \
  || { cat "$HEAVY_WORK/flags-list.out" >&2; heavy_fail "batlehub admin flags list does not name the flag"; }
"$CLI" admin exposure --registry "$FLAGS_REG" >"$HEAVY_WORK/exposure.out" 2>&1 \
  || { cat "$HEAVY_WORK/exposure.out" >&2; heavy_fail "batlehub admin exposure failed"; }
grep -q "ci-admin" "$HEAVY_WORK/exposure.out" \
  || { cat "$HEAVY_WORK/exposure.out" >&2; heavy_fail "batlehub admin exposure does not name ci-admin"; }

heavy_log "DELETE /api/v1/flags/soc/$FLAG_ID — the revoke, and the scheduler re-deriving"
REVOKE_CODE="$(curl -sS -o "$HEAVY_WORK/revoke.json" -w "$CURL_CODE" -X DELETE \
  "$HEAVY_BASE/api/v1/flags/soc/$FLAG_ID" -H "X-Hub-Signature-256: $(sign_revoke "$FLAG_ID")")"
[[ "$REVOKE_CODE" == "200" ]] || { cat "$HEAVY_WORK/revoke.json" >&2; heavy_fail "the revoke answered $REVOKE_CODE"; }
grep -q '"revoked":true' "$HEAVY_WORK/revoke.json" \
  || { cat "$HEAVY_WORK/revoke.json" >&2; heavy_fail "the revoke did not report the flag gone"; }
# The revoke queues a rescan (§13.1 decision 3), so the lift is the
# worker's, not the API call's. `batlehub wait` is the wrong instrument
# here: it exits 1 on the first poll of a *denied* verdict by design, and
# the verdict is denied until the rescan lands. The tarball itself is the
# signal, polled until it is served or the minute is out.
LIFTED=0
for _ in $(seq 1 60); do
  if [[ "$(curl -sS -o /dev/null -w "$CURL_CODE" -H "Authorization: Bearer $ADMIN_TOKEN" \
      "$HEAVY_BASE/proxy/$FLAGS_REG/$FLAG_PKG/$FLAG_VERSION/tarball")" == "200" ]]; then
    LIFTED=1
    break
  fi
  sleep 1
done
[[ "$LIFTED" == "1" ]] || heavy_fail "the revoke did not clear the hard block within 60s"
export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-flags-after"
(cd "$FLAG_PINNED" && npm ci --registry "$FLAGS_URL") \
  >"$HEAVY_WORK/install-14.out" 2>"$HEAVY_WORK/install-14.err" \
  || { cat "$HEAVY_WORK/install-14.err" >&2; heavy_fail "npm ci failed after the revoke"; }
[[ -f "$FLAG_PINNED/node_modules/$FLAG_PKG/package.json" ]] \
  || heavy_fail "$FLAG_PKG is not installed after the revoke — the same lockfile that was refused"
heavy_log "FLAGS-OK (a signed hard_block refused npm, the report named the pull before it, the revoke lifted it)"

heavy_done QUARANTINE-HEAVY-OK
