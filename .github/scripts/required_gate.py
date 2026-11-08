#!/usr/bin/env python3
"""The one required check of a workflow (RFC 0041 decisions 28 and 41).

A workflow-level `paths:` filter leaves a required check "Expected — waiting
for status" forever on a pull request it skips, and a skipped job counts as a
pass. So each workflow runs on every pull request, decides *inside* a job
whether its paths changed, and ends with one `<workflow> / required` job that
is the only check the `main` ruleset requires. Two subcommands:

  changed   prints `true` when a changed path matches one of the globs, or
            when the change set cannot be known (a schedule, a manual run, a
            new branch, a force push whose `before` is gone): a full run.
            `**` crosses directories, `*` does not.

  verdict   reads `toJSON(needs)` and exits 1 unless the `gate` job (if
            the workflow has one) succeeded and every other job succeeded — or the gate skipped the
            suite (`run=false`), in which case skipped is a pass, except when
            the skip was Dependabot's and a lockfile changed (decision 41).

Usage (in a workflow; `fetch-depth: 0` so the merge base is there):
  required_gate.py changed --event "$EVENT" --base "$BASE" --head "$HEAD" -- 'crates/**' '**/Cargo.lock'
  required_gate.py verdict "$NEEDS_JSON"
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

FULL_RUN_EVENTS = {"schedule", "workflow_dispatch"}
ZERO_SHA = "0" * 40


def glob_re(glob: str) -> re.Pattern[str]:
    """`**/` is zero or more directories, `**` anything, `*` one segment."""
    out, i = "", 0
    while i < len(glob):
        if glob.startswith("**/", i):
            out, i = out + "(?:.*/)?", i + 3
        elif glob.startswith("**", i):
            out, i = out + ".*", i + 2
        elif glob[i] == "*":
            out, i = out + "[^/]*", i + 1
        elif glob[i] == "?":
            out, i = out + "[^/]", i + 1
        else:
            out, i = out + re.escape(glob[i]), i + 1
    return re.compile(out + r"\Z")


def any_match(paths: list[str], globs: list[str]) -> bool:
    res = [glob_re(g) for g in globs]
    return any(r.match(p) for p in paths for r in res)


def changed_paths(base: str, head: str, cwd: str | None = None) -> list[str] | None:
    """`git diff --name-only base...head`, or None when it cannot be known."""
    if not base or not head or base == ZERO_SHA:
        return None
    try:
        out = subprocess.run(
            ["git", "diff", "--name-only", f"{base}...{head}"],
            check=True,
            capture_output=True,
            text=True,
            cwd=cwd,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        print(f"required_gate: cannot diff {base}...{head} ({e}); full run", file=sys.stderr)
        return None
    return [line for line in out.splitlines() if line]


def changed(event: str, base: str, head: str, globs: list[str], cwd: str | None = None) -> bool:
    if event in FULL_RUN_EVENTS:
        return True
    paths = changed_paths(base, head, cwd)
    return True if paths is None else any_match(paths, globs)


def verdict(needs: dict) -> tuple[bool, str]:
    """(passes, why). `needs` is the workflow's `toJSON(needs)`."""
    # A workflow without a path filter has no gate: every job must succeed.
    gate = needs.get("gate", {"result": "success", "outputs": {"run": "true"}})
    if gate.get("result") != "success":
        return False, f"the gate job did not succeed ({gate.get('result')})"
    outputs = gate.get("outputs") or {}
    others = {name: n.get("result") for name, n in needs.items() if name != "gate"}
    if outputs.get("run") != "true":
        if outputs.get("reason") == "dependabot" and outputs.get("lockfile") == "true":
            return False, "Cargo.lock changed but the Rust suite was skipped — add the full-test label"
        ran = {k: v for k, v in others.items() if v not in ("skipped", "success")}
        if ran:
            return False, f"the suite was skipped, yet these jobs did not pass: {ran}"
        return True, f"skipped by the gate ({outputs.get('reason') or 'no relevant change'})"
    bad = {k: v for k, v in others.items() if v != "success"}
    if bad:
        return False, "not every job succeeded: " + ", ".join(f"{k}={v}" for k, v in sorted(bad.items()))
    return True, f"all {len(others)} jobs succeeded"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("changed")
    c.add_argument("--event", required=True)
    c.add_argument("--base", default="")
    c.add_argument("--head", default="")
    c.add_argument("globs", nargs="+")
    v = sub.add_parser("verdict")
    v.add_argument("needs_json")
    a = p.parse_args(argv)
    if a.cmd == "changed":
        print("true" if changed(a.event, a.base, a.head, a.globs) else "false")
        return 0
    try:
        needs = json.loads(a.needs_json)
    except json.JSONDecodeError as e:
        print(f"::error::required_gate: needs is not JSON ({e})")
        return 1
    ok, why = verdict(needs)
    print(why if ok else f"::error::{why}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
