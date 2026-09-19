#!/usr/bin/env bash
# How long the process takes to become useful, and how long it takes to stop.
#
# Neither number is visible in any other scenario here: the soak and the k6 runs
# all start a server, wait for `/healthz` and then measure what happens *after*
# that, so the two ends of a process's life — the part a rolling deployment
# spends unable to serve, and the part it spends refusing to die — are exactly
# the parts nothing measured.
#
# Four measurements, because they fail for different reasons:
#
#   cold start   exec ─▶ first 200 on /healthz, against a database with no
#                schema. Includes the migrations, which is the number that
#                decides whether a fresh replica joins a rollout or times out
#                its readiness probe.
#   warm start   the same against a database already migrated — what every
#                restart after the first one costs. The *difference* between the
#                two is the migration cost, measured rather than parsed out of a
#                log.
#   idle stop    SIGTERM ─▶ process gone, with nothing in flight. The floor.
#   draining stop  SIGTERM ─▶ process gone with N slow requests in flight. This
#                is the one a `terminationGracePeriodSeconds` has to cover:
#                actix stops accepting immediately and then waits for in-flight
#                requests (up to its `shutdown_timeout`), so a proxy streaming a
#                large artifact from a slow upstream is what sets this number,
#                not the process's own teardown.
#
# Each start is also timed to the moment the port *accepts*, separately from the
# moment `/healthz` answers: a server that binds early and is not ready is a
# server a load balancer sends traffic to, and the gap between those two is how
# long that window is.
#
# Usage:
#   bash perf/scripts/lifecycle.sh [--iterations 5] [--in-flight 20]
#                                  [--upstream-delay-ms 3000]
#                                  [--profile release|debug]
#
# The report is always `perf/results/lifecycle.md`, beside the raw per-iteration
# samples — same construction as the soak, and for the same reason: no path
# crosses a process boundary.
#
# Requires: DATABASE_URL (a server it may CREATE and DROP databases on), cargo,
# curl, python3.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

ITERATIONS=5
IN_FLIGHT=20
PROFILE="release"
# The upstream is slow on purpose: the draining measurement needs requests that
# are still in flight when the signal lands, and against a 0 ms mock every
# request has finished before `kill` returns.
UPSTREAM_DELAY_MS=3000

while [[ $# -gt 0 ]]; do
  case "$1" in
    --iterations)        ITERATIONS="$2";        shift 2 ;;
    --in-flight)         IN_FLIGHT="$2";         shift 2 ;;
    --upstream-delay-ms) UPSTREAM_DELAY_MS="$2"; shift 2 ;;
    --profile)           PROFILE="$2";           shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

: "${DATABASE_URL:?set DATABASE_URL (e.g. postgresql://batlehub:changeme@127.0.0.1:5432/batlehub)}"

PORT="${LIFECYCLE_PORT:-8190}"
UPSTREAM_PORT="${LIFECYCLE_UPSTREAM_PORT:-8191}"
BASE="http://127.0.0.1:$PORT"
export SOAK_UPSTREAM_URL="http://127.0.0.1:$UPSTREAM_PORT"

RESULTS="perf/results"
mkdir -p "$RESULTS"
SAMPLES="$RESULTS/lifecycle-samples.csv"
REPORT="$RESULTS/lifecycle.md"

WORK="$(mktemp -d)"
UPSTREAM_PID=""
SERVER_PROC=""
# The throwaway database for the cold measurement: named per run, because two
# runs sharing one would make the second "cold" start a warm one.
COLD_DB="lifecycle_$(date +%s)"

log() { printf '\n==> %s\n' "$*"; }

# Milliseconds. `%N` rather than `%3N`: the width modifier is a GNU extension
# that uutils-coreutils ignores, printing full nanoseconds — so the arithmetic,
# not the format string, does the truncation.
now_ms() { echo $(( $(date +%s%N) / 1000000 )); }

psql_do() {  # <sql> — no psql on this image, and none needed for two statements
  uv run --quiet --with "psycopg[binary]" python -c "
import os, sys, psycopg
c = psycopg.connect(os.environ['ADMIN_DATABASE_URL'], autocommit=True)
c.execute(sys.argv[1])" "$1"
}

