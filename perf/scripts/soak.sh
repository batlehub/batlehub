#!/usr/bin/env bash
# Soak the server under constant load and report whether it gave anything back.
#
# This is the leak gate. It is not a benchmark: the numbers it prints are not
# about how fast anything is, they are about whether the process is the same
# size, holds the same descriptors, runs the same threads and has returned the
# same database connections after an hour of traffic as it did before.
#
#   warm-up load ──▶ quiesce ──▶ BASELINE ──▶ steady load ──▶ quiesce ──▶ FINAL
#
# **Both measurement windows are idle**, and the baseline is taken *after* a
# warm-up rather than at startup. That ordering is the whole design:
#
#   - measuring at startup would report every lazily-filled cache, connection
#     pool and allocator arena as a leak, so a first run would fail and the
#     threshold would then be raised until nothing could fail;
#   - measuring under load would compare a number that includes in-flight
#     request buffers against one that does not, which is a different quantity
#     in each window and cannot be subtracted;
#   - quiescing before each window lets the server drain and — with the
#     `jemalloc` feature that is on by default — lets jemalloc's decay return
#     dirty pages, which takes seconds and would otherwise read as growth.
#
# What is left after that is what was still held when nothing was in flight,
# which is what "leak" means.
#
# Usage:
#   bash perf/scripts/soak.sh [--duration 10m] [--rate 100] [--warmup 60]
#                             [--settle 60] [--report perf/results/soak.md]
#                             [--profile release|debug]
#
# `--profile debug` exists to smoke-test *this script* — it skips a release
# build that takes tens of minutes. The verdict it prints is about a binary
# whose allocation behaviour is not the one that ships, so it is never a
# leak result. CI uses the default.
#
# Requires: DATABASE_URL, k6, cargo, curl, python3.
# Thresholds (environment, all "fail above"):
#   SOAK_MAX_RSS_GROWTH_PCT        default 10    idle RSS, final vs baseline
#   SOAK_MAX_RSS_SLOPE_MIB_PER_MIN default 2.0   trend during the steady load
#   SOAK_MAX_FD_GROWTH             default 16    open descriptors
#   SOAK_MAX_THREAD_GROWTH         default 4     OS threads
#   SOAK_MAX_POOL_GROWTH           default 2     database connections held
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

DURATION="10m"
RATE="100"
WARMUP="60"
SETTLE="60"
REPORT="perf/results/soak.md"
PROFILE="release"
UPSTREAM_DELAY_MS="${SOAK_UPSTREAM_DELAY_MS:-0}"
ARTIFACT_KB="${SOAK_ARTIFACT_KB:-256}"
# Gems in the mock's compact index. 25 000 is `corpus-seed --size m`: a document
# of a few MB, which is what makes the RubyGems registry in this soak cost
# something different from the npm one rather than the same thing again.
GEMS="${SOAK_GEMS:-25000}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --duration) DURATION="$2"; shift 2 ;;
    --rate)     RATE="$2";     shift 2 ;;
    --warmup)   WARMUP="$2";   shift 2 ;;
    --settle)   SETTLE="$2";   shift 2 ;;
    --report)   REPORT="$2";   shift 2 ;;
    --profile)  PROFILE="$2";  shift 2 ;;
    -h|--help)  sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

: "${DATABASE_URL:?DATABASE_URL must point at a reachable Postgres}"

SOAK_PORT="${SOAK_PORT:-8180}"
SOAK_UPSTREAM_PORT="${SOAK_UPSTREAM_PORT:-8181}"
export SOAK_UPSTREAM_URL="http://127.0.0.1:$SOAK_UPSTREAM_PORT"
BASE="http://127.0.0.1:$SOAK_PORT"

WORK="$(mktemp -d)"
SAMPLES="$WORK/samples.csv"
MARKS="$WORK/marks.txt"
SERVER_LOG="$WORK/server.log"
UPSTREAM_LOG="$WORK/upstream.log"
SERVER_PID=""
SERVER_PROC=""
UPSTREAM_PID=""
SAMPLER_PID=""

