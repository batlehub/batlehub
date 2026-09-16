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
#   - quiescing before each window lets the server drain and lets jemalloc's
#     decay return dirty pages, which takes seconds and would otherwise read as
#     growth.
#
# What is left after that is what was still held when nothing was in flight,
# which is what "leak" means.
#
# **The quiesce only works on a binary whose jemalloc purges in the background**
# — `server/Cargo.toml`'s `tikv-jemallocator` `background_threads` feature.
# jemalloc's decay is driven by allocator activity, so an idle process purges
# nothing; the same build without that feature reported +10.3 % idle RSS on a
# run whose live heap was flat, because the final window follows ten minutes of
# load and the baseline follows one. Something that disables it — building with
# `--no-default-features`, or `_RJEM_MALLOC_CONF=background_thread:false` —
# turns this gate back into a measurement of retained arenas. See the comment on
# that dependency for the numbers.
#
# Usage:
#   bash perf/scripts/soak.sh [--duration 10m] [--rate 100] [--warmup 240]
#                             [--settle 60] [--profile release|debug]
#
# The report is always `perf/results/soak.md`, beside the samples, the chart and
# the metrics dumps the verdict reads. There is no flag to move it: the verdict
# script takes no path at all, by design.
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
#   SOAK_MAX_HEAP_GROWTH_PCT       default 5     idle live heap (jemalloc's
#                                                stats.allocated), final vs
#                                                baseline — tighter than RSS
#                                                because it is not the
#                                                allocator's bookkeeping
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

DURATION="10m"
RATE="100"
# The warm-up has to *finish the fill*, or the baseline is a measurement of a
# half-filled process and every run reports the rest of the fill as growth.
#
# **It is no longer a duration, because a duration was wrong three times.** It
# was 60s when the mix was three registries; 240s when the mix was 46 arms over
# 24 kinds; and 240s was too short again by the time the mix reached 48 arms
# over 25. Each correction was made after a red run, by measuring the fill and
# writing down a number that the next kind added to the mix invalidated.
#
# So the warm-up now ends on the thing it was always a proxy for: **the idle
# live heap has stopped moving.** Load runs in blocks; after each block the
# process is quiesced and its idle `stats.allocated` read; when two consecutive
# readings agree to within `WARMUP_STABLE_PCT`, the fill is over and that
# quiesce *is* the baseline window. A kind added to the mix lengthens the
# warm-up by itself instead of silently invalidating a constant.
#
# The numbers below are bounds on that loop, not the loop's answer:
#
#   WARMUP_MIN   never decide before this much load. 240s, the old fixed value,
#                so no run is warmed less than runs already trusted were.
#   WARMUP_MAX   give up after this much and say so. Measured 2026-09-16: the
#                idle residue of the 48-arm mix saturates at ~164 MiB after
#                800-900s of traffic (146.6 at 240s, 153.3 at 600s, 163.3 at
#                840s, 164.4 at 1200s), so 1200s clears it with room to spare.
#   WARMUP_BLOCK load between two readings.
#   WARMUP_STABLE_PCT  empty means **derived from the gate**, which is the only
#                setting that cannot disagree with it: the drift left in one
#                block, extrapolated across the load window, must be too small
#                to fail the live-heap row on its own —
#                `SOAK_MAX_HEAP_GROWTH_PCT * WARMUP_BLOCK / <load seconds>`,
#                so 5% over 10m with 120s blocks is 1.0% a block.
#
#                A hand-picked constant was tried first and was wrong for a
#                reason worth keeping: 1.5% a block accumulates to 7.5% across
#                a 600s load, so a process drifting just under the convergence
#                threshold fails a 5% gate *by construction*. Measured
#                2026-09-16: warm-up probes at 148, 158, 169, 167 MiB declared
#                convergence on that last -1.27% — noise between two 6.7% jumps
#                — and the run then failed at +6.2%.
#   WARMUP_STABLE_RUNS  how many consecutive stable probes end it. Two, from the
#                same run: one agreement is reachable by noise alone.
#
# What the fill actually *is*, since it is not obvious and cost a day to find:
# most of it is the Postgres connection pool. A listing document is stored in
# and read back from the `doc:` cache namespace, and carrying a 1.9 MiB
# rubygems compact index grows that connection's buffer to the size of the
# document for the connection's lifetime. Ten connections, ~3.9 MiB apiece,
# released only when sqlx recycles them at its 30-minute `max_lifetime` — which
# is *longer than a whole soak*, so both windows sit inside one lifetime and
# what the old fixed warm-up varied was how many connections had been grown
# before the baseline. Proven by an idle observation: the residue sat flat to
# the tenth of a MiB for 24 minutes, straight through the 300s cache TTL, and
# fell 15.5 MiB at t=1800s exactly, one sample before the pool shrank 10 -> 6.
WARMUP=""                 # empty: adaptive. A --warmup value pins it, for A/B.
WARMUP_MIN="${SOAK_WARMUP_MIN:-240}"
WARMUP_MAX="${SOAK_WARMUP_MAX:-1200}"
WARMUP_BLOCK="${SOAK_WARMUP_BLOCK:-120}"
WARMUP_STABLE_PCT="${SOAK_WARMUP_STABLE_PCT:-}"   # empty: derived, see above
WARMUP_STABLE_RUNS="${SOAK_WARMUP_STABLE_RUNS:-2}"
SETTLE="60"
# Fixed, and known to `soak_verdict.py` by the same construction: the
# directory comes from the script's own location, not from an argument.
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
    --profile)  PROFILE="$2";  shift 2 ;;
    -h|--help)  sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

