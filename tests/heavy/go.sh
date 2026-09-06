#!/usr/bin/env bash
# Heavy Go integration test — the real `go` command against a goproxy
# registry, measuring the RFC 0018 §4.4 axes of a rejection (there is no
# Publish axis: the Go module proxy protocol has no publish).
#
#   Hide     A blocked version is omitted from `@v/list` and from `@latest`
#            (RFC 0006): a fresh `go get module@latest` picks the previous
#            version; a go.mod that pins the blocked one reaches the
#            download gate.
#   Refuse   What `go` prints for the native block status and for the other
#            one, whether it retries, and — the Go-specific part — what a
#            GOPROXY list with a `direct` fallback does with each: the protocol
#            says a 404/410 means "try the next proxy" and anything else means
#            stop, which turns a hold served as 404 into a bypass.
#   Recover  With the same GOMODCACHE that saw the refusal, the next
#            `go mod download` after the block lifts must succeed.
#
# Run via `task test:go-heavy` or directly. Needs network: proxy.golang.org
# and sum.golang.org (through the proxy's `/sumdb/`). Environment knobs:
# DATABASE_URL (required), HEAVY_PORT (8103), HEAVY_TAP_PORT (8113), COVERAGE,
# HEAVY_GO_MODULE (github.com/google/uuid), HEAVY_GO_BLOCKED (v1.6.0, the
# newest), HEAVY_GO_PREVIOUS (v1.5.0).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init go 8103 8113
heavy_need go "the Go toolchain"
heavy_need python3 "python3 (the wire tap)"

REG="go-$HEAVY_RUN"
MODULE="${HEAVY_GO_MODULE:-github.com/google/uuid}"
BLOCKED="${HEAVY_GO_BLOCKED:-v1.6.0}"
PREVIOUS="${HEAVY_GO_PREVIOUS:-v1.5.0}"

heavy_start_server tests/heavy/config.go.toml
heavy_start_tap

PROXY="$HEAVY_TAP_BASE/proxy/$REG"
ZIP="/proxy/$REG/$MODULE/@v/$BLOCKED.zip"
INFO="/proxy/$REG/$MODULE/@v/$BLOCKED.info"

# consumer <dir> [version] — a module requiring $MODULE, pinned when asked.
consumer() {
  local dir="$1" version="${2:-}"
  mkdir -p "$dir"
  cat >"$dir/go.mod" <<EOF
module example.com/heavy

go 1.22
${version:+
require $MODULE $version
}
EOF
  cat >"$dir/main.go" <<EOF
package main

import _ "$MODULE"

func main() {}
EOF
  return $?
}

# run_go <cache> <dir> <goproxy> <args...>
run_go() {
  local cache="$1" dir="$2" goproxy="$3"
  shift 3
  mkdir -p "$cache/mod" "$cache/build"
  (cd "$dir" && GOMODCACHE="$cache/mod" GOCACHE="$cache/build" GOPATH="$cache/gopath" \
    GOPROXY="$goproxy" GOFLAGS="-mod=mod -modcacherw" GOTOOLCHAIN=local go "$@") >"$RUN_OUT" 2>&1
  return $?
}

# ── Hide ─────────────────────────────────────────────────────────────────────

heavy_mark fresh-before
RUN_OUT="$HEAVY_WORK/fresh-before.txt"
consumer "$HEAVY_WORK/c0"
run_go "$HEAVY_WORK/cache0" "$HEAVY_WORK/c0" "$PROXY" get "$MODULE@latest" \
  || { cat "$RUN_OUT" >&2; heavy_fail "go get through the proxy failed"; }
grep -q "$MODULE $BLOCKED" "$HEAVY_WORK/c0/go.mod" \
  || { cat "$HEAVY_WORK/c0/go.mod" >&2; heavy_fail "before the block, @latest is not $BLOCKED — set HEAVY_GO_BLOCKED to the newest version"; }
heavy_wire_after fresh-before "GET /proxy/$REG/$MODULE/@v/list -> 200"
heavy_wire_after fresh-before "GET /proxy/$REG/sumdb/sum.golang.org/supported -> 200" \
  "the registry did not claim the checksum database"
heavy_wire_after fresh-before "GET /proxy/$REG/sumdb/sum.golang.org/lookup/$MODULE@$BLOCKED -> 200" \
  "go did not verify the checksum through the proxy's sumdb — it went to the log directly"

heavy_log "Blocking $MODULE@$BLOCKED"
heavy_block "$REG" "$MODULE" "$BLOCKED"

heavy_mark fresh-after
RUN_OUT="$HEAVY_WORK/fresh-after.txt"
consumer "$HEAVY_WORK/c1"
run_go "$HEAVY_WORK/cache1" "$HEAVY_WORK/c1" "$PROXY" get "$MODULE@latest" \
  || { cat "$RUN_OUT" >&2; heavy_fail "a fresh go get @latest with $BLOCKED blocked failed instead of picking the previous version"; }
grep -q "$MODULE $PREVIOUS" "$HEAVY_WORK/c1/go.mod" \
  || { cat "$HEAVY_WORK/c1/go.mod" >&2; heavy_fail "Hide: a fresh @latest did not pick $PREVIOUS"; }
heavy_log "Hide/fresh: @latest resolves to $PREVIOUS with $BLOCKED hidden"

