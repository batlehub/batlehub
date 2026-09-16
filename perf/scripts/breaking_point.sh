#!/usr/bin/env bash
# Find the offered rate at which this server stops coping, and record what every
# rate below it cost. Scenario 14; the report is `perf/results/breaking-point-<label>.md`.
#
# **This is a measurement, not a gate.** It exits non-zero for exactly one
# outcome — the server *died* (a panic, an OOM kill, a process that is no longer
# there). Degrading under a rate no deployment will ever see is the expected
# result and exits 0: the number this produces is "where the knee is", and a
# knee is not a defect.
#
# Usage:
#   bash perf/scripts/breaking_point.sh --label fs-memory
#
#   --budget N      seconds the whole escalation may take (default 1200 = 20 min)
#   --step N        seconds per rate (default 60)
#   --start-rate N  first offered rate (default 100)
#   --factor F      rate multiplier between steps (default 2)
#   --max-fail-pct  a step fails above this % of 5xx/transport errors (default 5)
#   --max-p95-ms    a step fails above this p95 (default 5000)
#   --max-drop-pct  a step fails when k6 could not place this % of its iterations
#                   (default 5) — the server is refusing the offered rate even
#                   when the requests it *did* answer look healthy
#
# Requires: DATABASE_URL, k6, cargo, curl, python3. The backends come from
# `--config`, which is the whole point of the matrix: the same escalation against
# filesystem or S3, memory or Redis, so the knee can be attributed to a backend.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# The config that matches the workload. `config.perf.toml` declares two
# registries and the mix asks for twenty-six, so the escalation against it
# measured 404s: 96.8% errors at 50 req/s, on a server that was fine.
# The backends are varied with `PROXY_CACHE__STORAGE__*` and
# `PROXY_CACHE__CACHE__*` on top of this, which is how the CI matrix
# gets four combinations out of one file.
CONFIG="perf/config.soak.toml"
LABEL=""
BUDGET=1200
STEP=60
START_RATE=100
FACTOR=2
MAX_FAIL_PCT=5
MAX_P95_MS=5000
MAX_DROP_PCT=5
PROFILE="release"
PORT="${BP_PORT:-8180}"
UPSTREAM_PORT="${BP_UPSTREAM_PORT:-8181}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)        CONFIG="$2"; shift 2 ;;
    --label)         LABEL="$2"; shift 2 ;;
    --budget)        BUDGET="$2"; shift 2 ;;
    --step)          STEP="$2"; shift 2 ;;
    --start-rate)    START_RATE="$2"; shift 2 ;;
    --factor)        FACTOR="$2"; shift 2 ;;
    --max-fail-pct)  MAX_FAIL_PCT="$2"; shift 2 ;;
    --max-p95-ms)    MAX_P95_MS="$2"; shift 2 ;;
    --max-drop-pct)  MAX_DROP_PCT="$2"; shift 2 ;;
    --profile)       PROFILE="$2"; shift 2 ;;
    --port)          PORT="$2"; shift 2 ;;
    --upstream-port) UPSTREAM_PORT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -z "$LABEL" ]] && LABEL="$(basename "$CONFIG" .toml | sed 's/^config\.//')"

: "${DATABASE_URL:?DATABASE_URL is required}"
BASE="http://127.0.0.1:$PORT"
RESULTS="perf/results"
WORK="$(mktemp -d)"
mkdir -p "$RESULTS"
SAMPLES="$RESULTS/breaking-point-$LABEL-samples.csv"
REPORT="$RESULTS/breaking-point-$LABEL.md"
ROWS="$WORK/rows.jsonl"
SERVER_LOG="$RESULTS/breaking-point-$LABEL-server.log"
TARGET_DIR="release"; [[ "$PROFILE" == "debug" ]] && TARGET_DIR="debug"
SERVER_PROC=""
: > "$ROWS"

