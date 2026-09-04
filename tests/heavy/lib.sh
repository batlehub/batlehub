# Shared harness for the heavy client integration tests. Sourced, not executed.
#
# Every heavy test has the same skeleton: start a real BatleHub against a real
# Postgres, put a transparent tap in front of it, drive a real package manager
# at the tap, and assert on the wire transcript. What differs between them is
# only the client and the claims. This file is the skeleton, so the seven
# ecosystem scripts do not each re-derive the parts that are subtle:
#
#   - the server must be SIGTERMed as a *process group*, or `cargo` keeps the
#     binary alive and cargo-llvm-cov never flushes its profiles;
#   - the tap must not touch `Host`, or the server hands the client absolute
#     URLs pointing past the tap and the transcript goes quiet (RFC 0009 §12.10);
#   - the port knobs must not be called `PORT`, which dev containers and PaaS
#     runtimes export for their own service — one inherited value pointed a
#     health check at an unrelated process that never became healthy (§13.24);
#   - the registry name must be fresh per run, because the database persists and
#     what a previous run left behind changes what the client sees.
#
# `bundler.sh` and `marketplace.sh` predate this file and still carry their own
# copies of the server/tap machinery. They are left alone deliberately: both are
# green, CI-verified suites whose subtleties are documented in RFC 0009, and
# porting them would mean re-verifying two clients (a real `bundle install`, two
# IDEs) to remove duplication and nothing else. New suites use this file.
#
# Usage, from a script in this directory:
#
#     source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
#     heavy_init npm 8082 8092          # suite name, server port, tap port
#     export HEAVY_REGISTRY="npm-$HEAVY_RUN"
#     heavy_start_server tests/heavy/config.npm.toml
#     heavy_start_tap
#     ... drive the client at $HEAVY_TAP_BASE ...
#     heavy_wire "GET /proxy/$HEAVY_REGISTRY/-/ping -> 200"
#     heavy_done NPM-HEAVY-OK
#
# Environment knobs shared by every suite:
#   DATABASE_URL    (required)  Postgres for the server
#   HEAVY_PORT      the server's own port          (per-suite default)
#   HEAVY_TAP_PORT  what the client is pointed at  (per-suite default)
#   ADMIN_TOKEN     publish credential, matching the suite's config
#   COVERAGE=1      run the server under `cargo llvm-cov run --no-report`
#   HEAVY_CACHE     cacheable client downloads (default ~/.cache/batlehub-heavy)

set -euo pipefail

HEAVY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$HEAVY_ROOT"

HEAVY_SUITE=""
HEAVY_WORK=""
HEAVY_SERVER_PID=""
HEAVY_TAP_PID=""

heavy_log() { printf '\n==> %s\n' "$*"; }

# Every failure dumps the transcript: the sequence is the evidence, and a bare
# "assertion failed" from a heavy test is unactionable without it.
heavy_fail() {
  echo "ERROR: $*" >&2
  if [[ -n "${HEAVY_LOG:-}" && -s "${HEAVY_LOG:-/dev/null}" ]]; then
    echo "── wire transcript ──" >&2
    cat "$HEAVY_LOG" >&2
  fi
  if [[ -n "${HEAVY_WORK:-}" && -s "$HEAVY_WORK/server.log" ]]; then
    echo "── server log (tail) ──" >&2
    tail -60 "$HEAVY_WORK/server.log" >&2
  fi
  exit 1
}

