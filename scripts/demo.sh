#!/usr/bin/env bash
# A complete BatleHub demo on one machine — every console surface with data in it.
#
#   scripts/demo.sh start [--reset] [--no-build] [--no-seed]
#   scripts/demo.sh seed              # re-run the seeding against a running demo
#   scripts/demo.sh status | stop | logs
#
# What it stands up, under tmp/demo/ (git-ignored):
#
#   - PostgreSQL: $DATABASE_URL if set; else the devfile sidecar on :5432; else
#     a user-space PostgreSQL 17 on :5433 (mise + conda, no root). A database
#     `batlehub_demo` is created, and dropped first with --reset.
#   - An OSV that answers from api.osv.dev's own recorded answers
#     (tests/heavy/osv_fake.py over tests/fixtures/supply-chain-jdd/osv/), so
#     the findings are real advisories and the same on every run.
#   - The server on 0.0.0.0:8080, roles proxy + worker, serving the built
#     console from the same origin (static_dir) — :8080 is what a CDE exposes.
#   - Five registries chosen to light every security state: npm-public and
#     crates-io refuse high and above, pypi-public only warns, npm-young holds
#     every version on its age, go-public has no protection at all.
#   - Six personas (tokens below): an admin, a delegated security team, a
#     flags-only SOC reader, an auditor, a plain user and a hashed CI token.
#
# Then demo_seed.py drives real traffic: downloads that get scanned, held and
# refused, a signed SOC flag (hard_block) on a version already pulled, a warn
# flag on a version pulled before and after it, rejected credentials, a manual
# IP ban, an exemption that ends in three days. The server is restarted once at the end, so the startup run of the daily
# security rollup records today's state and /security/trends has a first day.
#
# Then the rest of the console: one local registry per ecosystem the seed can
# publish to (npm, cargo, pypi, nuget, maven, composer, rubygems) with READMEs
# and SBOMs, a block and a bulk block, a beta channel, a team namespace and a
# shadowed one, a closed registry, a webhook to a local sink, a signed inbound
# event, an upstream nobody answers, a scanner that is down (deferred jobs), a blocked user, a
# warming run, a release import and one config change. Last, 59 days of cache
# history and 29 of security history are copied from today's rows (SQL: no API
# writes the past).
#
# Needs the network (npmjs.org, pypi.org, crates.io, proxy.golang.org, GitHub).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$REPO/tmp/demo"
PORT="${DEMO_PORT:-8080}"
OSV_PORT="${DEMO_OSV_PORT:-8197}"
SINK_PORT="${DEMO_SINK_PORT:-8198}"
DB_NAME="batlehub_demo"
BIN="$REPO/target/debug/batlehub"
CLI="$REPO/target/debug/batlehub-cli"
PG_DIR="$HOME/.local/share/pg-fallback"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