: "${DATABASE_URL:?DATABASE_URL must point at a reachable Postgres}"

SOAK_PORT="${SOAK_PORT:-8180}"
SOAK_UPSTREAM_PORT="${SOAK_UPSTREAM_PORT:-8181}"
export SOAK_UPSTREAM_URL="http://127.0.0.1:$SOAK_UPSTREAM_PORT"
# Terraform's network-mirror routes carry the mirrored hostname in the path and
# the handler checks it against the registry's upstream, so the arm needs the
# mock's authority. Derived from the URL above rather than written twice.
export BATLEHUB_SOAK_UPSTREAM_HOST="127.0.0.1"  # the host alone: the check excludes the port
BASE="http://127.0.0.1:$SOAK_PORT"

WORK="$(mktemp -d)"
# The files the verdict script reads live in `perf/results/`, not in `$WORK`,
# and are named rather than passed: `soak_verdict.py` derives every one of them
# from its own location, so no path crosses the process boundary (see its
# header). `$WORK` keeps what only this script reads — build logs, the arms
# transcript, the server's storage directory.
#
# They are also where they were always going to end up: this script used to copy
# samples.csv and marks.txt into `perf/results/` at the end for the artifact
# upload, which is `perf/results/` in `soak.yaml`. Writing them there from the
# start deletes the copy and the window where a crashed run left them only in a
# temp directory.
RESULTS="perf/results"
mkdir -p "$RESULTS"
SAMPLES="$RESULTS/soak-samples.csv"
MARKS="$RESULTS/soak-marks.txt"
SERVER_LOG="$RESULTS/soak-server.log"
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
  local tool="$1" how="$2"
  command -v "$tool" >/dev/null 2>&1 \
    || { echo "ERROR: $tool not found on PATH — install it ($how)" >&2; exit 1; }
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

# `SOAK_SERVER_BIN` runs a binary this script did not build, and exists for one
# question: what does a *build option* cost? An allocator, a feature set, a
# toolchain — comparing those means two binaries and one workload, and a script
# that always rebuilds from the current manifest can only ever measure the
# manifest it is looking at. Build both, stash them, point this at each in turn.
#
# It is not a convenience for skipping the build: a stale binary measured
# against current sources is a result about nothing, so the path is echoed into
# the report's own log and the caller owns what it points at.
#
# **The file has to be called `batlehub`.** The sampler finds the process to
# measure with `pgrep -x batlehub` — by executable name, because `-f` would also
# match this script — and `comm` is truncated to 15 characters anyway, so a
# descriptive filename would both miss and be unfixable. Put each build in its
# own directory instead: `/tmp/alloc-cmp/jemalloc/batlehub`.
SERVER_BIN="./target/$TARGET_DIR/batlehub"
if [[ -n "${SOAK_SERVER_BIN:-}" ]]; then
  [[ -x "$SOAK_SERVER_BIN" ]] \
    || { echo "ERROR: SOAK_SERVER_BIN=$SOAK_SERVER_BIN is not an executable" >&2; exit 1; }
  SERVER_BIN="$SOAK_SERVER_BIN"
  log "Using a prebuilt server: $SERVER_BIN ($("$SERVER_BIN" --version 2>/dev/null | head -1))"
fi

log "Building the server and the mock upstream ($PROFILE)"
if [[ -z "${SOAK_SERVER_BIN:-}" ]]; then
  cargo build "${BUILD_FLAGS[@]}" -p batlehub-server >"$WORK/build.log" 2>&1 \
    || { cat "$WORK/build.log" >&2; echo "ERROR: the server did not build" >&2; exit 1; }
fi
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
setsid "$SERVER_BIN" --config perf/config.soak.toml \
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

