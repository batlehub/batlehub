#!/usr/bin/env bash
# Soak the server with a **real client**, in a loop, and check what it gave back.
#
# The companion to `perf/scripts/soak.sh`, and the reason both exist: k6 offers
# the requests this project *thinks* a client makes. npm offers the ones it
# actually makes — a packument, a conditional re-fetch, a tarball, a publish
# with a body it built itself, each with its own headers and its own connection
# reuse. Every registry defect this project has shipped was found by a client
# and not by a test double (RFC 0009 §5.1), and there is no reason a leak would
# be different.
#
# The shape is the same as the k6 soak, for the same reasons, and the comments
# there explain them: warm-up, quiesce, **baseline**, the loop, quiesce,
# **final**. Both windows are idle, and the baseline is after a warm-up, so
# what is measured is what the process was still holding with nothing in
# flight rather than every lazily-filled cache and allocator arena.
#
# What it asserts, in order:
#
#   1. every round's `npm install` and `npm publish` succeeded — a soak whose
#      server stopped serving leaks nothing and proves nothing;
#   2. the loop reached the upstream **zero** times after a warm-up that
#      touched every package, so what it is soaking is the served path;
#   3. RSS, open descriptors and threads came back to where they started;
#   4. the server logged no panic.
#
# Run: `task test:soak-heavy`, or `bash tests/heavy/soak.sh`.
#
# Environment: DATABASE_URL (required); HEAVY_PORT (8170), HEAVY_TAP_PORT
# (8171), HEAVY_UPSTREAM_PORT (8172); COVERAGE.
#   SOAK_ROUNDS                 rounds in the loop          (default 25)
#   SOAK_PACKAGES               distinct upstream packages  (default 8)
#   SOAK_SETTLE                 quiesce before each window  (default 20 s)
#   SOAK_MAX_RSS_GROWTH_PCT     idle RSS growth             (default 15)
#   SOAK_MAX_FD_GROWTH          open descriptors            (default 16)
#   SOAK_MAX_THREAD_GROWTH      OS threads                  (default 4)
#   SOAK_REPORT                 write a markdown summary here (default: none)

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init soak 8170 8171
heavy_need npm "nodejs"
heavy_need node "nodejs"
heavy_need python3 "python3"

HEAVY_UPSTREAM_PORT="${HEAVY_UPSTREAM_PORT:-8172}"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/upstream_dir.sh"

ROUNDS="${SOAK_ROUNDS:-25}"
PACKAGES="${SOAK_PACKAGES:-8}"
SETTLE="${SOAK_SETTLE:-20}"
MAX_RSS_PCT="${SOAK_MAX_RSS_GROWTH_PCT:-15}"
MAX_FD="${SOAK_MAX_FD_GROWTH:-16}"
MAX_THREADS="${SOAK_MAX_THREAD_GROWTH:-4}"

PROXY="npm-proxy-$HEAVY_RUN"
LOCAL="npm-local-$HEAVY_RUN"
PUBLISHED="heavy-soak-$HEAVY_RUN"

# ── The upstream: a handful of names, so the loop resolves a set rather than
# one document over and over ─────────────────────────────────────────────────
for i in $(seq 1 "$PACKAGES"); do
  upstream_npm_package "heavy-soak-up-$i" "1.0.0"
done
upstream_serve

heavy_start_server tests/heavy/config.soak.toml
heavy_start_tap

PROXY_URL="$HEAVY_TAP_BASE/proxy/$PROXY/"
LOCAL_URL="$HEAVY_TAP_BASE/proxy/$LOCAL/"

NPMRC="$HEAVY_WORK/npmrc"
cat > "$NPMRC" <<RC
registry=$PROXY_URL
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$PROXY/:_authToken=$ADMIN_TOKEN
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$LOCAL/:_authToken=$ADMIN_TOKEN
RC
export NPM_CONFIG_USERCONFIG="$NPMRC"
export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false

heavy_log "npm $(npm --version), node $(node --version) — $ROUNDS rounds over $PACKAGES packages"