log() { printf '\033[1m[demo]\033[0m %s\n' "$*"; }
die() { printf '\033[31m[demo] %s\033[0m\n' "$*" >&2; exit 1; }
port_up() { local port="$1"; (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; }
need() { local cmd="$1" hint="$2"; command -v "$cmd" >/dev/null || die "missing '$cmd' — $hint"; }

# A process started detached, its pid kept: never stopped by pattern (a pkill
# pattern matches the calling shell too).
spawn() {
  local name="$1"; shift
  setsid nohup "$@" >"$WORK/$name.log" 2>&1 </dev/null &
  echo $! >"$WORK/$name.pid"
}
stop_one() {
  local pidfile="$WORK/$1.pid"
  [[ -f "$pidfile" ]] || return 0
  local pid; pid="$(cat "$pidfile")"
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$pidfile"
}

psql_cmd() {
  # `command -v` is not enough: a mise shim with no version set exists and fails.
  if psql --version >/dev/null 2>&1; then psql "$@"; else mise x conda:postgresql@17 -- psql "$@"; fi
}

# ── PostgreSQL ────────────────────────────────────────────────────────────────
ensure_postgres() {
  if [[ -n "${DATABASE_URL:-}" ]]; then
    ADMIN_URL="$DATABASE_URL"
  elif port_up 5432; then
    ADMIN_URL="postgresql://batlehub:changeme@127.0.0.1:5432/postgres"
  else
    if ! port_up 5433; then
      log "no PostgreSQL on :5432 — starting the user-space fallback on :5433"
      need mise "PostgreSQL comes from 'mise x conda:postgresql@17'"
      mkdir -p "$PG_DIR"
      if [[ ! -d "$PG_DIR/data" ]]; then
        printf 'changeme' >"$PG_DIR/pw"
        mise x conda:postgresql@17 -- initdb -D "$PG_DIR/data" -U batlehub \
          --pwfile="$PG_DIR/pw" -A scram-sha-256 >/dev/null
      fi
      setsid nohup mise x conda:postgresql@17 -- pg_ctl -D "$PG_DIR/data" -l "$PG_DIR/server.log" \
        -o "-p 5433 -k $PG_DIR -c listen_addresses=127.0.0.1 -c max_connections=200" start \
        </dev/null >/dev/null 2>&1 &
      for _ in $(seq 1 60); do port_up 5433 && break; sleep 1; done
      port_up 5433 || die "the fallback PostgreSQL did not start — see $PG_DIR/server.log"
    fi
    ADMIN_URL="postgresql://batlehub:changeme@127.0.0.1:5433/postgres"
  fi
  local base="${ADMIN_URL%/*}"
  DEMO_DB_URL="$base/$DB_NAME"
  if [[ "$RESET" == 1 ]]; then
    log "dropping $DB_NAME"
    psql_cmd "$ADMIN_URL" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" >/dev/null
  fi
  if ! psql_cmd "$ADMIN_URL" -tAc "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" | grep -q 1; then
    psql_cmd "$ADMIN_URL" -qc "CREATE DATABASE $DB_NAME" >/dev/null
    log "created database $DB_NAME"
  fi
}

# ── Build ─────────────────────────────────────────────────────────────────────
build() {
  log "building the server and the CLI (CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS)"
  ( cd "$REPO" && mise x -- cargo build -p batlehub-server -p batlehub-cli ) \
    >"$WORK/build-rust.log" 2>&1 || die "cargo build failed — see $WORK/build-rust.log"
  log "building the console (same origin: VITE_API_BASE_URL is empty, on purpose)"
  (
    cd "$REPO/ui"
    pnpm install --frozen-lockfile
    pnpm run generate
    VITE_API_BASE_URL="" pnpm run build
  ) >"$WORK/build-ui.log" 2>&1 || die "the console build failed — see $WORK/build-ui.log"
}

# ── Config ────────────────────────────────────────────────────────────────────
write_config() {
  # Every token but the admin's is stored hashed, as a deployment should:
  # one plain token, one auth.plain-static-token warning. A hashed token is
  # still typed in clear at sign-in.
  local ci_hash secops_hash soc_hash auditor_hash user_hash
  ci_hash="$("$BIN" hash-token demo-ci-token | tail -1)"
  secops_hash="$("$BIN" hash-token demo-secops-token | tail -1)"
  soc_hash="$("$BIN" hash-token demo-soc-token | tail -1)"
  auditor_hash="$("$BIN" hash-token demo-auditor-token | tail -1)"
  user_hash="$("$BIN" hash-token demo-user-token | tail -1)"
  local shadow_until; shadow_until="$(date -u -d '+30 days' +%F)"
  local cors_origins='"http://localhost:5173", "http://localhost:5174"' front=""
  command -v url >/dev/null && front="$(url front 2>/dev/null | sed 's#/$##')"
  [[ -n "$front" ]] && cors_origins+=", \"$front\""
  cat >"$WORK/config.toml" <<EOF
# Generated by scripts/demo.sh — edit the script, not this file.

[server]
host = "0.0.0.0"
port = $PORT
roles = ["proxy", "worker"]
static_dir = "$REPO/ui/dist"
# /me/cli offers this binary for download.
cli_binary_path = "$CLI"
# The Vite dev fronts call this server from another origin (task ui:dev on
# 5173, ui:dev:local on 5174, and the workspace's public front URL). Without
# them the preflight is refused and the sign-in page reports a bad token.
cors_allowed_origins = [$cors_origins]

[database]
type = "postgresql"
url = "$DEMO_DB_URL"
max_connections = 20

[storage]
type = "filesystem"
path = "$WORK/storage"

# Bans live in the [cache] backend: the default (memory) would lose the seeded
# ban when seeding restarts the server.
[cache]
type = "postgres"

# ── Personas ──────────────────────────────────────────────────────────────────
[[auth]]
type = "token"

# In clear on purpose: raises auth.plain-static-token, which Instance security
# and the dashboard's Config line then report.
[[auth.tokens]]
value = "demo-admin-token"
role = "admin"
user_id = "demo-admin"

[[auth.tokens]]
value = "$secops_hash"
role = "user"
user_id = "demo-secops"

[[auth.tokens]]
value = "$soc_hash"
role = "user"
user_id = "demo-soc"

[[auth.tokens]]
value = "$auditor_hash"
role = "user"
user_id = "demo-auditor"

[[auth.tokens]]
value = "$user_hash"
role = "user"
user_id = "demo-user"

# Hashed, as every static token should be: no warning for this one.
[[auth.tokens]]
value = "$ci_hash"
role = "user"
user_id = "ci-bot"

[grants]
# RFC 0042's delegated security team: the whole /security space, no admin.
"user:demo-secops" = ["quarantine:read", "findings:read", "flags:read", "audit:read", "scans:run"]
# A SOC that reads flags and nothing else: /security opens on Overview + Flags.
"user:demo-soc" = ["flags:read"]
# Exposure only.
"user:demo-auditor" = ["audit:read"]
# Nobody holds gates:exempt by default; the admin needs it for the seeded exemption.
"user:demo-admin" = ["gates:exempt"]

# ── History, abuse, workers ───────────────────────────────────────────────────
[stats]
history_enabled = true
security_history_retention_days = 90

[ip_blocking]
enabled = true
# High enough that the seeded rejected credentials never ban the demo's own
# address (a CDE's ingress is one address for every browser).
violation_threshold = 500
violation_window_secs = 300
ban_duration_secs = 600
trigger_on_status = [429]

[worker]
max_concurrent = 4
max_attempts = 2
job_timeout_secs = 120

# ── Scanners ──────────────────────────────────────────────────────────────────
[scanners.osv]
type = "osv"
api_url = "http://127.0.0.1:$OSV_PORT/replay"

# ── Registries ────────────────────────────────────────────────────────────────
[[registries]]
type = "npm"
name = "npm-public"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

# A rate limit, so GET /api/v1/me/limits and the console have a counter to show.
[registries.rate_limit]
requests_per_window = 600
window_secs = 60
enforcement = "block"

[registries.security]
mode = "block"
min_age_secs = 3600
mature_age_secs = 0
scanners = ["osv"]
required_scanners = ["osv"]
max_severity = "high"
pullers_window_days = 30

[registries.security.rescan]
interval_secs = 86400

[[registries]]
type = "npm"
name = "npm-young"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

# Twenty years: every version is too recent, so holds wait on MIN_AGE_NOT_MET.
[registries.security]
mode = "block"
min_age_secs = 631152000
mature_age_secs = 0
scanners = ["osv"]
required_scanners = ["osv"]
max_severity = "high"

[[registries]]
type = "pypi"
name = "pypi-public"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

# Warn mode never refuses on severity: a finding at or above max_severity is
# served with a warning. Known malware (etherreeum) is still refused.
[registries.security]
mode = "warn"
min_age_secs = 3600
mature_age_secs = 0
scanners = ["osv"]
required_scanners = ["osv"]
max_severity = "critical"

[[registries]]
type = "cargo"
name = "crates-io"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

[registries.security]
mode = "block"
min_age_secs = 3600
mature_age_secs = 0
scanners = ["osv"]
required_scanners = ["osv"]
max_severity = "high"

[[registries.rules]]
kind = "cve_gate"
min_severity = "high"
block = true

# No [security], no cve_gate: Coverage's "not protected" row and ATTENTION line.
[[registries]]
type = "goproxy"
name = "go-public"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

# ── A scanner that is down ────────────────────────────────────────────────────
# An OSV nobody answers on. A transport error is transient, so npm-broken's jobs
# are deferred, not failed: Scanners shows osvdown in ATTENTION and two deferred
# jobs. (A *failed* job needs a permanent error — an oversized answer — which
# the demo does not fake.)
[scanners.osvdown]
type = "osv"
api_url = "http://127.0.0.1:1"

[[registries]]
type = "npm"
name = "npm-broken"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

[registries.security]
mode = "block"
min_age_secs = 3600
mature_age_secs = 0
scanners = ["osvdown"]
required_scanners = ["osvdown"]
max_severity = "high"

# An upstream nothing listens on: recent errors on Health, Degraded on Registries.
[[registries]]
type = "npm"
name = "npm-down"
mode = "proxy"
upstreams = ["http://127.0.0.1:9"]

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

# ── Private registries: publish, README, SBOM, quota (no network needed) ──────
# One local registry per ecosystem the seed can publish to with plain HTTP. The
# demo user may publish; SBOMs are built at publish; quotas are low enough that
# the home page's gauge moves.
$(for kind in npm cargo pypi nuget maven composer rubygems; do
  name="$kind-local"; [[ $kind == rubygems ]] && name="gems-local"
  cat <<REG
[[registries]]
type = "$kind"
name = "$name"
mode = "local"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "releases:publish", "source:read"]
admin = ["*"]

[registries.sbom]
enabled = true

[registries.quota]
max_packages_per_user = 12
max_storage_bytes_per_user = 1048576
warn_threshold_pct = 50

REG
done)
# npm-team carries the rest: a beta channel, a scan, and a team namespace
# whose sealed grants run in shadow until $shadow_until — served, and recorded on
# Authorization's Shadow panel.
[[registries]]
type = "npm"
name = "npm-team"
mode = "local"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "releases:publish", "source:read"]
admin = ["*"]