# ── Pre-flight, which is also the seed ────────────────────────────────────────
#
# Every arm of the mix, once, asserted against the statuses the arm itself
# declares. This is a gate and not a warm-up nicety: the load's own check is
# "not 5xx", so an arm whose URL is wrong or whose upstream route was never
# written answers `404` and **passes**, and the report then shows that registry
# costing nothing — which reads exactly like a cheap registry.
#
# It seeds at the same time: one read of each document the mix will ask for, so
# the warm-up measures steady behaviour rather than first-touch.
log "Pre-flight: every arm of the mix, once"
if ! BATLEHUB_URL="$BASE" k6 run --quiet perf/k6/scenarios/11_soak_arms.js \
       >"$WORK/arms.log" 2>&1; then
  sed -n '/soak arms did not answer as declared/,$p' "$WORK/arms.log" >&2 \
    || tail -40 "$WORK/arms.log" >&2
  echo "ERROR: the mix does not work against this configuration — see $WORK/arms.log" >&2
  exit 1
fi
grep -m1 "soak arms:" "$WORK/arms.log" >&2 || true

# ── The sampler ───────────────────────────────────────────────────────────────
# /proc for the process facts and /metrics for the pool and the live heap. One
# line a second: a soak is a curve, and the verdict below needs the points, not
# a summary.
#
# `heap_kb` is jemalloc's `stats.allocated` — bytes in live allocations, which
# is the only column here that separates "the server is holding objects" from
# "the allocator is holding pages". It is empty on a build without the
# `jemalloc` feature, and the verdict reports that row as not measured rather
# than as zero growth.
sample_header="epoch_s,rss_kb,fds,threads,pool_size,pool_idle,heap_kb"
echo "$sample_header" > "$SAMPLES"
# Truncated for the same reason the samples are: the verdict reads the marks
# into a dict and takes the last value for each name, so a mark this run does
# not write would otherwise be inherited from the previous one — and
# `warmup_converged` is exactly such a mark, written only when the adaptive
# warm-up has something to say about itself.
: > "$MARKS"
(
  while kill -0 "$SERVER_PROC" 2>/dev/null; do
    rss="$(awk '/^VmRSS:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null || echo "")"
    threads="$(awk '/^Threads:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null || echo "")"
    # `ls` rather than a glob: the directory changes while it is being read and
    # a failed glob under `set -e` would end the sampler silently.
    fds="$(ls "/proc/$SERVER_PROC/fd" 2>/dev/null | wc -l || echo "")"
    # One scrape, three numbers: a second curl a second would be a second
    # render of the whole exposition, which is itself an allocation.
    pool="$(curl -s --max-time 5 "$BASE/metrics" 2>/dev/null \
      | awk '/^batlehub_db_pool_size/{s=$2}
             /^batlehub_db_pool_available_connections/{i=$2}
             /^batlehub_memory_allocated_bytes/{h=$2/1024}
             END{printf "%s,%s,%s", (s==""?"":s), (i==""?"":i), (h==""?"":int(h))}')"
    if [[ -n "$rss" ]]; then
      echo "$(date +%s),$rss,$fds,$threads,${pool:-,,}" >> "$SAMPLES"
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
  curl -s --max-time 10 "$BASE/metrics" > "$RESULTS/soak-metrics-$name.txt" || true
}

run_k6() {  # <phase> <duration>
  local phase="$1" duration="$2"
  log "k6 — $phase phase: ${duration} at ${RATE} req/s"
  BATLEHUB_URL="$BASE" \
  BATLEHUB_SOAK_DURATION="$duration" \
  BATLEHUB_SOAK_RATE="$RATE" \
  BATLEHUB_SOAK_PHASE="$phase" \
    k6 run --summary-export "$RESULTS/soak-k6-$phase.json" perf/k6/scenarios/10_soak.js
}

quiesce() {  # <seconds> — no load, so the next window measures what is *held*
  local seconds="$1"
  log "Quiescing for ${seconds}s"
  sleep "$seconds"
}

# The idle live heap in KiB — the quantity the warm-up converges on, and the
# same series the verdict judges. Empty on a build without jemalloc stats.
idle_heap_kb() {
  curl -s --max-time 5 "$BASE/metrics" 2>/dev/null \
    | awk '/^batlehub_memory_allocated_bytes/{printf "%d", $2/1024}'
}

# Seconds from a k6 duration (`600`, `600s`, `10m`, `1h`).
to_seconds() {
  local d="$1"
  case "$d" in
    *h) echo $(( ${d%h} * 3600 )) ;;
    *m) echo $(( ${d%m} * 60 )) ;;
    *s) echo "${d%s}" ;;
    *)  echo "$d" ;;
  esac
}

