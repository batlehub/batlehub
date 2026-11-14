#!/usr/bin/env bash
# RFC 0049 §6.4: the shipped configs validate against the committed schema,
# checked by taplo — the engine editors run, so what passes here is what an
# operator's editor accepts.
#
#   config.example*.toml         against config.schema.json (complete files)
#   each deploy/helm render      against config-layer.schema.json: a first
#                                layer may leave database.url to the
#                                credentials Secret, so only types and
#                                unknown keys are checked
#
# Run by `task docs:config-schema:check` and by helm-lint.yaml. Needs taplo,
# helm (with the chart's dependencies built) and python3 with PyYAML; set
# PYTHON to another interpreter command if python3 lacks it.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
python=${PYTHON:-python3}
full="file://$root/docs/public/schema/config.schema.json"
layer="file://$root/docs/public/schema/config-layer.schema.json"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fail=0
for f in config.example*.toml; do
  if taplo check --schema "$full" "$f" >"$tmp/out" 2>&1; then
    echo "  ok  $f"
  else
    echo "$f: does not validate against config.schema.json" >&2
    grep -v '^ *\(INFO\|ERROR\)' "$tmp/out" >&2 || true
    fail=1
  fi
done

for f in deploy/helm/values-*.yaml helm/batlehub/ci/values-integers.yaml; do
  helm template batlehub ./helm/batlehub -f "$f" >"$tmp/rendered.yaml"
  # shellcheck disable=SC2086 # PYTHON may be a command with arguments
  $python - "$tmp/rendered.yaml" "$tmp" <<'PY'
import sys, yaml
n = 0
for d in yaml.safe_load_all(open(sys.argv[1])):
    for k, v in ((d or {}).get("stringData") or {}).items() if (d or {}).get("kind") == "Secret" else ():
        if k.endswith(".toml"):
            n += 1
            open(f"{sys.argv[2]}/{k}", "w").write(v)
if not n:
    sys.exit("no config Secret rendered")
PY
  for t in "$tmp"/*.toml; do
    if taplo check --schema "$layer" "$t" >"$tmp/out" 2>&1; then
      echo "  ok  $f ($(basename "$t"))"
    else
      echo "$f: the rendered $(basename "$t") does not validate against config-layer.schema.json" >&2
      grep -v '^ *\(INFO\|ERROR\)' "$tmp/out" >&2 || true
      fail=1
    fi
    rm -f "$t"
  done
done

[ "$fail" -eq 0 ] || exit 1
echo "every example config and chart render validates against the schema"