[registries.sbom]
enabled = true

[registries.beta_channel]
enabled = true

[registries.security]
mode = "warn"
min_age_secs = 3600
mature_age_secs = 0
scanners = ["osv"]
max_severity = "critical"

[[registries.namespaces]]
match = "@acme/internal"
visibility = "team"

[registries.namespaces.grants]

[registries.namespaces.grants_shadow]
until = "$shadow_until"

# Closed to users: a plain user's request is refused (Authorization's Refused).
[[registries]]
type = "npm"
name = "npm-closed"
mode = "local"

[registries.rbac]
anonymous = []
user = []
admin = ["*"]

# A forge, the source of a release import (/admin/registries/imports).
[[registries]]
type = "github"
name = "gh"
mode = "proxy"

[registries.rbac]
anonymous = ["releases:read", "releases:list", "source:read"]
user = ["releases:read", "releases:list", "source:read"]
admin = ["*"]

[[release_imports]]
into = "npm-local"
from = "gh"
repo = "batleforc/batlehub"
assets = ["*.tgz"]
releases = "latest"

[release_imports.as]
user_id = "svc-release-import"

# ── Notifications: a webhook channel to a local sink, an inbound endpoint ─────
[notifications]
enabled = true

[[notifications.channels]]
name = "sink"
type = "webhook"
url = "http://127.0.0.1:$SINK_PORT/hook"
secret = "demo-webhook-secret"