# The per-block drift that cannot, on its own, fail the live-heap row.
#
# Derived rather than chosen, so the warm-up and the gate cannot disagree: a
# drift of `p` per block accumulates to `p * load/block` across the window the
# gate measures, and that has to stay under the gate's own limit.
derived_stable_pct() {
  local load_s
  load_s="$(to_seconds "$DURATION")"
  awk -v g="${SOAK_MAX_HEAP_GROWTH_PCT:-5.0}" -v b="$WARMUP_BLOCK" -v l="$load_s" \
    'BEGIN{ if (l <= 0) l = 600; printf "%.3f", g * b / l }'
}

# warm_up — offer load until the idle live heap stops moving.
#
# Each iteration is a block of load, a quiesce, and one idle reading. Two
# consecutive readings within `WARMUP_STABLE_PCT` end it.
#
# `warmup_end` and `baseline_end` are written on *every* probe, and that is
# deliberate: `soak_verdict.py` reads the marks into a dict, so the last pair
# written wins. The probe that decided is therefore the baseline window, the
# probes before it are warm-up, and no quiesce is spent twice — the convergence
# check and the baseline are the same 60 seconds of idle.
warm_up() {
  local warmed=0 prev="" now="" delta="" stable=0
  [[ -z "$WARMUP_STABLE_PCT" ]] && WARMUP_STABLE_PCT="$(derived_stable_pct)"
  log "Warm-up: adaptive — blocks of ${WARMUP_BLOCK}s, ending on ${WARMUP_STABLE_RUNS} consecutive readings within ${WARMUP_STABLE_PCT}% (min ${WARMUP_MIN}s, max ${WARMUP_MAX}s)"
  while :; do
    run_k6 warmup "${WARMUP_BLOCK}s" || true
    warmed=$(( warmed + WARMUP_BLOCK ))
    (( warmed < WARMUP_MIN )) && continue
    mark warmup_end
    quiesce "$SETTLE"
    now="$(idle_heap_kb)"
    mark baseline_end
    if [[ -z "$now" ]]; then
      log "Warm-up: no live-heap series to converge on (jemalloc stats absent) — stopping at ${warmed}s"
      return 0
    fi
    if [[ -n "$prev" ]]; then
      delta="$(awk -v a="$prev" -v b="$now" 'BEGIN{d=(b-a)/a*100; printf "%.2f", (d<0?-d:d)}')"
      log "Warm-up: idle live heap $(( prev / 1024 )) -> $(( now / 1024 )) MiB after ${warmed}s of load (${delta}% apart, stable at or below ${WARMUP_STABLE_PCT}%)"
      if awk -v d="$delta" -v t="$WARMUP_STABLE_PCT" 'BEGIN{exit !(d<=t)}'; then
        stable=$(( stable + 1 ))
        if (( stable >= WARMUP_STABLE_RUNS )); then
          log "Warm-up: converged after ${warmed}s — ${stable} consecutive stable readings, the baseline is a steady state"
          echo "warmup_converged 1" >> "$MARKS"
          return 0
        fi
        log "Warm-up: stable reading ${stable} of ${WARMUP_STABLE_RUNS}"
      else
        # One agreement inside a climb is noise, so the count starts over
        # rather than accumulating across a jump.
        stable=0
      fi
    else
      log "Warm-up: idle live heap $(( now / 1024 )) MiB after ${warmed}s of load (first reading)"
    fi
    prev="$now"
    if (( warmed >= WARMUP_MAX )); then
      log "WARNING: the idle live heap was still moving after ${warmed}s (${delta}% apart). The baseline is not a steady state, so the live-heap row is reported and not judged."
      echo "warmup_converged 0" >> "$MARKS"
      return 0
    fi
  done
}

# ── The run ───────────────────────────────────────────────────────────────────
mark warmup_start
if [[ -n "$WARMUP" ]]; then
  # Pinned by the caller, for an A/B against a run whose warm-up is known. No
  # `warmup_converged` mark: the caller owns the claim that this is long enough,
  # and the verdict judges the live-heap row as it always did.
  run_k6 warmup "${WARMUP}s" || true   # the warm-up's thresholds are not a verdict
  mark warmup_end
  quiesce "$SETTLE"
  mark baseline_end
else
  warm_up
fi

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
# No path is passed. Every file above was written into `perf/results/` under a
# name `soak_verdict.py` knows, and it resolves that directory from its own
# location — so the only arguments crossing are the three numbers that describe
# the run. What that buys, beyond a shorter command: a path-shaped argument is
# a path-shaped *injection*, and the way to be sure there is none is not to
# take one.
set +e
python3 perf/scripts/soak_verdict.py \
  --k6-exit "$K6_EXIT" \
  --duration "$DURATION" \
  --rate "$RATE"
VERDICT=$?
set -e

cat "$REPORT"
[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && cat "$REPORT" >> "$GITHUB_STEP_SUMMARY"

log "Samples: $(dirname "$REPORT")/soak-samples.csv   Report: $REPORT"
exit "$VERDICT"