heavy_stop_server() {
  if [[ -n "$HEAVY_SERVER_PID" ]]; then
    # SIGTERM the whole process group: $HEAVY_SERVER_PID is the `cargo` wrapper
    # and the server binary is a grandchild. cargo does not forward signals, so
    # signalling the wrapper alone strands the server — no graceful shutdown, no
    # llvm profile flush. `setsid` at launch made it the process-group id.
    kill -TERM -- "-$HEAVY_SERVER_PID" 2>/dev/null \
      || kill -TERM "$HEAVY_SERVER_PID" 2>/dev/null || true
    wait "$HEAVY_SERVER_PID" 2>/dev/null || true
    for _ in $(seq 1 60); do
      pgrep -g "$HEAVY_SERVER_PID" >/dev/null 2>&1 || break
      sleep 1
    done
    if pgrep -g "$HEAVY_SERVER_PID" >/dev/null 2>&1; then
      echo "WARNING: server process group $HEAVY_SERVER_PID still alive after 60s;" \
        "llvm coverage profiles may be incomplete" >&2
    fi
  fi
  HEAVY_SERVER_PID=""
}

heavy_cleanup() {
  [[ -n "$HEAVY_TAP_PID" ]] && kill "$HEAVY_TAP_PID" 2>/dev/null
  heavy_stop_server
  [[ -n "$HEAVY_WORK" ]] && rm -rf "$HEAVY_WORK"
  return 0
}

# heavy_init <suite> <default-server-port> <default-tap-port>
heavy_init() {
  local suite="$1" default_port="$2" default_tap_port="$3"
  HEAVY_SUITE="$suite"
  HEAVY_PORT="${HEAVY_PORT:-$default_port}"
  HEAVY_TAP_PORT="${HEAVY_TAP_PORT:-$default_tap_port}"
  ADMIN_TOKEN="${ADMIN_TOKEN:-heavy-admin-token}"
  COVERAGE="${COVERAGE:-0}"
  HEAVY_CACHE="${HEAVY_CACHE:-$HOME/.cache/batlehub-heavy}"

  : "${DATABASE_URL:?DATABASE_URL must point at a reachable Postgres}"

  HEAVY_WORK="$(mktemp -d)"
  # `date` rather than a counter: the suffix only has to differ from whatever
  # the *database* already holds, and the database outlives this process.
  HEAVY_RUN="$(date +%H%M%S)"
  HEAVY_BASE="http://127.0.0.1:$HEAVY_PORT"
  HEAVY_TAP_BASE="http://127.0.0.1:$HEAVY_TAP_PORT"
  HEAVY_LOG="$HEAVY_WORK/tap.log"
  : > "$HEAVY_LOG"
  # The tap's status-rewrite rules (RFC 0018 §4.4); see `heavy_tap_rewrite`.
  export TAP_REWRITE_FILE="$HEAVY_WORK/tap.rewrite"

  # `${HEAVY_RUN}` is read by the suite configs through the loader's `${VAR}`
  # expansion, which happens on the raw text; the port and storage path go
  # through the loader's env-override path instead, so the config files stay
  # valid TOML that an editor or `taplo` can parse (a `${…}` placeholder in a
  # scalar position does not).
  export HEAVY_PORT HEAVY_RUN
  export HEAVY_STORAGE="$HEAVY_WORK/storage"
  export PROXY_CACHE__SERVER__PORT="$HEAVY_PORT"
  export PROXY_CACHE__STORAGE__PATH="$HEAVY_STORAGE"
  mkdir -p "$HEAVY_STORAGE"

  trap heavy_cleanup EXIT
  heavy_log "[$HEAVY_SUITE] work dir $HEAVY_WORK, run id $HEAVY_RUN"
}