[[notifications.inbound]]
name = "ci"
secret = "demo-inbound-secret"

# ── Upstream audit (/admin/registries/upstream) ───────────────────────────────
[upstream_audit]
enabled = true
interval_secs = 3600
confirm_after = 2
registries = ["npm-public", "pypi-public"]

# ── The SOC's signed push channel (RFC 0002) ──────────────────────────────────
[[flag_sources]]
name = "soc"
secret = "demo-soc-secret"
max_effect = "hard_block"
registries = ["npm-public"]
EOF
}

# ── Processes ─────────────────────────────────────────────────────────────────
start_osv() {
  stop_one osv
  # The flip file never exists: /replay is all the demo asks.
  spawn osv python3 -I "$REPO/tests/heavy/osv_fake.py" "$WORK/osv-flip" "$OSV_PORT" \
    "never-matches" "$REPO/tests/fixtures/supply-chain-jdd/osv"
  for _ in $(seq 1 20); do port_up "$OSV_PORT" && return; sleep 0.5; done
  die "the replayed OSV did not start — see $WORK/osv.log"
}

start_sink() {
  stop_one sink
  spawn sink python3 -I "$REPO/tests/heavy/webhook_sink.py" "$WORK/webhooks.log" "$SINK_PORT"
  for _ in $(seq 1 20); do port_up "$SINK_PORT" && return; sleep 0.5; done
  die "the webhook sink did not start — see $WORK/sink.log"
}

