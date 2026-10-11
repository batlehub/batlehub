#!/usr/bin/env bash
# CPU profile per request path: every soak arm, alone, under a constant load,
# while the server samples its own CPU.
#
# What it answers, per arm (one request shape against one registry kind, from
# `perf/k6/soak_arms.js`): how much CPU one request costs, which functions it
# is spent in, and which routes the router actually matched to serve it. Then,
# across all arms, which of our own functions cost the most overall.
#
# The server is a release build with the `profiling` feature
# (`server/src/profiling.rs`) and line-level debuginfo kept, so a stack names
# functions rather than addresses. The upstream is the soak's mock
# (`perf/mock-upstream`), so a profile measures this process, not the network.
#
#   DATABASE_URL=postgresql://… bash perf/scripts/profile.sh
#
# Knobs (environment):
#   PROFILE_SECONDS   CPU window per arm (15)
#   PROFILE_RATE      requests/s offered to the arm (50)
#   PROFILE_FREQ      samples/s (199)
#   PROFILE_ARMS      extended regex selecting arms by op (all)
#   PROFILE_BUILD     release | debug (release; debug only smoke-tests the harness)
#   PROFILE_BASELINE  a previous run's profile.json to compare with (none)
#   PROFILE_OUT       output directory (perf/results/profile)
#
# Outputs, in $PROFILE_OUT:
#   arms/<op>.folded        folded stacks — speedscope and inferno read them
#   arms/<op>.svg           flamegraph, when `inferno-flamegraph` is on PATH
#   arms/<op>.routes.json   "METHOD pattern" → requests, as the router matched
#   arms/<op>.wait.json     where the requests' wall time went (SQL, upstream, …)
#   profile.json            the numbers, and the next run's baseline
#   profile.md              the report
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

SECONDS_PER_ARM="${PROFILE_SECONDS:-15}"
RATE="${PROFILE_RATE:-50}"
FREQ="${PROFILE_FREQ:-199}"
ARM_FILTER="${PROFILE_ARMS:-.}"
BUILD="${PROFILE_BUILD:-release}"
OUT="${PROFILE_OUT:-perf/results/profile}"
PORT="${PROFILE_PORT:-8180}"
UPSTREAM_PORT="${PROFILE_UPSTREAM_PORT:-8181}"
BASE="http://127.0.0.1:$PORT"
AUTH="Authorization: Bearer perf-admin-token"
export SOAK_UPSTREAM_URL="http://127.0.0.1:$UPSTREAM_PORT"

log() { printf '\n==> %s\n' "$*"; }
need() {
  local tool="$1" hint="$2"
  command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool not found on PATH — $hint" >&2; exit 1; }
}
need k6 "mise install k6"
need cargo "rustup"
need curl "curl"
need python3 "python3"
[[ -n "${DATABASE_URL:-}" ]] || { echo "ERROR: DATABASE_URL is required (perf/config.soak.toml reads it)" >&2; exit 1; }

WORK="$(mktemp -d)"
SERVER_PID=""
UPSTREAM_PID=""
cleanup() {
  [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
  [[ -n "$UPSTREAM_PID" ]] && kill "$UPSTREAM_PID" 2>/dev/null || true
  return 0
}
trap cleanup EXIT

rm -rf "$OUT"
mkdir -p "$OUT/arms"

# ── Build ─────────────────────────────────────────────────────────────────────
# The workspace's release profile strips the binary; a profile of a stripped
# binary is a column of hex. Line tables are enough to name every frame and
# cost a fraction of full debuginfo.
case "$BUILD" in
  release) BUILD_FLAGS=(--release); TARGET_DIR=release
           export CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=line-tables-only ;;
  debug)   BUILD_FLAGS=(); TARGET_DIR=debug
           echo "WARNING: PROFILE_BUILD=debug — this smoke-tests the harness; its CPU numbers are not the shipped binary's" >&2 ;;
  *) echo "unknown PROFILE_BUILD: $BUILD (release or debug)" >&2; exit 2 ;;
esac
log "Building the server (profiling, $BUILD) and the mock upstream"
cargo build "${BUILD_FLAGS[@]}" -p batlehub-server --features profiling >"$WORK/build.log" 2>&1 \
  || { tail -40 "$WORK/build.log" >&2; echo "ERROR: the server did not build" >&2; exit 1; }
cargo build "${BUILD_FLAGS[@]}" --manifest-path perf/mock-upstream/Cargo.toml >>"$WORK/build.log" 2>&1 \
  || { tail -40 "$WORK/build.log" >&2; echo "ERROR: the mock upstream did not build" >&2; exit 1; }

# ── Upstream and server ───────────────────────────────────────────────────────
log "Starting the mock upstream on $UPSTREAM_PORT"
"./perf/mock-upstream/target/$TARGET_DIR/mock-upstream" --port "$UPSTREAM_PORT" \
  >"$WORK/upstream.log" 2>&1 &
UPSTREAM_PID=$!
for _ in $(seq 1 30); do curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" && break; sleep 0.5; done
curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" \
  || { cat "$WORK/upstream.log" >&2; echo "ERROR: the mock upstream never came up" >&2; exit 1; }