log() { echo "==> $*"; }
cleanup() {
  [[ -n "${SAMPLER_PID:-}" ]] && kill "$SAMPLER_PID" 2>/dev/null
  [[ -n "$SERVER_PROC" ]] && kill "$SERVER_PROC" 2>/dev/null
  pkill -x mock-upstream 2>/dev/null
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

# ── Build and start, the same shape `soak.sh` uses ────────────────────────────
BUILD_FLAGS=(--release); [[ "$PROFILE" == "debug" ]] && BUILD_FLAGS=()
log "Building ($PROFILE)"
cargo build "${BUILD_FLAGS[@]}" -p batlehub-server >"$WORK/build.log" 2>&1 \
  || { cat "$WORK/build.log" >&2; echo "ERROR: the server did not build" >&2; exit 2; }
cargo build "${BUILD_FLAGS[@]}" --manifest-path perf/mock-upstream/Cargo.toml >>"$WORK/build.log" 2>&1 \
  || { cat "$WORK/build.log" >&2; echo "ERROR: the mock upstream did not build" >&2; exit 2; }

export SOAK_UPSTREAM_URL="http://127.0.0.1:$UPSTREAM_PORT"
log "Starting the mock upstream on $UPSTREAM_PORT"
"./perf/mock-upstream/target/$TARGET_DIR/mock-upstream" --port "$UPSTREAM_PORT" \
  --delay-ms 0 --artifact-size-kb 256 --gems 25000 >"$WORK/upstream.log" 2>&1 &
for _ in $(seq 1 30); do curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" && break; sleep 0.5; done

export PROXY_CACHE__SERVER__PORT="$PORT"
export PROXY_CACHE__STORAGE__PATH="$WORK/storage"
mkdir -p "$PROXY_CACHE__STORAGE__PATH"
log "Starting BatleHub on $PORT with $CONFIG"
setsid "./target/$TARGET_DIR/batlehub" --config "$CONFIG" >"$SERVER_LOG" 2>&1 &
for _ in $(seq 1 120); do curl -sf -o /dev/null "$BASE/healthz" && break; sleep 1; done
curl -sf -o /dev/null "$BASE/healthz" \
  || { tail -40 "$SERVER_LOG" >&2; echo "ERROR: the server did not become healthy" >&2; exit 2; }
for _ in $(seq 1 10); do
  for pid in $(pgrep -x batlehub 2>/dev/null); do
    tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q -- "$CONFIG" && { SERVER_PROC="$pid"; break 2; }
  done; sleep 1
done
[[ -n "$SERVER_PROC" ]] || { echo "ERROR: no server process to measure" >&2; exit 2; }
log "Healthy at $BASE (measuring pid $SERVER_PROC)"

# ── The sampler ───────────────────────────────────────────────────────────────
# CPU% from /proc deltas against /proc/uptime, not from `date +%s%3N`: that is a
# GNU extension this image does not honour — it prints nanoseconds, and every
# interval came out a million times too long (the bug `record_run.py` fixed).
echo "epoch_s,rss_kb,cpu_pct,threads,fds" > "$SAMPLES"
(
  TICKS=$(getconf CLK_TCK)
  prev_cpu=""; prev_up=""
  while kill -0 "$SERVER_PROC" 2>/dev/null; do
    up="$(awk '{print $1; exit}' /proc/uptime)"
    read -r _ _ _ _ _ _ _ _ _ _ _ _ _ utime stime _ < "/proc/$SERVER_PROC/stat" 2>/dev/null || break
    cpu=$(( utime + stime ))
    pct=""
    if [[ -n "$prev_cpu" ]]; then
      pct="$(awk -v c="$cpu" -v p="$prev_cpu" -v u="$up" -v q="$prev_up" -v t="$TICKS" \
        'BEGIN{d=u-q; if(d<=0){print ""} else {printf "%.1f", (c-p)/t/d*100}}')"
    fi
    rss="$(awk '/^VmRSS:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null)"
    threads="$(awk '/^Threads:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null)"
    fds="$(ls "/proc/$SERVER_PROC/fd" 2>/dev/null | wc -l)"
    [[ -n "$rss" ]] && echo "$(date +%s),$rss,$pct,$threads,$fds" >> "$SAMPLES"
    prev_cpu="$cpu"; prev_up="$up"
    sleep 1
  done
) &
SAMPLER_PID=$!

# ── Pre-flight, which is also the seed ────────────────────────────────────────
#
# Every arm once, asserted against the statuses it declares, before any rate is
# offered. Two things it stops, both of which produce a plausible-looking number
# instead of an error: an arm whose registry is not in this config answers 404
# and the load's own check passes it, and a first-touch miss on every document
# would be charged to the first step as if it were the server's steady cost.
log "Pre-flight: every arm of the mix, once"
if ! BATLEHUB_URL="$BASE" k6 run --quiet perf/k6/scenarios/11_soak_arms.js >"$WORK/arms.log" 2>&1; then
  sed -n '/soak arms did not answer as declared/,$p' "$WORK/arms.log" >&2 || tail -40 "$WORK/arms.log" >&2
  echo "ERROR: the mix does not work against $CONFIG — see $WORK/arms.log" >&2
  exit 2
fi

# ── The escalation ────────────────────────────────────────────────────────────
STARTED="$(date +%s)"
rate="$START_RATE"
broke_at=""
break_reason=""
while :; do
  spent=$(( $(date +%s) - STARTED ))
  left=$(( BUDGET - spent ))
  if (( left < STEP )); then
    log "Budget exhausted after ${spent}s — stopping at $rate req/s without a break"
    break
  fi
  log "Step: ${rate} req/s for ${STEP}s (${left}s of budget left)"
  step_start="$(date +%s)"
  BATLEHUB_URL="$BASE" BATLEHUB_BP_RATE="$rate" BATLEHUB_BP_STEP="${STEP}s" \
    k6 run --quiet --summary-export "$WORK/step-$rate.json" \
      --summary-trend-stats "min,med,p(95),p(98),max" \
      perf/k6/scenarios/14_breaking_point.js >"$WORK/k6-$rate.log" 2>&1
  step_end="$(date +%s)"

  if ! kill -0 "$SERVER_PROC" 2>/dev/null; then
    broke_at="$rate"; break_reason="the server process died"
    python3 perf/scripts/breaking_point_row.py --rate "$rate" --summary "$WORK/step-$rate.json" \
      --samples "$SAMPLES" --from "$step_start" --to "$step_end" --died --append "$ROWS" >/dev/null
    break
  fi

  verdict="$(python3 perf/scripts/breaking_point_row.py --rate "$rate" \
    --summary "$WORK/step-$rate.json" --samples "$SAMPLES" \
    --from "$step_start" --to "$step_end" \
    --max-fail-pct "$MAX_FAIL_PCT" --max-p95-ms "$MAX_P95_MS" --max-drop-pct "$MAX_DROP_PCT" \
    --append "$ROWS")"
  log "  $verdict"
  if [[ "$verdict" == FAILED* ]]; then
    broke_at="$rate"; break_reason="${verdict#FAILED — }"
    break
  fi
  rate=$(( rate * FACTOR ))
done

# `grep -c` prints 0 *and* exits 1 when it matches nothing, so a `|| echo 0`
# appends a second line and every arithmetic test on it dies. Default the
# variable instead of appending to it.
panics="$(grep -c "panicked at" "$SERVER_LOG" 2>/dev/null)"
panics="${panics:-0}"
alive=1; kill -0 "$SERVER_PROC" 2>/dev/null || alive=0

python3 perf/scripts/breaking_point_report.py \
  --label "$LABEL" --config "$CONFIG" --rows "$ROWS" --report "$REPORT" \
  --broke-at "${broke_at:-}" --reason "${break_reason:-}" \
  --panics "$panics" --alive "$alive" --budget "$BUDGET" --step "$STEP" --factor "$FACTOR"

log "Report: $REPORT   Samples: $SAMPLES"
if (( panics > 0 )) || (( alive == 0 )); then
  echo "ERROR: the server did not survive the escalation — see $SERVER_LOG" >&2
  exit 1
fi
exit 0