start_server() {
  stop_one server
  port_up "$PORT" && die "port $PORT is taken by something else — set DEMO_PORT"
  spawn server "$BIN" --config "$WORK/config.toml"
  for _ in $(seq 1 120); do
    curl -sf "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && return
    kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null || die "the server exited — see $WORK/server.log"
    sleep 1
  done
  die "the server did not answer /healthz in 120 s — see $WORK/server.log"
}

seed() {
  log "seeding: downloads, scans, refusals, flags, sign-ins, a ban, an exemption"
  python3 -I "$REPO/scripts/demo_seed.py" "http://127.0.0.1:$PORT" "$WORK" \
    || die "seeding failed — the server is still running; see the output above"
  log "restarting once, so the startup rollup records today for /security/trends"
  start_server
  backfill_history
}

# The one step that writes SQL. No API writes the past: the rollups record the
# day and the hour they run in. So today's real rows — the registries and the
# refusal codes the server itself produced — are copied onto earlier days with a
# varying factor: 59 days of cache history (the dashboard's trend compares two
# 30-day windows) and 29 days of security history (/security/trends).
backfill_history() {
  log "backfilling 59 days of cache history and 29 days of security history from today's rows"
  psql_cmd "$DEMO_DB_URL" -q -v ON_ERROR_STOP=1 <<'SQL' >/dev/null || die "the history backfill failed"
WITH reg AS (SELECT DISTINCT registry FROM stats_history),
     d AS (SELECT n, 0.6 + 0.4 * (n % 7) / 6.0 + (60 - n) / 120.0 AS f FROM generate_series(1, 59) n)
INSERT INTO stats_history (registry, window_start, hits, misses, listing_reads, cached_bytes)
SELECT reg.registry,
       date_trunc('hour', now()) - make_interval(days => d.n),
       round(120 * d.f + (n * 37 % 50))::bigint,
       round(40 * (1.4 - d.f) + (n * 13 % 9))::bigint,
       round(30 * d.f)::bigint,
       COALESCE((SELECT max(cached_bytes) FROM stats_history s WHERE s.registry = reg.registry), 0)
FROM reg CROSS JOIN d
ON CONFLICT (registry, window_start) DO NOTHING;

WITH today AS (SELECT * FROM security_daily WHERE day = current_date),
     d AS (SELECT n, 0.4 + 0.6 * ((n * 7) % 11) / 10.0 AS f FROM generate_series(1, 29) n)
INSERT INTO security_daily (registry, day, allowed, warned, quarantined, denied,
  findings_critical, findings_high, findings_medium, findings_low, findings_unknown,
  refusals, refusals_by_code, flags_opened, flags_revoked, jobs_done, jobs_failed)
SELECT t.registry, current_date - d.n,
       round(COALESCE(t.allowed, 0) * d.f), round(COALESCE(t.warned, 0) * d.f),
       round(COALESCE(t.quarantined, 0) * d.f), round(COALESCE(t.denied, 0) * d.f),
       round(COALESCE(t.findings_critical, 0) * d.f), round(COALESCE(t.findings_high, 0) * d.f),
       round(COALESCE(t.findings_medium, 0) * d.f), round(COALESCE(t.findings_low, 0) * d.f),
       round(COALESCE(t.findings_unknown, 0) * d.f),
       round(COALESCE(t.refusals, 0) * d.f),
       (SELECT jsonb_object_agg(k, round(v::numeric * d.f)) FROM jsonb_each_text(COALESCE(t.refusals_by_code, '{}')) AS e(k, v)),
       CASE WHEN d.n % 9 = 0 THEN 1 ELSE 0 END, CASE WHEN d.n % 13 = 0 THEN 1 ELSE 0 END,
       round(COALESCE(t.jobs_done, 0) * d.f), CASE WHEN d.n % 5 = 0 THEN 1 ELSE 0 END
FROM today t CROSS JOIN d
ON CONFLICT (registry, day) DO NOTHING;
SQL
}