# heavy_start_server <config-path>
heavy_start_server() {
  local config="$1"

  # Compile before the health clock starts. The launch below is `cargo run`,
  # which builds first — and under COVERAGE=1 that is an *instrumented* build of
  # the whole dependency graph, minutes of it on a cold target directory. The
  # wait loop would then be timing the compiler and reporting "the server did
  # not become healthy", which names the wrong thing and is indistinguishable
  # from a server that starts and hangs. `--help` exits 0 without reading the
  # config or binding a port, so this builds and returns.
  heavy_log "Building BatleHub (coverage=$COVERAGE)"
  if [[ "$COVERAGE" == "1" ]]; then
    cargo llvm-cov run --no-report -p batlehub-server -- --help >/dev/null 2>&1 \
      || heavy_fail "the instrumented server did not build — run 'cargo llvm-cov run --no-report -p batlehub-server -- --help' to see why"
  else
    cargo build -p batlehub-server >"$HEAVY_WORK/build.log" 2>&1 \
      || { cat "$HEAVY_WORK/build.log" >&2; heavy_fail "the server did not build"; }
  fi

  heavy_log "Starting BatleHub (coverage=$COVERAGE, config=$config)"
  if [[ "$COVERAGE" == "1" ]]; then
    setsid cargo llvm-cov run --no-report -p batlehub-server -- \
      --config "$config" >"$HEAVY_WORK/server.log" 2>&1 &
  else
    setsid cargo run -p batlehub-server -- \
      --config "$config" >"$HEAVY_WORK/server.log" 2>&1 &
  fi
  HEAVY_SERVER_PID=$!

  for i in $(seq 1 180); do
    if curl -sf "$HEAVY_BASE/healthz" >/dev/null 2>&1; then
      heavy_log "Server healthy at $HEAVY_BASE"
      return 0
    fi
    if ! kill -0 "$HEAVY_SERVER_PID" 2>/dev/null; then
      HEAVY_SERVER_PID=""
      heavy_fail "server exited before becoming healthy"
    fi
    sleep 2
    [[ "$i" == 180 ]] && heavy_fail "server did not become healthy within 6 minutes"
  done
}

# heavy_start_tap [cert key] — the logging proxy the client is actually pointed
# at. With a certificate it terminates TLS, for the clients that refuse a
# plain-http registry (Terraform has no opt-out; NuGet and Composer have one).
heavy_start_tap() {
  local cert="${1:-}" key="${2:-}"
  python3 tests/heavy/http_tap.py "$HEAVY_LOG" "$HEAVY_TAP_PORT" "$HEAVY_PORT" \
    ${cert:+"$cert" "$key"} >"$HEAVY_WORK/tap.err" 2>&1 &
  HEAVY_TAP_PID=$!
  # `-f` is deliberately absent: readiness here means "the tap answers", and on
  # a host bound to one registry *every* path is that registry's — `/healthz`
  # included, which is then a 404 from the registry rather than the health
  # endpoint. A probe that required 2xx would wait out its whole timeout
  # against a tap that was up from the first second.
  local probe=(curl -s -o /dev/null)
  if [[ -n "$cert" ]]; then
    HEAVY_TAP_BASE="https://${HEAVY_TAP_HOST:-localhost}:$HEAVY_TAP_PORT"
    probe+=(--cacert "$cert")
  fi
  for _ in $(seq 1 30); do
    "${probe[@]}" "$HEAVY_TAP_BASE/healthz" && break
    sleep 1
  done
  "${probe[@]}" "$HEAVY_TAP_BASE/healthz" || {
    cat "$HEAVY_WORK/tap.err" >&2
    heavy_fail "the logging tap never came up on $HEAVY_TAP_PORT"
  }
  heavy_log "Tap listening on $HEAVY_TAP_PORT -> $HEAVY_PORT ($HEAVY_TAP_BASE)"
}

# heavy_self_signed <hostname> — write a certificate/key pair for <hostname>
# into the work dir and echo the certificate path. Self-signed and used as its
# own CA: the client trusts it through SSL_CERT_FILE, so there is no chain to
# build.
heavy_self_signed() {
  local host="$1"
  local cert="$HEAVY_WORK/tls-cert.pem" key="$HEAVY_WORK/tls-key.pem"
  heavy_need openssl "openssl"
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
    -keyout "$key" -out "$cert" -subj "/CN=$host" \
    -addext "subjectAltName=DNS:$host,DNS:localhost,IP:127.0.0.1" \
    >"$HEAVY_WORK/openssl.log" 2>&1 \
    || { cat "$HEAVY_WORK/openssl.log" >&2; heavy_fail "could not generate a certificate for $host"; }
  echo "$cert"
}

