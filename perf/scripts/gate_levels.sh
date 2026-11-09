#!/usr/bin/env bash
# What each level of security gating adds to a request: profile.sh once per
# level (perf/scripts/gate_levels.py says what each level turns on), on the
# same arms, then one table. `none` runs first and last: the two disagreeing
# by more than the levels do means the runner, not the gates, moved.
#
#   DATABASE_URL=postgresql://… bash perf/scripts/gate_levels.sh
#
# Knobs: GATE_LEVELS (none min medium high max), GATE_ARMS (an extended regex
# of soak arms, a download/metadata mix), GATE_OUT (perf/results/gate-levels),
# and profile.sh's PROFILE_SECONDS / PROFILE_RATE / PROFILE_BUILD.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
OUT="${GATE_OUT:-perf/results/gate-levels}"
LEVELS="${GATE_LEVELS:-none min medium high max}"
# Up to 5 minutes for the embedded worker to scan what `high` and `max` hold.
export PROFILE_PREFLIGHT_WAIT="${PROFILE_PREFLIGHT_WAIT:-300}"
export PROFILE_ARMS="${GATE_ARMS:-^(npm_warm_read|npm_packument|npm_cache_miss|pypi_wheel|maven_jar|cargo_download|go_zip|github_asset)$}"
rm -rf "$OUT"
mkdir -p "$OUT/layers"
for level in $LEVELS none; do
  dir="$OUT/$level"
  [[ -d "$dir" ]] && dir="$OUT/${level}-again"
  python3 -I perf/scripts/gate_levels.py layer "$level" >"$OUT/layers/$level.toml"
  printf '\n==> gate level %s\n' "$level"
  PROFILE_LAYERS="$OUT/layers/$level.toml" PROFILE_OUT="$dir" bash perf/scripts/profile.sh
done
python3 -I perf/scripts/gate_levels.py report "$OUT" | tee "$OUT/gate-levels.md"