log() { printf '\n==> %s\n' "$*"; }
# Whole seconds, from `date +%s`. **Not `date +%s%3N`**: the millisecond width
# is a GNU extension that uutils-coreutils (the `date` on some images, and on
# the machine this was written on) silently ignores, printing full nanoseconds
# instead — and when the nanosecond field has a leading zero the concatenation
# is one digit shorter, so the sequence is not even monotonic. Measured: the
# final window matched no samples at all and the report said "not measured"
# for every row, which is a soak that cannot fail. A window here is tens of
# seconds wide, so seconds are resolution enough.
mark() { echo "$1 $(date +%s)" >> "$MARKS"; }

cleanup() {
  [[ -n "$SAMPLER_PID" ]] && kill "$SAMPLER_PID" 2>/dev/null || true
  # Both the backgrounded process *and* the resolved server, because they are
  # not reliably the same one: `setsid` forks when it is already a process
  # group leader, and then `$!` is a process that exited a moment later while
  # the server it created is reparented to init. Killing only `$!` leaves a
  # server holding the port, which the next run meets as "address in use".
  [[ -n "$SERVER_PROC" ]] && kill -TERM "$SERVER_PROC" 2>/dev/null || true
  [[ -n "$SERVER_PID" ]] && kill -TERM -- "-$SERVER_PID" 2>/dev/null || true
  [[ -n "$UPSTREAM_PID" ]] && kill "$UPSTREAM_PID" 2>/dev/null || true
  sleep 1
  [[ -n "$SERVER_PROC" ]] && kill -KILL "$SERVER_PROC" 2>/dev/null || true
  [[ -n "$SERVER_PID" ]] && kill -KILL -- "-$SERVER_PID" 2>/dev/null || true
  return 0
}
trap cleanup EXIT

need() {
  command -v "$1" >/dev/null 2>&1 \
    || { echo "ERROR: $1 not found on PATH — install it ($2)" >&2; exit 1; }
}
need k6 "mise install k6"
need cargo "rustup"
need curl "curl"
need python3 "python3"

# ── Build ─────────────────────────────────────────────────────────────────────
# `--release`, and the binary is run directly rather than through `cargo run`:
# the sampler needs the server's own PID, and under `cargo run` that is a
# grandchild of a wrapper whose PID is the one the shell knows. A debug build
# would also make the measurement meaningless — its allocation behaviour is not
# the one that ships.
case "$PROFILE" in
  release) BUILD_FLAGS=(--release); TARGET_DIR="release" ;;
  debug)   BUILD_FLAGS=();          TARGET_DIR="debug"
           echo "WARNING: --profile debug — this smoke-tests the harness; its numbers are not a leak result" >&2 ;;
  *) echo "unknown profile: $PROFILE (use release or debug)" >&2; exit 2 ;;
esac

log "Building the server and the mock upstream ($PROFILE)"
cargo build "${BUILD_FLAGS[@]}" -p batlehub-server >"$WORK/build.log" 2>&1 \
  || { cat "$WORK/build.log" >&2; echo "ERROR: the server did not build" >&2; exit 1; }
cargo build "${BUILD_FLAGS[@]}" --manifest-path perf/mock-upstream/Cargo.toml >>"$WORK/build.log" 2>&1 \
  || { cat "$WORK/build.log" >&2; echo "ERROR: the mock upstream did not build" >&2; exit 1; }

# ── The upstream ──────────────────────────────────────────────────────────────
log "Starting the mock upstream on $SOAK_UPSTREAM_PORT"
"./perf/mock-upstream/target/$TARGET_DIR/mock-upstream" \
  --port "$SOAK_UPSTREAM_PORT" --delay-ms "$UPSTREAM_DELAY_MS" \
  --artifact-size-kb "$ARTIFACT_KB" --gems "$GEMS" >"$UPSTREAM_LOG" 2>&1 &
UPSTREAM_PID=$!
for _ in $(seq 1 30); do
  curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" && break
  sleep 0.5