# A mark in the transcript, so an assertion can be scoped to one phase of the
# run rather than to everything the client has ever asked for.
heavy_mark() { echo "### $*" >> "$HEAVY_LOG"; }

# heavy_tap_rewrite <METHOD> <path-prefix> <from> <to> [Header: value]... —
# from now on the tap answers <to> where the server answered <from> on that
# path, with the headers added. This is the instrument of RFC 0018 §4.4: the
# server does not emit a quarantine's `403` + `Retry-After` yet (phase 2), and
# what each client *does* with one is the measurement. The transcript shows
# `-> 200=>202` for a rewritten line, so the assertion can tell the two apart.
# Rules accumulate until `heavy_tap_rewrite_clear`.
heavy_tap_rewrite() {
  local method="$1" prefix="$2" from="$3" to="$4"
  shift 4
  local headers=""
  local h
  for h in "$@"; do headers="${headers:+$headers ;; }$h"; done
  echo "$method $prefix $from $to${headers:+ $headers}" >> "$TAP_REWRITE_FILE"
}
heavy_tap_rewrite_clear() { rm -f "$TAP_REWRITE_FILE"; }

# heavy_block <registry> <name> <version> [artifact] — block one coordinate
# through the admin API; `heavy_unblock` lifts it. The path-proxy kinds have
# one package (`repo`, version `_`) and address a file by its artifact path.
heavy_block() {
  local registry="$1" name="$2" version="$3" artifact="${4:-}"
  local body
  body=$(printf '{"registry":"%s","name":"%s","version":"%s",%s"reason":"heavy: administratively blocked"}' \
    "$registry" "$name" "$version" "${artifact:+\"artifact\":\"$artifact\",}")
  curl -fsS -X POST "$HEAVY_BASE/api/v1/admin/packages/block" \
    -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
    -d "$body" >"$HEAVY_WORK/block.json" \
    || { cat "$HEAVY_WORK/block.json" >&2; heavy_fail "the block request failed"; }
}
heavy_unblock() {
  local registry="$1" name="$2" version="$3" artifact="${4:-}"
  local body
  body=$(printf '{"registry":"%s","name":"%s","version":"%s"%s}' \
    "$registry" "$name" "$version" "${artifact:+,\"artifact\":\"$artifact\"}")
  curl -fsS -X POST "$HEAVY_BASE/api/v1/admin/packages/unblock" \
    -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
    -d "$body" >"$HEAVY_WORK/unblock.json" \
    || { cat "$HEAVY_WORK/unblock.json" >&2; heavy_fail "the unblock request failed"; }
}

# heavy_wire <fixed-string> [explanation] — the line must be in the transcript.
heavy_wire() {
  local needle="$1" explanation="${2:-}"
  grep -qF -- "$needle" "$HEAVY_LOG" \
    || heavy_fail "${explanation:-no request matching \"$needle\" was observed}"
}

# heavy_wire_not <fixed-string> [explanation]
heavy_wire_not() {
  local needle="$1" explanation="${2:-}"
  grep -qF -- "$needle" "$HEAVY_LOG" \
    && heavy_fail "${explanation:-unexpected request \"$needle\" was observed}"
  return 0
}

# heavy_wire_seen_after <mark> <fixed-string> — the predicate
# `heavy_wire_after` is built on: true when the line appears after a
# `heavy_mark`. Call it directly when the failure needs more than a message —
# dumping the client's own log alongside it, say.
heavy_wire_seen_after() {
  local label="$1" needle="$2"
  awk -v mark="### $label" -v needle="$needle" '
    index($0, mark) == 1 { seen = 1; next }
    seen && index($0, needle) { found = 1 }
    END { exit found ? 0 : 1 }' "$HEAVY_LOG"
}

