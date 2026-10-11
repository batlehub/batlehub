#!/usr/bin/env bash
# Database statements per request on the critical paths, against a budget.
#
# A real server on a real Postgres, with sqlx logging every statement it runs
# (`sqlx::query=debug`); db_calls.py drives one request at a time and counts
# what landed in the log in between. The budget (db_calls.budget.json) is what
# the last `--update` measured, so a change that adds a query to a hot path
# fails here with the statement named, and one that removes a query says so.
#
#   DATABASE_URL=… bash tests/heavy/db_calls.sh            # check the budget
#   DATABASE_URL=… bash tests/heavy/db_calls.sh --update   # rewrite it
#
# uv provides psycopg, which mints the PAT the token-auth scenarios present.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
heavy_init db-calls 8122 8132

export DB_CALLS_MOCK_PORT="${DB_CALLS_MOCK_PORT:-8142}"
python3 tests/heavy/db_calls.py mock "$DB_CALLS_MOCK_PORT" &
HEAVY_EXTRA_PIDS+=($!)

# Every statement, and nothing else at debug: the count is read from this log.
# The request span is INFO: without it enabled, no statement carries its
# request_id and a handler's own queries cannot be told from the workers'.
export RUST_LOG="warn,sqlx::query=debug,batlehub::server_factory=info" NO_COLOR=1
heavy_start_server tests/heavy/config.db-calls.toml

uv run --quiet --with "psycopg[binary]" python3 tests/heavy/db_calls.py run \
  "$HEAVY_BASE" "$HEAVY_WORK/server.log" tests/heavy/db_calls.budget.json "$@"
# The scan worker's idle cost: the same server with the worker embedded and no
# traffic at all. Measured on its own because the requests above run without
# it — its polls would land in their windows.
heavy_stop_server
# The restart truncates server.log; keep the requests' half for HEAVY_KEEP_WORK.
mv "$HEAVY_WORK/server.log" "$HEAVY_WORK/server.requests.log"
sed 's/^roles = \["proxy"\]$/roles = ["proxy", "worker"]/' \
  tests/heavy/config.db-calls.toml > "$HEAVY_WORK/config.worker.toml"
grep -q '"worker"' "$HEAVY_WORK/config.worker.toml" \
  || heavy_fail "config.db-calls.toml no longer has the roles line this rewrites"
heavy_start_server "$HEAVY_WORK/config.worker.toml"
python3 tests/heavy/db_calls.py idle "$HEAVY_WORK/server.log"
heavy_log DB-CALLS-OK
