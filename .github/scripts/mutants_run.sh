#!/usr/bin/env bash
# Tier-1 mutation testing in two invocations (RFC 0041 §6.4), shared by
# mutants.yaml (nightly and PR, from the base on a PR) and `task mutants`.
#
#   mutants_run.sh <cargo-mutants> <config> <out-dir> [cargo-mutants args…]
#
# The crates/core paths run with --test-workspace=true, because crates/web's
# suites prove their refusals too. cargo-mutants 27.1.0 still runs *its*
# baseline per package and sets the timeout from it, so the workspace suite is
# run here first: red, and every core mutant would read as caught; slow, and
# every one would time out.
#
# Exit 0 when both invocations ended 0 (all caught), 2 (some missed) or 3
# (some timed out) — survivors are reported, not failed (the list is worked
# down) — and 1 otherwise: a red baseline (cargo-mutants' 4), a usage error.
# No errexit: a missed mutant's exit 2 must not skip the second invocation.
set -uo pipefail
if [[ $# -lt 3 ]]; then
  echo "usage: $0 <cargo-mutants> <config> <out-dir> [cargo-mutants args…]" >&2
  exit 1
fi
cm=$1 cfg=$2 out=$3
shift 3
mkdir -p "$out" # cargo-mutants does not create the parent of -o

# PR mode (MUTANTS_PR_BUDGET=<minutes>): a pull request's changed lines in
# the time a CI job has. Each mutant is tested by its own package's suite —
# a crates/core mutant otherwise rebuilds every crate's tests, ~5 min each —
# so a mutant only crates/web's suites kill reads MISSED here; the nightly
# runs the workspace. At most MUTANTS_PR_MAX mutants (an even spread, by
# --shard), and whatever the budget leaves unexamined is reported, not failed.
if [[ -n "${MUTANTS_PR_BUDGET:-}" ]]; then
  max=${MUTANTS_PR_MAX:-40}
  n=$("$cm" mutants --config "$cfg" "$@" --list 2>/dev/null | grep -c . || true)
  shard=()
  if [[ "$n" -gt "$max" ]]; then
    k=$(((n + max - 1) / max))
    shard=(--shard "0/$k")
    echo "::warning::$n mutants on the changed lines; examining 1 in $k (MUTANTS_PR_MAX=$max), the nightly covers the rest"
  fi
  echo "$n mutants on the changed lines"
  rc=0
  timeout --signal=INT --kill-after=60 "$((MUTANTS_PR_BUDGET * 60))" \
    "$cm" mutants --config "$cfg" "$@" "${shard[@]}" --test-workspace=false -o "$out/pr" || rc=$?
  missed=$(cat "$out"/pr/mutants.out/missed.txt 2>/dev/null || true)
  if [[ -n "$missed" ]]; then
    echo "MISSED — no test noticed these changes:"
    printf '%s\n' "$missed"
  fi
  case $rc in
    0 | 2 | 3) exit 0 ;;
    124 | 130 | 137)
      echo "::warning::the ${MUTANTS_PR_BUDGET}-minute budget ran out; the outcomes so far are reported"
      exit 0
      ;;
    *)
      echo "::error::cargo-mutants exited $rc (4 is a red baseline)"
      exit 1
      ;;
  esac
fi

start=$(date +%s)
if ! cargo test --workspace --locked >"$out/workspace-baseline.log" 2>&1; then
  tail -n 40 "$out/workspace-baseline.log"
  echo "::error::cargo test --workspace is red before any mutant — every crates/core mutant would read as caught"
  exit 1
fi
secs=$(($(date +%s) - start))
echo "workspace baseline green in ${secs}s"

core=0
"$cm" mutants --config "$cfg" "$@" --test-workspace=true --minimum-test-timeout $((secs * 3 + 60)) \
  --exclude 'crates/web/**' --exclude 'crates/adapters/**' -o "$out/core" || core=$?
rest=0
"$cm" mutants --config "$cfg" "$@" --exclude 'crates/core/**' -o "$out/rest" || rest=$?
echo "exit codes: core=$core rest=$rest"

missed=$(cat "$out"/*/mutants.out/missed.txt 2>/dev/null || true)
if [[ -n "$missed" ]]; then
  echo "MISSED — no test noticed these changes:"
  printf '%s\n' "$missed"
fi
for rc in $core $rest; do
  case $rc in
    0 | 2 | 3) ;;
    *)
      echo "::error::cargo-mutants exited $rc (4 is a red baseline)"
      exit 1
      ;;
  esac
done