# heavy_wire_after <mark> <fixed-string> [explanation] — the line must appear
# after a `heavy_mark`. The transcript accumulates across the whole run, so an
# unscoped assertion can be satisfied by an earlier phase's request; that is the
# "green for the wrong reason" failure this suite exists to avoid.
heavy_wire_after() {
  local label="$1" needle="$2" explanation="${3:-}"
  heavy_wire_seen_after "$label" "$needle" \
    || heavy_fail "${explanation:-no request matching \"$needle\" after mark \"$label\"}"
}

# heavy_done <banner> — stop the server first, so the coverage profiles are
# flushed before the caller runs `cargo llvm-cov report`, then print the
# transcript and the banner CI greps for.
heavy_done() {
  local banner="$1"
  heavy_log "Stopping the server"
  heavy_stop_server
  heavy_log "Wire transcript"
  cat "$HEAVY_LOG"
  heavy_log "$banner"
}

# heavy_need <binary> <what-provides-it> — a missing client is a failed run, not
# a skipped one. A heavy test that skips itself when its client is absent
# reports success for having done nothing, which is the one outcome worse than
# red (see `REAL_PROXY_REQUIRE` in Taskfile.yml).
heavy_need() {
  local bin="$1" provided_by="$2"
  command -v "$bin" >/dev/null 2>&1 \
    || heavy_fail "$bin not found on PATH — install it ($provided_by) before running this suite"
}

# heavy_runner_for <binary> <mise-spec>... — set HEAVY_RUNNER to the prefix
# that runs <binary>: empty when it works on PATH, `mise x <spec>... --` when
# only a directory-scoped mise toolchain has it. Several specs when the tool
# needs a second one to run at all (Maven needs a JDK).
#
# `command -v` is not the test. mise installs shims: the binary is on PATH and
# exits non-zero with "No version is set for shim" because no version is pinned
# for this directory. Probe by *running* it.
heavy_runner_for() {
  local bin="$1"
  shift
  HEAVY_RUNNER=()
  if "$bin" --version >/dev/null 2>&1; then
    return 0
  fi
  if command -v mise >/dev/null 2>&1 && mise x "$@" -- "$bin" --version >/dev/null 2>&1; then
    HEAVY_RUNNER=(mise x "$@" --)
    return 0
  fi
  heavy_fail "no working $bin (and no mise toolchain for $*)"
}

# heavy_cached_dir <name> <url> [format] — download and unpack once into
# HEAVY_CACHE, echo the directory. `format` (tar.gz | tar.bz2 | zip) overrides
# the guess from the URL, which several of these downloads need: the URL that
# serves micromamba names a version, not an extension.
heavy_cached_dir() {
  local name="$1" url="$2" format="${3:-}"
  local dest="$HEAVY_CACHE/$name"
  if [[ -z "$format" ]]; then
    case "$url" in
      *.tar.gz|*.tgz) format="tar.gz" ;;
      *.tar.bz2)      format="tar.bz2" ;;
      *.zip)          format="zip" ;;
      *) heavy_fail "heavy_cached_dir: cannot guess the format of $url — pass one" ;;
    esac
  fi
  if [[ ! -d "$dest" ]]; then
    heavy_log "Downloading $name" >&2
    rm -rf "$dest.tmp"
    mkdir -p "$dest.tmp"
    case "$format" in
      tar.gz)  curl -fsSL "$url" | tar -xz -C "$dest.tmp" ;;
      tar.bz2) curl -fsSL "$url" | tar -xj -C "$dest.tmp" ;;
      zip)     curl -fsSL "$url" -o "$dest.tmp/archive.zip"
               (cd "$dest.tmp" && unzip -q archive.zip && rm archive.zip) ;;
      *)       heavy_fail "heavy_cached_dir: unknown format '$format'" ;;
    esac
    mv "$dest.tmp" "$dest"
  fi
  echo "$dest"
}