done
curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" \
  || { cat "$UPSTREAM_LOG" >&2; echo "ERROR: the mock upstream never came up" >&2; exit 1; }

# ── The server ────────────────────────────────────────────────────────────────
STORAGE="$WORK/storage"
mkdir -p "$STORAGE"
export PROXY_CACHE__SERVER__PORT="$SOAK_PORT"
export PROXY_CACHE__STORAGE__PATH="$STORAGE"

log "Starting BatleHub on $SOAK_PORT"
setsid "./target/$TARGET_DIR/batlehub" --config perf/config.soak.toml \
  >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 120); do
  curl -sf -o /dev/null "$BASE/healthz" && break
  kill -0 "$SERVER_PID" 2>/dev/null || grep -q "listening" "$SERVER_LOG" 2>/dev/null \
    || { cat "$SERVER_LOG" >&2; echo "ERROR: the server exited before becoming healthy" >&2; exit 1; }
  sleep 1
done
curl -sf -o /dev/null "$BASE/healthz" \
  || { tail -40 "$SERVER_LOG" >&2; echo "ERROR: the server did not become healthy" >&2; exit 1; }
# The process to measure, resolved rather than assumed. `$!` is whatever the
# shell backgrounded — `setsid` above, or the server, depending on whether the
# shell put the job in its own process group — and `/proc/<the wrong one>`
# answers every question with a plausible number instead of an error. A soak
# that samples the wrong process reports a clean run for a leaking server,
# which is the one outcome worse than a red one.
#
# `pgrep -x` matches the executable name; `-f` would also match this script.
for _ in $(seq 1 10); do
  for pid in $(pgrep -x batlehub 2>/dev/null || true); do
    if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q "perf/config.soak.toml"; then
      SERVER_PROC="$pid"
      break 2
    fi
  done
  sleep 1
done
[[ -n "$SERVER_PROC" && -d "/proc/$SERVER_PROC" ]] \
  || { echo "ERROR: the server answers /healthz but no 'batlehub --config perf/config.soak.toml' process could be found to measure" >&2; exit 1; }
log "Healthy at $BASE (measuring pid $SERVER_PROC)"

# `/metrics` is what the pool numbers come from. A server built without it, or
# with `metrics_enabled = false`, would silently report a flat zero for every
# connection gauge — which is indistinguishable from a pool that never grows.
curl -sf "$BASE/metrics" | grep -q batlehub_db_pool_size \
  || { echo "ERROR: /metrics does not expose batlehub_db_pool_size — the pool leak check cannot run" >&2; exit 1; }

# ── Seed ──────────────────────────────────────────────────────────────────────
# One read of each document the mix asks for, so the warm-up measures steady
# behaviour rather than first-touch. Failures here are configuration errors —
# a wrong token, a registry that is not declared — and are worth naming now
# rather than as a wall of k6 check failures.
log "Seeding"
AUTH=(-H "Authorization: Bearer perf-admin-token")
for url in \
  "$BASE/proxy/perf-npm/perf-pkg" \
  "$BASE/proxy/perf-npm/perf-pkg/1.0.0/tarball" \
  "$BASE/api/v1/packages?registry=perf-npm&limit=50" \
  "$BASE/proxy/perf-gems/versions" \
  "$BASE/proxy/perf-gems/info/perf-gem-0000001" ; do
  code="$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" "$url")"
  [[ "$code" == "200" ]] || { echo "ERROR: seed request $url answered $code" >&2; exit 1; }
done