cleanup() {
  [[ -n "$SERVER_PROC" ]] && kill -KILL "$SERVER_PROC" 2>/dev/null || true
  [[ -n "$UPSTREAM_PID" ]] && kill "$UPSTREAM_PID" 2>/dev/null || true
  psql_do "DROP DATABASE IF EXISTS $COLD_DB" 2>/dev/null || true
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

need() {
  command -v "$1" >/dev/null 2>&1 \
    || { echo "ERROR: $1 not found on PATH — install it ($2)" >&2; exit 1; }
}
need cargo "rustup"
need curl "curl"
need python3 "python3"
need uv "mise install"

case "$PROFILE" in
  release) BUILD_FLAGS=(--release); TARGET_DIR="release" ;;
  debug)   BUILD_FLAGS=();          TARGET_DIR="debug"
           echo "WARNING: --profile debug — a debug binary's startup is not the one that ships" >&2 ;;
  *) echo "unknown profile: $PROFILE (use release or debug)" >&2; exit 2 ;;
esac

log "Building the server and the mock upstream ($PROFILE)"
cargo build "${BUILD_FLAGS[@]}" -p batlehub-server >"$WORK/build.log" 2>&1 \
  || { cat "$WORK/build.log" >&2; echo "ERROR: the server did not build" >&2; exit 1; }
cargo build "${BUILD_FLAGS[@]}" --manifest-path perf/mock-upstream/Cargo.toml >>"$WORK/build.log" 2>&1 \
  || { cat "$WORK/build.log" >&2; echo "ERROR: the mock upstream did not build" >&2; exit 1; }
SERVER_BIN="./target/$TARGET_DIR/batlehub"

# The admin URL is the configured one with its database swapped for `postgres`,
# so `CREATE DATABASE` does not run inside the database being created.
export ADMIN_DATABASE_URL="${DATABASE_URL%/*}/postgres"

log "Starting the mock upstream on $UPSTREAM_PORT (delay ${UPSTREAM_DELAY_MS}ms)"
"./perf/mock-upstream/target/$TARGET_DIR/mock-upstream" \
  --port "$UPSTREAM_PORT" --delay-ms "$UPSTREAM_DELAY_MS" >"$WORK/upstream.log" 2>&1 &
UPSTREAM_PID=$!
for _ in $(seq 1 60); do
  curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" && break
  sleep 0.5
done
curl -sf -o /dev/null "$SOAK_UPSTREAM_URL/health" \
  || { cat "$WORK/upstream.log" >&2; echo "ERROR: the mock upstream never came up" >&2; exit 1; }

echo "phase,iteration,accept_ms,healthy_ms,stop_ms,in_flight" > "$SAMPLES"

