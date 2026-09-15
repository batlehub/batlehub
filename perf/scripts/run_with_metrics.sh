#!/usr/bin/env bash
# Wraps `k6 run`, appends a resource summary (min/median/max RAM and CPU) for
# the batlehub server process, and records both halves as one row of the
# results table (`perf/results/runs.jsonl` → `task perf:report`).
#
# Usage: bash perf/scripts/run_with_metrics.sh [--label NAME] [k6-flags...] <scenario.js>
#   e.g. bash perf/scripts/run_with_metrics.sh --no-thresholds perf/k6/scenarios/02_warm_read.js
#
# RSS is read from /proc/{PID}/status (accurate, Linux-only).
# CPU% is computed from /proc/{PID}/stat deltas so it is instantaneous
# (same method as `top`), not the lifetime average that `ps pcpu` reports.
#
# Environment read by the recorder, so a row says what it was measured against:
#   PERF_BACKEND   "filesystem+memory" (default) | "s3+redis" | …
#   PERF_PROFILE   "release" (default)
#   PERF_VERSION   overrides `git describe` in the row
#   PERF_NOTE      free text carried into the report
#   PERF_RESULTS_DIR  where runs.jsonl lands (default: perf/results)
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
RESULTS_DIR="${PERF_RESULTS_DIR:-$REPO_ROOT/perf/results}"
RUNS_FILE="$RESULTS_DIR/runs.jsonl"

# ── Arguments ─────────────────────────────────────────────────────────────────
# Everything except --label is k6's; the label defaults to the scenario's
# basename, which is what makes `01_at_rest` the row's name without every task
# having to spell it.
LABEL=""
K6_ARGS=()
SCENARIO=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --label) LABEL="${2:-}"; shift 2 ;;
    --label=*) LABEL="${1#*=}"; shift ;;
    *)
      [[ "$1" == *.js ]] && SCENARIO="$1"
      K6_ARGS+=("$1")
      shift
      ;;
  esac
done
if [[ -z "$LABEL" && -n "$SCENARIO" ]]; then
  LABEL=$(basename "$SCENARIO" .js)
fi
LABEL="${LABEL:-k6}"

mkdir -p "$RESULTS_DIR"
SUMMARY_JSON="$RESULTS_DIR/${LABEL}.k6.json"
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%S+00:00)

# p(99) is not in k6's default trend stats and is the one that shows a tail
# widening while p95 holds still — the shape every bottleneck in §9 of the
# README makes first.
K6_ARGS=(
  --summary-export "$SUMMARY_JSON"
  --summary-trend-stats "avg,min,med,max,p(90),p(95),p(99)"
  "${K6_ARGS[@]}"
)

record() {
  local pid="$1" rss_file="$2" cpu_file="$3" exit_code="$4"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "  [resource-monitor] python3 not found — not recording this run in $RUNS_FILE"
    return 0
  fi
  local args=(
    --label "$LABEL"
    --scenario "$SCENARIO"
    --summary "$SUMMARY_JSON"
    --pid "$pid"
    --k6-exit "$exit_code"
    --started-at "$STARTED_AT"
    --out "$RUNS_FILE"
  )
  [[ -n "$rss_file" ]] && args+=(--rss-file "$rss_file")
  [[ -n "$cpu_file" ]] && args+=(--cpu-file "$cpu_file")
  python3 "$REPO_ROOT/perf/scripts/record_run.py" "${args[@]}" || true
}

# ── Locate the server process ─────────────────────────────────────────────────
# Accept an explicit PID via env var to bypass auto-detection.
if [[ -n "${BATLEHUB_PID:-}" ]]; then
  PID="$BATLEHUB_PID"
else
  # 1. Exact binary name match (works for both `cargo run` and direct invocation).
  # 2. Fall back to full-path match in case multiple binaries share the name.
  # Using two separate pgrep calls avoids ERE alternation portability issues.
  PID=$(pgrep -x "batlehub" 2>/dev/null | head -1 || \
        pgrep -f "target/release/batlehub" 2>/dev/null | head -1 || true)
fi

if [[ -z "$PID" ]]; then
  echo "  [resource-monitor] batlehub process not found — run 'task perf:server' first"
  echo "  [resource-monitor] tip: set BATLEHUB_PID=<pid> to pin the process manually"
  echo "  [resource-monitor] skipping resource metrics, running k6 only"
  # Still recorded, with no resource columns: a run against a remote server
  # (§10) has throughput numbers worth keeping, and a row that admits it has no
  # RSS is better than a table that silently skips the run.
  set +e
  k6 run "${K6_ARGS[@]}"
  K6_EXIT=$?
  set -e
  record 0 "" "" "$K6_EXIT"
  exit $K6_EXIT