# ── The sampler ───────────────────────────────────────────────────────────────
# /proc for the process facts and /metrics for the pool. One line a second:
# a soak is a curve, and the verdict below needs the points, not a summary.
sample_header="epoch_s,rss_kb,fds,threads,pool_size,pool_idle"
echo "$sample_header" > "$SAMPLES"
(
  while kill -0 "$SERVER_PROC" 2>/dev/null; do
    rss="$(awk '/^VmRSS:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null || echo "")"
    threads="$(awk '/^Threads:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null || echo "")"
    # `ls` rather than a glob: the directory changes while it is being read and
    # a failed glob under `set -e` would end the sampler silently.
    fds="$(ls "/proc/$SERVER_PROC/fd" 2>/dev/null | wc -l || echo "")"
    pool="$(curl -s --max-time 5 "$BASE/metrics" 2>/dev/null \
      | awk '/^batlehub_db_pool_size/{s=$2} /^batlehub_db_pool_available_connections/{i=$2} END{printf "%s,%s", (s==""?"":s), (i==""?"":i)}')"
    if [[ -n "$rss" ]]; then
      echo "$(date +%s),$rss,$fds,$threads,${pool:-,}" >> "$SAMPLES"
    fi
    sleep 1
  done
) &
SAMPLER_PID=$!

# scrape_metrics <name> — the whole of /metrics, kept for the verdict.
#
# The per-registry accounting comes from here rather than from k6, because the
# server is the only party that knows what a request *cost* it: k6 can say how
# many requests went to a registry and how many bytes came back, and neither is
# the same as how long the server spent or how much it pulled from upstream to
# answer them. Two scrapes and a subtraction, at the boundaries of the load, so
# the warm-up's traffic is not counted against the steady phase.
scrape_metrics() {
  local name="$1"
  curl -s --max-time 10 "$BASE/metrics" > "$WORK/metrics-$name.txt" || true
}

run_k6() {  # <phase> <duration>
  local phase="$1" duration="$2"
  log "k6 — $phase phase: ${duration} at ${RATE} req/s"
  BATLEHUB_URL="$BASE" \
  BATLEHUB_SOAK_DURATION="$duration" \
  BATLEHUB_SOAK_RATE="$RATE" \
  BATLEHUB_SOAK_PHASE="$phase" \
    k6 run --summary-export "$WORK/k6-$phase.json" perf/k6/scenarios/10_soak.js
}

quiesce() {  # <seconds> — no load, so the next window measures what is *held*
  log "Quiescing for ${1}s"
  sleep "$1"
}

# ── The run ───────────────────────────────────────────────────────────────────
mark warmup_start
run_k6 warmup "${WARMUP}s" || true   # the warm-up's thresholds are not a verdict
mark warmup_end
quiesce "$SETTLE"
mark baseline_end

mark steady_start
scrape_metrics steady-start
set +e
run_k6 steady "$DURATION"
K6_EXIT=$?
set -e
scrape_metrics steady-end
mark steady_end
quiesce "$SETTLE"
mark final_end

# Stop sampling, but leave the server up: the verdict reads /proc one last time
# through the samples it already has, and a killed server would truncate them.
kill "$SAMPLER_PID" 2>/dev/null || true
wait "$SAMPLER_PID" 2>/dev/null || true
SAMPLER_PID=""

# ── The verdict ───────────────────────────────────────────────────────────────
mkdir -p "$(dirname "$REPORT")"
cp "$SAMPLES" "$(dirname "$REPORT")/soak-samples.csv"
cp "$MARKS" "$(dirname "$REPORT")/soak-marks.txt"

set +e
python3 perf/scripts/soak_verdict.py \
  --samples "$SAMPLES" \
  --marks "$MARKS" \
  --server-log "$SERVER_LOG" \
  --k6-summary "$WORK/k6-steady.json" \
  --k6-exit "$K6_EXIT" \
  --metrics-before "$WORK/metrics-steady-start.txt" \
  --metrics-after "$WORK/metrics-steady-end.txt" \
  --duration "$DURATION" \
  --rate "$RATE" \
  --chart "$(dirname "$REPORT")/soak-chart.svg" \
  --report "$REPORT"
VERDICT=$?
set -e

cat "$REPORT"
[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && cat "$REPORT" >> "$GITHUB_STEP_SUMMARY"

log "Samples: $(dirname "$REPORT")/soak-samples.csv   Report: $REPORT"
exit "$VERDICT"
