#!/usr/bin/env bash
# Arrange the two arms of the conda index measurement (scenario 12).
#
# One registry has a block and the other does not; that is the entire
# difference between them, and this script is what creates it. It then proves
# the arrangement took — an unfiltered arm that is quietly being filtered, or a
# filtered arm with nothing to filter, would produce two numbers that look like
# a measurement and are the same measurement twice.
#
# Pre-requisites (all must be running):
#   task compose:db           — PostgreSQL
#   task perf:conda:upstream  — mock upstream on :9999, channel-sized index
#   task perf:conda:server    — BatleHub with perf/config.perf-conda.toml
#
# Usage: bash perf/scripts/seed_conda.sh [BASE_URL]
set -euo pipefail

BASE="${1:-http://localhost:8080}"
TOKEN="${BATLEHUB_TOKEN:-perf-admin-token}"
AUTH="Authorization: Bearer $TOKEN"
PLAIN_REG="${CONDA_REGISTRY:-perf-conda}"
FILTERED_REG="${CONDA_FILTERED_REGISTRY:-perf-conda-filtered}"
PLATFORM="${CONDA_PLATFORM:-linux-64}"
# The entry the block names. Index entry 0 exists for every `--conda-packages`
# value, so the seed does not have to know how big the channel is.
BLOCK_NAME="${CONDA_BLOCK_NAME:-perf-conda-0000}"
BLOCK_VERSION="${CONDA_BLOCK_VERSION:-1.0.0}"
BLOCKED_FILE="${BLOCK_NAME}-${BLOCK_VERSION}-py311_0.conda"
CODE='%{http_code}'

echo "==> conda filter seed  base=$BASE  plain=$PLAIN_REG  filtered=$FILTERED_REG"

# ── 1. Wait for the server ────────────────────────────────────────────────────
echo -n "    waiting for server..."
for i in $(seq 1 60); do
  if curl -sf "$BASE/healthz" >/dev/null 2>&1; then
    echo " ok"
    break
  fi
  sleep 1
  if [[ "$i" -eq 60 ]]; then
    echo " TIMEOUT — is 'task perf:conda:server' running?"
    exit 1
  fi
done

# ── 2. Verify the mock upstream is up ─────────────────────────────────────────
# Checked before the block rather than after: with the upstream down, every
# index request is a 502, the filtered check below would pass for the wrong
# reason (no document, so no blocked filename in it) and the run would report
# the arms as arranged.
UPSTREAM="${CONDA_UPSTREAM:-http://localhost:9999}"
echo -n "    checking mock upstream at $UPSTREAM..."
STATUS=$(curl -s -o /dev/null -w "$CODE" "$UPSTREAM/health" || echo "000")
if [[ "$STATUS" != "200" ]]; then
  echo " NOT RUNNING (HTTP $STATUS)"
  echo "    Start it with: task perf:conda:upstream PACKAGES=200000"
  exit 1
fi
echo " ok"

# ── 3. Block one version in the filtered registry ─────────────────────────────
# Idempotent: blocking an already-blocked coordinate is not an error, so the
# script can be re-run between scenario arms without resetting the database.
echo -n "    blocking $BLOCK_NAME@$BLOCK_VERSION in $FILTERED_REG..."
STATUS=$(curl -s -o /dev/null -w "$CODE" -X POST "$BASE/api/v1/admin/packages/block" \
  -H "$AUTH" -H "Content-Type: application/json" \
  -d "{\"registry\":\"$FILTERED_REG\",\"name\":\"$BLOCK_NAME\",\"version\":\"$BLOCK_VERSION\",\"reason\":\"perf scenario 12 — one block is all the filtering path needs\"}")
if [[ "$STATUS" != "200" ]]; then
  echo " FAILED (HTTP $STATUS)"
  echo "    The admin token needs 'packages:block' on $FILTERED_REG."
  exit 1
fi
echo " ok"

# ── 4. Prove each arm is what it claims to be ─────────────────────────────────
# The blocked set is read from a 30-second snapshot
# (`ProxyService::blocked_snapshot_fingerprint`), so a check run immediately
# after the block above can legitimately still see the unfiltered document.
# Waiting for the snapshot to turn over is part of arranging the experiment.
echo -n "    waiting for the blocked-set snapshot to turn over"
FILTERED_OK=0
for _ in $(seq 1 45); do
  BODY=$(curl -sf -H "$AUTH" "$BASE/proxy/$FILTERED_REG/$PLATFORM/repodata.json" || true)
  if [[ -n "$BODY" ]] && ! grep -qF "$BLOCKED_FILE" <<<"$BODY"; then
    FILTERED_OK=1
    break
  fi
  echo -n "."
  sleep 2
done
if [[ "$FILTERED_OK" -ne 1 ]]; then
  echo " FAILED"
  echo "    $FILTERED_REG still advertises $BLOCKED_FILE — the filtered arm would"
  echo "    measure the same path as the unfiltered one. Check the server log."
  exit 1
fi
echo " ok"

echo -n "    checking $PLAIN_REG is NOT filtered..."
if curl -sf -H "$AUTH" "$BASE/proxy/$PLAIN_REG/$PLATFORM/repodata.json" | grep -qF "$BLOCKED_FILE"; then
  echo " ok"
else
  echo " FAILED"
  echo "    $PLAIN_REG does not advertise $BLOCKED_FILE, so it is being filtered too."
  echo "    Both arms would then measure the same path. Unblock it:"
  echo "      curl -X POST $BASE/api/v1/admin/packages/unblock -H 'Authorization: Bearer $TOKEN' \\"
  echo "        -H 'Content-Type: application/json' \\"
  echo "        -d '{\"registry\":\"$PLAIN_REG\",\"name\":\"$BLOCK_NAME\",\"version\":\"$BLOCK_VERSION\"}'"
  exit 1
fi

# ── 5. Report the document's size, which is the independent variable ──────────
# A herestring and not `read … < <(curl …)`: curl's `-w` output has no trailing
# newline, so `read` reaches EOF, returns 1, and `set -e` ends the script —
# which is exactly what happened the first time this ran, silently, leaving the
# server and the mock upstream running and the scenario never started. `<<<`
# appends the newline `read` needs.
for reg in "$PLAIN_REG" "$FILTERED_REG"; do
  for doc in "repodata.json" "repodata.json.zst"; do
    measured=$(curl -s -o /dev/null -w "%{http_code} %{size_download}" \
      -H "$AUTH" "$BASE/proxy/$reg/$PLATFORM/$doc" || echo "000 0")
    read -r code size <<<"$measured"
    printf "    %-22s %-18s HTTP %s  %s MiB\n" "$reg" "$doc" "$code" \
      "$(awk -v b="${size:-0}" 'BEGIN{printf "%.1f", b/1048576}')"
  done
done

cat <<EOF

==> Both arms are arranged. Run the scenario:

    task perf:run:conda

    or one arm at a time:

    BATLEHUB_CONDA_ARM=plain_unfiltered k6 run perf/k6/scenarios/12_conda_filter.js
    BATLEHUB_CONDA_ARM=plain_filtered   k6 run perf/k6/scenarios/12_conda_filter.js
    BATLEHUB_CONDA_ARM=zst_filtered     k6 run perf/k6/scenarios/12_conda_filter.js

EOF