fi

CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)
RSS_FILE=$(mktemp /tmp/batlehub-rss.XXXXXX)
CPU_FILE=$(mktemp /tmp/batlehub-cpu.XXXXXX)
trap 'rm -f "$RSS_FILE" "$CPU_FILE"' EXIT

echo "  [resource-monitor] tracking PID $PID  CLK_TCK=$CLK_TCK"

# ── Background sampler ────────────────────────────────────────────────────────
#
# The clock is `/proc/uptime` and **not** `date +%s%3N`. That format string is a
# GNU extension, and where it is not honoured — this workspace's image among
# them — `date` prints *nanoseconds*, so every interval came out a million times
# too long and the CPU column was 0.00 % for every scenario ever run. It was
# only caught by looking at a server that was demonstrably pinning a core while
# the box under the k6 output said it was idle.
#
# `/proc/uptime` is two floats and has no format string to get wrong; the
# arithmetic moves into awk, which handles them as floats where bash could not.
(
  prev_ticks=0
  prev_up=""

  while kill -0 "$PID" 2>/dev/null; do
    # RSS in kB (from /proc — always instantaneous)
    rss=$(awk '/VmRSS/{print $2; exit}' "/proc/$PID/status" 2>/dev/null || echo "")

    # Instantaneous CPU% from the tick delta between samples. Field 14 + 15 of
    # /proc/{pid}/stat are utime + stime for the whole thread group, so this is
    # the server's total CPU and not one thread's.
    curr_ticks=$(awk '{print $14+$15}' "/proc/$PID/stat" 2>/dev/null || echo 0)
    read -r curr_up _ < /proc/uptime

    if [[ -n "$prev_up" ]] && [[ -n "$rss" ]]; then
      tick_delta=$(( curr_ticks - prev_ticks ))
      cpu=$(awk -v td="$tick_delta" -v t0="$prev_up" -v t1="$curr_up" -v clk="$CLK_TCK" \
        'BEGIN { dt = t1 - t0; if (dt > 0) printf "%.2f", (td/clk)/dt*100; else print "0" }')
      echo "$rss" >> "$RSS_FILE"
      echo "$cpu"  >> "$CPU_FILE"
    fi

    prev_ticks=$curr_ticks
    prev_up=$curr_up
    sleep 1
  done
) &
SAMPLER_PID=$!

# ── Run k6 ────────────────────────────────────────────────────────────────────
set +e
k6 run "${K6_ARGS[@]}"
K6_EXIT=$?
set -e

kill "$SAMPLER_PID" 2>/dev/null
wait "$SAMPLER_PID" 2>/dev/null || true

# ── Print resource summary ────────────────────────────────────────────────────
N=$(wc -l < "$RSS_FILE" 2>/dev/null | tr -d ' ')
N=${N:-0}

echo ""
echo "┌─ Resource usage — PID $PID — $N samples @ 1 s ─────────────────────────────"

awk_stats() {
  local file="$1" label="$2" divisor="$3" unit="$4"
  if [[ ! -s "$file" ]]; then
    printf "│  %-14s no data\n" "$label"
    return
  fi
  awk -v lbl="$label" -v div="$divisor" -v unit="$unit" '
    {
      v = $1 / div
      a[NR] = v
      sum += v
      if (NR == 1 || v < mn) mn = v
      if (NR == 1 || v > mx) mx = v
    }
    END {
      n = NR
      # insertion sort for median
      for (i = 2; i <= n; i++) {
        k = a[i]; j = i - 1
        while (j >= 1 && a[j] > k) { a[j+1] = a[j]; j-- }
        a[j+1] = k
      }
      med = (n % 2 == 1) ? a[int(n/2)+1] : (a[n/2] + a[n/2+1]) / 2
      printf "│  %-14s  min=%8.1f   median=%8.1f   max=%8.1f   %s\n", \
             lbl, mn, med, mx, unit
    }
  ' "$file"
}

awk_stats "$RSS_FILE" "RAM (RSS)"  1024  "MiB"
awk_stats "$CPU_FILE" "CPU"        1     "%"

echo "└────────────────────────────────────────────────────────────────────────────"

record "$PID" "$RSS_FILE" "$CPU_FILE" "$K6_EXIT"

exit $K6_EXIT