# start_server <database url> <log file> — returns once the process exists,
# recording `T_EXEC`, `T_ACCEPT` and `T_HEALTHY`. The two probes are separate
# because they answer different questions: the port accepting is the moment a
# load balancer can route to this process, `/healthz` the moment it should.
start_server() {
  local db_url="$1" logfile="$2"
  local storage; storage="$WORK/storage-$RANDOM"
  mkdir -p "$storage"

  T_EXEC="$(now_ms)"
  DATABASE_URL="$db_url" \
  PROXY_CACHE__SERVER__PORT="$PORT" \
  PROXY_CACHE__STORAGE__PATH="$storage" \
    "$SERVER_BIN" --config perf/config.soak.toml >"$logfile" 2>&1 &
  SERVER_PROC=$!

  # The accept probe runs in its own process, and that is not tidiness.
  #
  # Interleaved with the `/healthz` poll in one loop, it loses the very window
  # it exists to measure: `curl` connects the moment the port opens and then
  # blocks inside its timeout waiting for a server that is not answering yet, so
  # the loop sits *inside the gap* and reaches the accept probe only once the
  # gap has closed. Measured: two of seven iterations reported an accept time
  # identical to the healthy time, on a server that in fact accepted 365 ms
  # earlier.
  local stamp="$WORK/accept-$RANDOM"
  (
    while ! (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; do
      sleep 0.005
    done
    now_ms > "$stamp"
  ) &
  local accept_watcher=$!

  T_ACCEPT=""
  T_HEALTHY=""
  # 180s: a cold start runs every migration, and a machine that is also
  # building something is slower than one that is not.
  local deadline=$(( $(now_ms) + 180000 ))
  while [[ "$(now_ms)" -lt "$deadline" ]]; do
    if ! kill -0 "$SERVER_PROC" 2>/dev/null; then
      kill "$accept_watcher" 2>/dev/null || true
      tail -20 "$logfile" >&2
      echo "ERROR: the server exited during startup" >&2
      exit 1
    fi
    if curl -sf -o /dev/null --max-time 2 "$BASE/healthz" 2>/dev/null; then
      T_HEALTHY="$(now_ms)"
      wait "$accept_watcher" 2>/dev/null || true
      T_ACCEPT="$(cat "$stamp" 2>/dev/null || echo "$T_HEALTHY")"
      rm -f "$stamp"
      return 0
    fi
    sleep 0.01
  done
  kill "$accept_watcher" 2>/dev/null || true
  tail -20 "$logfile" >&2
  echo "ERROR: the server did not become healthy in 180s" >&2
  exit 1
}

# stop_server — SIGTERM, then wait for the process to be gone. Records `T_STOP`.
stop_server() {
  local t0; t0="$(now_ms)"
  kill -TERM "$SERVER_PROC" 2>/dev/null || true
  # 120s: actix's default `shutdown_timeout` is 30s, and a request that reaches
  # it is meant to be abandoned rather than waited on forever. Anything past
  # this is a bug, and the run should say so rather than hang.
  local deadline=$(( t0 + 120000 ))
  while kill -0 "$SERVER_PROC" 2>/dev/null; do
    if [[ "$(now_ms)" -gt "$deadline" ]]; then
      echo "ERROR: the server was still running 120s after SIGTERM" >&2
      kill -KILL "$SERVER_PROC" 2>/dev/null || true
      exit 1
    fi
    sleep 0.01
  done
  wait "$SERVER_PROC" 2>/dev/null || true
  SERVER_PROC=""
  T_STOP=$(( $(now_ms) - t0 ))
}

record() {  # <phase> <iteration> <in-flight>
  echo "$1,$2,$(( T_ACCEPT - T_EXEC )),$(( T_HEALTHY - T_EXEC )),$T_STOP,$3" >> "$SAMPLES"
}

# ── Cold: a database with no schema, so the start pays for the migrations ─────
log "Cold start: a database with no schema ($COLD_DB)"
psql_do "DROP DATABASE IF EXISTS $COLD_DB"
psql_do "CREATE DATABASE $COLD_DB"
COLD_URL="${DATABASE_URL%/*}/$COLD_DB"
start_server "$COLD_URL" "$WORK/server-cold.log"
stop_server
record cold 1 0
printf '    accept %sms   healthy %sms   stop %sms\n' \
  "$(( T_ACCEPT - T_EXEC ))" "$(( T_HEALTHY - T_EXEC ))" "$T_STOP"

# ── Warm: the same database, now migrated ────────────────────────────────────
log "Warm start/stop × $ITERATIONS"
for i in $(seq 1 "$ITERATIONS"); do
  start_server "$COLD_URL" "$WORK/server-warm-$i.log"
  stop_server
  record warm "$i" 0
  printf '  %2d/%s  accept %sms   healthy %sms   stop %sms\n' \
    "$i" "$ITERATIONS" "$(( T_ACCEPT - T_EXEC ))" "$(( T_HEALTHY - T_EXEC ))" "$T_STOP"
done

# ── Draining: the same stop, with requests still in flight ───────────────────
#
# The reads go to a registry whose upstream is the slow mock and to a coordinate
# nothing has cached, so each one is still waiting on the upstream when the
# signal arrives. `--max-time` is above the upstream delay so curl is not the
# thing that ends them.
log "Draining stop: $IN_FLIGHT requests in flight against a ${UPSTREAM_DELAY_MS}ms upstream"
start_server "$COLD_URL" "$WORK/server-drain.log"
DRAIN_PIDS=()
for n in $(seq 1 "$IN_FLIGHT"); do
  curl -s -o /dev/null --max-time 60 \
    -H "Authorization: Bearer perf-user-token" \
    "$BASE/proxy/perf-npm/drain-$RANDOM-$n/1.0.0/tarball" &
  DRAIN_PIDS+=("$!")
done
# Long enough for every one of them to have reached the upstream and be waiting
# on it, and short of the delay so none has come back.
sleep 1
stop_server
# Only the load generators. A bare `wait` also waits on the mock upstream, which
# is a child of this shell and never exits — measured, by hanging the script
# here for seventeen minutes after the measurement it was collecting was over.
wait "${DRAIN_PIDS[@]}" 2>/dev/null || true
record draining 1 "$IN_FLIGHT"
printf '    stop %sms with %s in flight\n' "$T_STOP" "$IN_FLIGHT"

log "Report"
python3 perf/scripts/lifecycle_report.py --in-flight "$IN_FLIGHT" \
  --upstream-delay-ms "$UPSTREAM_DELAY_MS" --iterations "$ITERATIONS"
cat "$REPORT"
[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && cat "$REPORT" >> "$GITHUB_STEP_SUMMARY"

log "Samples: $SAMPLES   Report: $REPORT"
