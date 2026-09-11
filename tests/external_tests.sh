#!/usr/bin/env bash
# The integration tests that need a real Postgres, S3 and Redis — the ones
# CI's `integration` job runs against its service containers — on a machine
# without Podman.
#
# `task test:pg-*`, `test:s3` and `test:redis` each start a throwaway container
# and are the right tool where Podman is there. Where it is not (a Che
# workspace has a Postgres sidecar and nothing to run containers with), the
# same tests were skipped silently and reported green: every one of them
# returns early when its `DATABASE_URL` / `S3_TEST_ENDPOINT` / `REDIS_URL` is
# unset. This script runs them for real:
#
#   - Postgres: `DATABASE_URL` (required; the sidecar's
#     postgresql://batlehub:changeme@127.0.0.1:5432/batlehub here);
#   - S3: `S3_TEST_ENDPOINT` as given, else a MinIO started from the cached
#     binary tests/heavy/backends.sh also uses (downloaded once);
#   - Redis: `REDIS_URL` as given, else a `redis-server` from the cached PyPI
#     wheel, likewise.
#
# Then the four invocations of the CI job, verbatim: adapters `--test '*'`
# (Postgres + default features), `s3_storage`, and the three Redis suites.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
: "${DATABASE_URL:?DATABASE_URL must point at a reachable Postgres}"
CACHE="${HEAVY_CACHE:-$HOME/.cache/batlehub-heavy}"
MINIO_PORT="${EXTERNAL_MINIO_PORT:-8143}"
REDIS_PORT="${EXTERNAL_REDIS_PORT:-8144}"
WORK="$(mktemp -d)"
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done; rm -rf "$WORK"; }
trap cleanup EXIT
log() { printf '\n==> %s\n' "$*"; }
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' "$@"; }

export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-minioadmin}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-minioadmin}"
export AWS_EC2_METADATA_DISABLED=true

if [[ -z "${S3_TEST_ENDPOINT:-}" ]]; then
  MINIO="$CACHE/minio"
  if [[ ! -x "$MINIO" ]]; then
    log "Downloading the MinIO server into $MINIO"
    mkdir -p "$CACHE"
    fetch -o "$MINIO.download" https://dl.min.io/server/minio/release/linux-amd64/minio && mv "$MINIO.download" "$MINIO" && chmod +x "$MINIO"
  fi
  mkdir -p "$WORK/minio"
  MINIO_ROOT_USER="$AWS_ACCESS_KEY_ID" MINIO_ROOT_PASSWORD="$AWS_SECRET_ACCESS_KEY" MINIO_BROWSER=off \
    "$MINIO" server "$WORK/minio" --address "127.0.0.1:$MINIO_PORT" >"$WORK/minio.log" 2>&1 &
  PIDS+=($!)
  export S3_TEST_ENDPOINT="http://127.0.0.1:$MINIO_PORT"
  for _ in $(seq 1 60); do curl -sf -o /dev/null "$S3_TEST_ENDPOINT/minio/health/live" && break; sleep 0.5; done
  curl -sf -o /dev/null "$S3_TEST_ENDPOINT/minio/health/live" || { tail -20 "$WORK/minio.log" >&2; echo "MinIO never came up" >&2; exit 1; }
  log "MinIO at $S3_TEST_ENDPOINT"
fi

if [[ -z "${REDIS_URL:-}" ]]; then
  REDIS="$CACHE/redis-server-6.0.9/redis-server"
  if [[ ! -x "$REDIS" ]]; then
    log "Downloading redis-server 6.0.9 (the PyPI wheel) into $(dirname "$REDIS")"
    WHEEL_URL="$(curl -fsS https://pypi.org/pypi/redis-server/6.0.9/json \
      | python3 -c 'import sys,json;print([u["url"] for u in json.load(sys.stdin)["urls"] if "cp38-cp38-manylinux2010_x86_64" in u["filename"]][0])')"
    mkdir -p "$(dirname "$REDIS")"
    fetch -o "$WORK/redis-server.whl" "$WHEEL_URL"
    unzip -qo -j "$WORK/redis-server.whl" "redis_server/bin/redis-server" -d "$(dirname "$REDIS")"
    chmod +x "$REDIS"
  fi
  "$REDIS" --port "$REDIS_PORT" --bind 127.0.0.1 --save "" --appendonly no --dir "$WORK" >"$WORK/redis.log" 2>&1 &
  PIDS+=($!)
  export REDIS_URL="redis://127.0.0.1:$REDIS_PORT"
  sleep 1
  log "Redis at $REDIS_URL"
fi

status=0
run() {  # <label> <cargo args…>
  log "$1"; shift
  if cargo test "$@"; then echo "OK: $*"; else echo "FAILED: $*" >&2; status=1; fi
}
run "adapters integration tests (Postgres + default features)" -p batlehub-adapters --test '*'
run "S3 storage" -p batlehub-adapters --features storage-s3 --test s3_storage
for t in redis_cache redis_rate_limit redis_warm_coordinator; do
  run "Redis: $t" -p batlehub-adapters --features cache-redis --test "$t"
done
[[ $status -eq 0 ]] && log "EXTERNAL-TESTS-OK" || { log "EXTERNAL-TESTS-FAILED"; exit 1; }