# ── The server's own PID ─────────────────────────────────────────────────────
# `heavy_start_server` launches `setsid cargo run`, so `$HEAVY_SERVER_PID` is
# whichever of those two the shell happened to background — and neither is the
# server. Measuring it anyway is the failure this whole suite exists to avoid:
# `/proc/<cargo>/status` answers every question with a plausible number, so a
# leak in the server reads as a clean run and the suite is green for the wrong
# reason. Measured, before this was resolved properly: 224 MiB of "server RSS"
# that was cargo's, falling 8 % over a soak that never touched it.
#
# So: find the process whose **comm is the binary** and whose command line is
# this suite's config, and refuse to measure anything else. `pgrep -x` matches
# the executable name, never the full command line — `-f` would also match this
# script and the shell that launched it, which is the standing rule in these
# suites about matching processes by pattern.
soak_server_proc() {
  local pid cmd
  for pid in $(pgrep -x batlehub 2>/dev/null || true); do
    cmd="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)"
    [[ "$cmd" == *"tests/heavy/config.soak.toml"* ]] && { echo "$pid"; return 0; }
  done
  return 1
}
SERVER_PROC="$(soak_server_proc)" \
  || heavy_fail "no running 'batlehub --config tests/heavy/config.soak.toml' process to measure — the server is up (it answered /healthz), so this is the PID lookup, not the server"
[[ -d "/proc/$SERVER_PROC" ]] \
  || heavy_fail "cannot read /proc/$SERVER_PROC — nothing can be measured"
heavy_log "Measuring pid $SERVER_PROC: $(tr '\0' ' ' < "/proc/$SERVER_PROC/cmdline")"

# soak_sample — "<rss_kb> <fds> <threads>" for the server process, right now.
soak_sample() {
  local rss fds threads
  rss="$(awk '/^VmRSS:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null || echo 0)"
  threads="$(awk '/^Threads:/{print $2; exit}' "/proc/$SERVER_PROC/status" 2>/dev/null || echo 0)"
  fds="$(ls "/proc/$SERVER_PROC/fd" 2>/dev/null | wc -l || echo 0)"
  echo "$rss $fds $threads"
}

# soak_window <label> — quiesce, then the median of five samples a second apart.
# The median, because one sample can land on a scrape, a sweep or a log flush;
# and five, because that is enough for a median to mean something and short
# enough not to double the run.
# Its progress goes to **stderr**: the function's stdout is the measurement,
# read through `$(…)`, and a `heavy_log` line on stdout would be captured as
# part of it — which reads as a window whose RSS is the string "==>".
soak_window() {
  local label="$1"
  heavy_log "Quiescing ${SETTLE}s before the $label window" >&2
  sleep "$SETTLE"
  local rss=() fds=() threads=() s
  for _ in 1 2 3 4 5; do
    s="$(soak_sample)"
    rss+=("$(echo "$s" | cut -d' ' -f1)")
    fds+=("$(echo "$s" | cut -d' ' -f2)")
    threads+=("$(echo "$s" | cut -d' ' -f3)")
    sleep 1
  done
  local m_rss m_fds m_threads
  m_rss="$(printf '%s\n' "${rss[@]}" | sort -n | sed -n '3p')"
  m_fds="$(printf '%s\n' "${fds[@]}" | sort -n | sed -n '3p')"
  m_threads="$(printf '%s\n' "${threads[@]}" | sort -n | sed -n '3p')"
  # A window that is not three numbers is a broken measurement, and every
  # comparison below it would be arithmetic on a string. Name it here.
  [[ "$m_rss" =~ ^[0-9]+$ && "$m_fds" =~ ^[0-9]+$ && "$m_threads" =~ ^[0-9]+$ ]] \
    || heavy_fail "the $label window did not measure three numbers (got '$m_rss' '$m_fds' '$m_threads') — is /proc/$SERVER_PROC still there?"
  heavy_log "$label: RSS $((m_rss / 1024)) MiB, $m_fds fds, $m_threads threads" >&2
  echo "$m_rss $m_fds $m_threads"
}

# ── One round: what a developer's machine does, once ─────────────────────────
soak_round() {  # <n>
  local n="$1"
  local pkg="heavy-soak-up-$(( (n % PACKAGES) + 1 ))"
  local project="$HEAVY_WORK/project"

  # A cache of its own per round, removed after: npm that answers from its own
  # cacache never asks the proxy anything, and a soak of npm's cache is not a
  # soak of this server (npm.sh records the same trap, from the other side).
  rm -rf "$project" "$HEAVY_WORK/npm-cache"
  mkdir -p "$project"
  export NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache"
  cat > "$project/package.json" <<JSON
{ "name": "heavy-soak-consumer", "version": "1.0.0", "private": true }
JSON
  (cd "$project" && npm install "$pkg@1.0.0" --registry "$PROXY_URL" \
    >"$HEAVY_WORK/install-$n.log" 2>&1) || {
      heavy_client_said "$HEAVY_WORK/install-$n.log" "error|ERR!" 6
      heavy_fail "round $n: npm install $pkg failed"
    }
  [[ -f "$project/node_modules/$pkg/package.json" ]] \
    || heavy_fail "round $n: npm reported success but installed nothing"

  # The write path, with a new version each round so nothing is a no-op.
  local pubdir="$HEAVY_WORK/publish"
  rm -rf "$pubdir"
  mkdir -p "$pubdir"
  cat > "$pubdir/package.json" <<JSON
{ "name": "$PUBLISHED", "version": "1.0.$n", "description": "soak round $n", "license": "MIT", "main": "index.js" }
JSON
  echo "module.exports = $n;" > "$pubdir/index.js"
  (cd "$pubdir" && npm publish --registry "$LOCAL_URL" \
    >"$HEAVY_WORK/publish-$n.log" 2>&1) || {
      heavy_client_said "$HEAVY_WORK/publish-$n.log" "error|ERR!" 6
      heavy_fail "round $n: npm publish failed"
    }

  # And a metadata read of what was just written, so the round touches the
  # local read path too.
  npm view "$PUBLISHED" version --registry "$LOCAL_URL" >/dev/null 2>&1 \
    || heavy_fail "round $n: npm view failed on the package it had just published"
}