heavy_mark pinned-native
RUN_OUT="$HEAVY_WORK/pinned-native.txt"
consumer "$HEAVY_WORK/c2" "$BLOCKED"
if run_go "$HEAVY_WORK/cache2" "$HEAVY_WORK/c2" "$PROXY" mod download; then
  cat "$RUN_OUT" >&2
  heavy_fail "Hide/pinned: go mod download of the blocked $BLOCKED succeeded"
fi
FIRST=$(awk -v mark="### pinned-native" -v p="GET /proxy/$REG/$MODULE/@v/$BLOCKED." '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { print; exit }' "$HEAVY_LOG")
[[ -n "$FIRST" ]] || heavy_fail "Hide/pinned: the pinned build never asked for $BLOCKED"
NATIVE=$(echo "$FIRST" | sed 's/.* -> \([0-9]*\).*/\1/')
NATIVE_TRIES=$(awk -v mark="### pinned-native" -v p="GET /proxy/$REG/$MODULE/@v/$BLOCKED." '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { c++ } END { print c + 0 }' "$HEAVY_LOG")
heavy_log "Refuse/native: the block answers $NATIVE ($NATIVE_TRIES request(s) for $BLOCKED); go said:"
grep -v '^$' "$RUN_OUT" | head -4 >&2

# ── Refuse: the other status, and the direct fallback ────────────────────────

case "$NATIVE" in
  403) OTHER=404 ;;
  404) OTHER=403 ;;
  *) heavy_fail "the block answered $NATIVE, neither 403 nor 404" ;;
esac
heavy_tap_rewrite GET "/proxy/$REG/$MODULE/@v/$BLOCKED." "$NATIVE" "$OTHER" "Retry-After: 30"
heavy_mark pinned-other
RUN_OUT="$HEAVY_WORK/pinned-other.txt"
consumer "$HEAVY_WORK/c3" "$BLOCKED"
START=$(date +%s)
if run_go "$HEAVY_WORK/cache3" "$HEAVY_WORK/c3" "$PROXY" mod download; then
  heavy_fail "Refuse: go mod download succeeded on a $OTHER"
fi
ELAPSED=$(( $(date +%s) - START ))
# go asks for `.mod` first when go.mod already pins the version, `.info`
# first when resolving — whichever it was, it must have been rewritten.
grep -qF -- "@v/$BLOCKED." "$HEAVY_LOG" && awk -v mark="### pinned-other" -v p="GET /proxy/$REG/$MODULE/@v/$BLOCKED." -v w="-> $NATIVE=>$OTHER" '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 && index($0, w) { found = 1 }
  END { exit found ? 0 : 1 }' "$HEAVY_LOG" \
  || heavy_fail "Refuse: no rewritten $NATIVE=>$OTHER answer for $BLOCKED after mark pinned-other"
heavy_log "Refuse/$OTHER: took ${ELAPSED}s with Retry-After: 30; go said:"
grep -v '^$' "$RUN_OUT" | head -4 >&2
[[ "$ELAPSED" -lt 25 ]] || heavy_fail "go waited on Retry-After — the CI contract assumes it does not"
heavy_tap_rewrite_clear

# With `direct` after the proxy: a 404 is "not here, ask the next source" and
# the blocked module is fetched from the origin behind BatleHub's back; a 403
# is "stop". Whichever of the two is the block's native answer, both are
# measured, because the doc page has to say which one a hold must use.
for status in 404 403; do
  if [[ "$status" != "$NATIVE" ]]; then
    heavy_tap_rewrite GET "/proxy/$REG/$MODULE/@v/$BLOCKED." "$NATIVE" "$status"
  fi
  heavy_mark "fallback-$status"
  RUN_OUT="$HEAVY_WORK/fallback-$status.txt"
  consumer "$HEAVY_WORK/f$status" "$BLOCKED"
  if run_go "$HEAVY_WORK/cachef$status" "$HEAVY_WORK/f$status" "$PROXY,direct" mod download; then
    FALLBACK="bypassed"
  else
    FALLBACK="stopped"
  fi
  declare "FALLBACK_$status=$FALLBACK"
  heavy_log "Refuse/fallback: GOPROXY=proxy,direct on a $status: $FALLBACK"
  heavy_tap_rewrite_clear
done
[[ "$FALLBACK_403" == "stopped" ]] \
  || heavy_fail "a 403 did not stop the GOPROXY fallback — a hold served as 403 would be bypassed"

# ── Recover ──────────────────────────────────────────────────────────────────

heavy_log "Unblocking $MODULE@$BLOCKED"
heavy_unblock "$REG" "$MODULE" "$BLOCKED"
heavy_mark recover
RUN_OUT="$HEAVY_WORK/recover.txt"
run_go "$HEAVY_WORK/cache2" "$HEAVY_WORK/c2" "$PROXY" mod download \
  || { cat "$RUN_OUT" >&2; heavy_fail "Recover: go mod download after the unblock failed — the client remembered the refusal"; }
heavy_wire_after recover "GET $ZIP -> 200" "Recover: the zip was not fetched after the unblock"
heavy_log "Recover: the same module cache downloaded $MODULE@$BLOCKED after the unblock"

heavy_done "go heavy test passed: hide=omitted/$PREVIOUS refuse=$NATIVE(x$NATIVE_TRIES) fallback-404=$FALLBACK_404 fallback-403=$FALLBACK_403 recover=ok"