mkdir -p "$WORK/storage"
export PROXY_CACHE__SERVER__PORT="$PORT" PROXY_CACHE__STORAGE__PATH="$WORK/storage"
log "Starting BatleHub on $PORT"
"./target/$TARGET_DIR/batlehub" --config perf/config.soak.toml >"$OUT/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 120); do
  curl -sf -o /dev/null "$BASE/healthz" && break
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -40 "$OUT/server.log" >&2; echo "ERROR: the server exited" >&2; exit 1; }
  sleep 1
done
curl -sf -o /dev/null -H "$AUTH" "$BASE/debug/routes" \
  || { echo "ERROR: /debug/routes does not answer — is this a build with --features profiling?" >&2; exit 1; }

# ── Pre-flight, which is also the seed ────────────────────────────────────────
# The soak's: every arm once, against its declared statuses. An arm that 404s
# would otherwise profile the 404 path and be reported as cheap.
log "Pre-flight: every arm, once"
BATLEHUB_URL="$BASE" k6 run --quiet perf/k6/scenarios/11_soak_arms.js >"$WORK/arms.log" 2>&1 \
  || { tail -40 "$WORK/arms.log" >&2; echo "ERROR: the pre-flight failed — fix the arm before profiling it" >&2; exit 1; }

# ── One CPU window per arm ────────────────────────────────────────────────────
# k6 runs one second longer on each side of the window, so the window sees
# only steady load: requests in the window are RATE × SECONDS, which a
# constant-arrival-rate executor holds as long as it drops no iteration (the
# report flags an arm that dropped any).
mapfile -t OPS < <(grep -oP '^\s+op: "\K[^"]+' perf/k6/soak_arms.js | grep -E -- "$ARM_FILTER")
[[ ${#OPS[@]} -gt 0 ]] || { echo "ERROR: PROFILE_ARMS=$ARM_FILTER selects no arm" >&2; exit 1; }
log "Profiling ${#OPS[@]} arm(s): ${SECONDS_PER_ARM}s each at ${RATE}/s, sampling at ${FREQ} Hz"
FAILED=()
for op in "${OPS[@]}"; do
  printf '  %-40s' "$op"
  # Drain the pre-flight's and the last arm's counts.
  curl -sf -o /dev/null -H "$AUTH" "$BASE/debug/routes"
  curl -sf -o /dev/null -H "$AUTH" "$BASE/debug/wait"
  BATLEHUB_URL="$BASE" BATLEHUB_PROFILE_ARM="$op" BATLEHUB_PROFILE_RATE="$RATE" \
    BATLEHUB_PROFILE_DURATION="$((SECONDS_PER_ARM + 2))s" \
    k6 run --quiet --summary-export "$OUT/arms/$op.k6.json" perf/k6/scenarios/15_profile_arm.js \
    >"$OUT/arms/$op.k6.log" 2>&1 &
  K6_PID=$!
  sleep 1
  if ! curl -sf -H "$AUTH" -o "$OUT/arms/$op.folded" \
      "$BASE/debug/pprof/folded?seconds=$SECONDS_PER_ARM&frequency=$FREQ"; then
    FAILED+=("$op: the profile request failed")
  fi
  if ! wait "$K6_PID"; then
    FAILED+=("$op: k6 failed — see arms/$op.k6.log")
  fi
  curl -sf -H "$AUTH" -o "$OUT/arms/$op.routes.json" "$BASE/debug/routes"
  curl -sf -H "$AUTH" -o "$OUT/arms/$op.wait.json" "$BASE/debug/wait"
  echo " $(wc -l <"$OUT/arms/$op.folded" 2>/dev/null || echo 0) stacks"
done

if command -v inferno-flamegraph >/dev/null 2>&1; then
  log "Rendering flamegraphs"
  for f in "$OUT"/arms/*.folded; do
    [[ -s "$f" ]] && inferno-flamegraph --title "$(basename "$f" .folded)" <"$f" >"${f%.folded}.svg" || true
  done
else
  log "inferno-flamegraph not on PATH — no SVGs (cargo install inferno); the .folded files open in speedscope"
fi

# ── Report ────────────────────────────────────────────────────────────────────
log "Report"
REPORT_ARGS=(--dir "$OUT" --seconds "$SECONDS_PER_ARM" --rate "$RATE" --frequency "$FREQ")
[[ -n "${PROFILE_BASELINE:-}" && -f "${PROFILE_BASELINE:-}" ]] && REPORT_ARGS+=(--baseline "$PROFILE_BASELINE")
[[ "$ARM_FILTER" != "." ]] && REPORT_ARGS+=(--partial)
STATUS=0
python3 perf/scripts/profile_report.py "${REPORT_ARGS[@]}" || STATUS=$?
if [[ ${#FAILED[@]} -gt 0 ]]; then
  printf '\nERROR: %d arm(s) did not profile:\n  %s\n' "${#FAILED[@]}" "$(printf '%s\n  ' "${FAILED[@]}")" >&2
  STATUS=1
fi
cat "$OUT/profile.md"
exit "$STATUS"