# ── Warm-up: one round per package, so every document is cached ──────────────
# One round per package rather than a fixed two, because the assertion after
# the loop is that the loop reached the upstream **zero** times. That is only a
# statement about caching if the warm-up has already touched every name; with a
# shorter warm-up it would be a statement about which names the loop happened
# to pick.
heavy_mark warmup
for n in $(seq 0 $((PACKAGES - 1))); do
  soak_round "$n"
done
UP_PACKUMENTS_WARM="$(upstream_requests "heavy-soak-up-")"
UP_TARBALLS_WARM="$(upstream_requests "tarballs/heavy-soak-up-")"
heavy_log "After warm-up the upstream had served $UP_PACKUMENTS_WARM packuments and $UP_TARBALLS_WARM tarballs"

BASELINE="$(soak_window baseline)"
BASE_RSS="$(echo "$BASELINE" | cut -d' ' -f1)"
BASE_FDS="$(echo "$BASELINE" | cut -d' ' -f2)"
BASE_THREADS="$(echo "$BASELINE" | cut -d' ' -f3)"

# ── The loop ─────────────────────────────────────────────────────────────────
heavy_mark loop
for n in $(seq "$PACKAGES" $((PACKAGES + ROUNDS - 1))); do
  soak_round "$n"
  (( n % 5 == 0 )) && heavy_log "round $((n - PACKAGES + 1))/$ROUNDS: $(soak_sample)"
done

FINAL="$(soak_window final)"
FINAL_RSS="$(echo "$FINAL" | cut -d' ' -f1)"
FINAL_FDS="$(echo "$FINAL" | cut -d' ' -f2)"
FINAL_THREADS="$(echo "$FINAL" | cut -d' ' -f3)"

# ── 1. The client kept working, and the cache did its job ────────────────────
# Every round installed the same small set of names. The upstream is a served
# directory, so "asked once per package" is a count: more requests than
# packages means the proxy re-fetched what it already had, which is a cache
# that is not holding rather than a leak — but it is a finding either way.
UP_PACKUMENTS="$(upstream_requests "heavy-soak-up-")"
UP_TARBALLS="$(upstream_requests "tarballs/heavy-soak-up-")"
LOOP_PACKUMENTS=$((UP_PACKUMENTS - UP_PACKUMENTS_WARM))
LOOP_TARBALLS=$((UP_TARBALLS - UP_TARBALLS_WARM))

# **Zero.** `metadata_ttl_secs = 3600` outlives any run and the warm-up touched
# every name, so a loop that reaches the upstream at all is a cache that is not
# holding — and it would also make the growth numbers below meaningless, since
# the loop would then be measuring the upstream path rather than the served one.
#
# The warm-up's own count is *not* asserted to be one per package: measured
# against this fixture, a first install costs three packument fetches and one
# tarball fetch per package — the packument the client asks for, then two more
# when the artifact route resolves the coordinate for the first time. That is
# the shape today, and a bound on it here would be a second, silent copy of a
# number that belongs in a test about resolution rather than about leaks.
if (( LOOP_PACKUMENTS != 0 || LOOP_TARBALLS != 0 )); then
  heavy_fail "the loop reached the upstream $LOOP_PACKUMENTS times for packuments and $LOOP_TARBALLS times for tarballs after the warm-up had cached every one of $PACKAGES packages — the proxy is re-fetching what it holds"
fi
heavy_log "Loop of $ROUNDS rounds reached the upstream 0 times (warm-up: $UP_PACKUMENTS_WARM packuments, $UP_TARBALLS_WARM tarballs)"