summary() {
  local public=""
  command -v url >/dev/null && public="$(url back 2>/dev/null | sed 's#/$##')"
  cat <<EOF

  BatleHub demo is running.

    console   http://127.0.0.1:$PORT${public:+   ·   $public}
    logs      $WORK/server.log   (osv: $WORK/osv.log)
    config    $WORK/config.toml
    stop      scripts/demo.sh stop

  Sign in on the console with one of these tokens:

    demo-admin-token     everything; /admin/dashboard, /admin/instance-security
    demo-secops-token    the delegated security team: all of /security, and the
                         audit log as its one admin page (audit:read)
    demo-soc-token       flags:read only — /security shows Overview + Flags
    demo-auditor-token   audit:read only — /security opens on Exposure
    demo-user-token      a plain user: no /security, no /admin
    demo-ci-token        hashed in the config (no warning), plain user

  Worth opening:

    /security                    the status strip and "Needs a decision"
    /security/trends             today's day (history starts on the first run)
    /security/coverage           go-public is not protected
    /security/scanners           the worker, the queue, the OSV scanner
    /admin/instance-security     rejected sign-ins, the ban, the plain token
    /admin/dashboard             the security lines beside the cache verdict
    /admin/access/check          an old bookmark: lands on Authorization's Simulate

    /packages                    published packages in seven ecosystems, a block,
                                 a bulk block, a pre-release (npm-local demo-greeter)
    /packages/npm-local/demo-greeter   README, SBOM, a blocked version, downloads
    /admin/system/health         npm-down: an upstream nobody answers
    /security/scanners           osvdown in ATTENTION, npm-broken's deferred jobs
    /admin/access/authorization  the shadow namespace, refusals on npm-closed
    /admin/system/notifications  the sink subscription (deliveries: $WORK/webhooks.log)
    /admin/system/inbound-events a signed event from "ci"
    /admin/system/configuration  one recorded change (a quota raised 12 → 15)
    /admin/instance-security/blocks   a banned address and a blocked user
    /me/cli                      the CLI binary to download

  Not seeded: /me/tokens and /me/namespace need an OIDC sign-in (dex), and the
  air-gap page needs [air_gap], which turns the whole instance offline.

  From a terminal:

    BATLEHUB_SERVER=http://127.0.0.1:$PORT BATLEHUB_TOKEN=demo-secops-token \\
      $CLI security status

EOF
}

# ── Main ──────────────────────────────────────────────────────────────────────
CMD="${1:-start}"; shift || true
RESET=0; BUILD=1; SEED=1
for arg in "$@"; do
  case "$arg" in
    --reset) RESET=1 ;;
    --no-build) BUILD=0 ;;
    --no-seed) SEED=0 ;;
    *) die "unknown option $arg" ;;
  esac
done
mkdir -p "$WORK/storage"

case "$CMD" in
  start)
    need curl "used to wait for the server"
    need python3 "the seeding and the replayed OSV are Python"
    need pnpm "the console is built with pnpm (mise install)"
    need mise "cargo and PostgreSQL come through mise"
    [[ "$RESET" == 1 ]] && { stop_one server; rm -rf "$WORK/storage"; mkdir -p "$WORK/storage"; }
    ensure_postgres
    [[ "$BUILD" == 1 || ! -x "$BIN" || ! -d "$REPO/ui/dist" ]] && build
    write_config
    start_osv
    start_sink
    log "starting the server on :$PORT"
    start_server
    [[ "$SEED" == 1 ]] && seed
    summary
    ;;
  seed)
    port_up "$PORT" || die "no demo is running on :$PORT — scripts/demo.sh start"
    seed
    ;;
  stop)
    stop_one server
    stop_one osv
    stop_one sink
    log "stopped (PostgreSQL left running: the data stays for the next start)"
    ;;
  status)
    for p in server osv sink; do
      if [[ -f "$WORK/$p.pid" ]] && kill -0 "$(cat "$WORK/$p.pid")" 2>/dev/null; then
        log "$p: running (pid $(cat "$WORK/$p.pid"))"
      else
        log "$p: stopped"
      fi
    done
    ;;
  logs) exec tail -f "$WORK/server.log" ;;
  *) die "usage: scripts/demo.sh start [--reset] [--no-build] [--no-seed] | seed | status | stop | logs" ;;
esac