heavy_wire_re_after warmup "PUT /proxy/$LOCAL/$PUBLISHED -> 20[01]" \
  "no publish reached the local registry"

# ── 2. Panics ────────────────────────────────────────────────────────────────
if grep -q "panicked at" "$HEAVY_WORK/server.log" 2>/dev/null; then
  grep "panicked at" "$HEAVY_WORK/server.log" | head -5 >&2
  heavy_fail "the server panicked during the soak"
fi

# ── 3. What it gave back ─────────────────────────────────────────────────────
RSS_GROWTH_PCT="$(awk -v b="$BASE_RSS" -v f="$FINAL_RSS" 'BEGIN{ printf "%.1f", (b>0 ? (f-b)/b*100 : 0) }')"
FD_GROWTH=$((FINAL_FDS - BASE_FDS))
THREAD_GROWTH=$((FINAL_THREADS - BASE_THREADS))

heavy_log "$(printf 'RSS %d -> %d MiB (%s%%), fds %d -> %d (%+d), threads %d -> %d (%+d)' \
  "$((BASE_RSS / 1024))" "$((FINAL_RSS / 1024))" "$RSS_GROWTH_PCT" \
  "$BASE_FDS" "$FINAL_FDS" "$FD_GROWTH" \
  "$BASE_THREADS" "$FINAL_THREADS" "$THREAD_GROWTH")"

FAILED=0
RSS_VERDICT=ok FD_VERDICT=ok THREAD_VERDICT=ok
awk -v g="$RSS_GROWTH_PCT" -v m="$MAX_RSS_PCT" 'BEGIN{ exit !(g > m) }' && {
  echo "LEAK: idle RSS grew ${RSS_GROWTH_PCT}% over $ROUNDS rounds (limit ${MAX_RSS_PCT}%)" >&2
  RSS_VERDICT="**over**"
  FAILED=1
}
if (( FD_GROWTH > MAX_FD )); then
  echo "LEAK: $FD_GROWTH more open descriptors than at baseline (limit $MAX_FD)" >&2
  FD_VERDICT="**over**"
  FAILED=1
fi
if (( THREAD_GROWTH > MAX_THREADS )); then
  echo "LEAK: $THREAD_GROWTH more threads than at baseline (limit $MAX_THREADS)" >&2
  THREAD_VERDICT="**over**"
  FAILED=1
fi

# The report, written **before** the verdict is acted on: `heavy_fail` exits,
# and a run that fails is the one whose numbers someone wants to read.
if [[ -n "${SOAK_REPORT:-}" ]]; then
  mkdir -p "$(dirname "$SOAK_REPORT")"
  {
    echo "<!-- soak-report-heavy -->"
    if (( FAILED )); then
      echo "## Soak (npm client) — FAILED"
    else
      echo "## Soak (npm client) — no leak detected"
    fi
    echo
    echo "$((ROUNDS)) rounds of \`npm install\` + \`npm publish\` + \`npm view\` over"
    echo "$PACKAGES packages, after a warm-up that touched every one of them."
    echo "Both windows below are **idle**."
    echo
    echo "| | baseline | final | growth | limit | |"
    echo "| --- | ---: | ---: | ---: | ---: | :-- |"
    echo "| RSS (idle) | $((BASE_RSS / 1024)) MiB | $((FINAL_RSS / 1024)) MiB | ${RSS_GROWTH_PCT}% | ${MAX_RSS_PCT}% | $RSS_VERDICT |"
    echo "| File descriptors | $BASE_FDS | $FINAL_FDS | $(printf '%+d' "$FD_GROWTH") | $MAX_FD | $FD_VERDICT |"
    echo "| Threads | $BASE_THREADS | $FINAL_THREADS | $(printf '%+d' "$THREAD_GROWTH") | $MAX_THREADS | $THREAD_VERDICT |"
    echo
    echo "The loop reached the upstream **$LOOP_PACKUMENTS** times for packuments"
    echo "and **$LOOP_TARBALLS** times for tarballs — zero is the assertion, since"
    echo "the warm-up had already cached every document the loop asks for."
    echo "(Warm-up itself: $UP_PACKUMENTS_WARM packuments, $UP_TARBALLS_WARM tarballs"
    echo "for $PACKAGES packages.)"
  } > "$SOAK_REPORT"
fi

(( FAILED )) && heavy_fail "the server did not give back what it took — see the growth above"

heavy_done "SOAK-HEAVY-OK ($ROUNDS rounds: RSS ${RSS_GROWTH_PCT}%, fds ${FD_GROWTH}, threads ${THREAD_GROWTH})"
