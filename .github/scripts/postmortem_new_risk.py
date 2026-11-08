#!/usr/bin/env python3
"""The postmortem gate of one dependency root (RFC 0041 §6.5).

Reads what the mise-pinned postmortem wrote and decides; it never calls the
network itself.

    postmortem_new_risk.py --root ui --tree tree.json [--sarif tree.sarif]
                           [--diff diff.json] [--fail-on high|critical]

`--tree` is `postmortem tree <root> --vulns --json`: the advisories, the
root's package count and the diagnostics. `--sarif` is the same tree as
SARIF, for the Security tab; `--diff` is the offline `postmortem diff
base/<root> <root> --json` of a pull request.

Exit 1 on any of:

- a lookup that did not complete. postmortem 2.9.2 exits 0 and writes an
  empty SARIF when vuln.mlab.sh does not answer, `--fail-on-vuln` included;
  only the JSON's `vuln_scan_failed` diagnostic says so. And `diff --vulns`
  writes `assessment: null` for a clean package *and* for one never looked up,
  which is why the advisories come from the tree and `diff` only says which
  packages the change adds or changes (§17 M18);
- more packages than the caller's vuln.mlab.sh tier reads — 512 anonymous,
  1 024 with `VULN_MLAB_TOKEN` — since the service cuts the scan silently
  (decision 50);
- a SARIF absent, without `runs[].results`, or holding a different number of
  advisories than the JSON (one of the two runs lost its lookup);
- a `MAL-` advisory, anywhere in the tree, on every event (decision 4) —
  matched on the structured id, never the message, so a package *named*
  `MAL-…` blocks nothing;
- with `--diff` (`block-new`, decisions 27 and 43): an advisory at or above
  `--fail-on` against a package the change adds or changes; a new or moved
  root, or more than 50 added packages, which needs a maintainer;
- without `--diff` (push, schedule): any advisory at or above `--fail-on`.

`unknown` ranks as `high` (§4.1). Every value read from lockfile data is
escaped before printing, so a package name cannot issue a workflow command.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

RANK = {"low": 1, "medium": 2, "moderate": 2, "high": 3, "critical": 4}
# §4.1: a source that never rates (RUSTSEC) ranks as high; so does a word
# this script does not know.
UNRATED = RANK["high"]
ANONYMOUS_TIER = 512
TOKEN_TIER = 1024
MAX_ADDED = 50
VULN_RULE = "postmortem.known-vulnerability"


def esc(value: object) -> str:
    """Neutralise workflow commands in a value read from a lockfile."""
    text = str(value)
    for raw, safe in (("%", "%25"), ("\r", "%0D"), ("\n", "%0A"), ("::", ":%3A")):
        text = text.replace(raw, safe)
    return text


def rank(severity: object) -> int:
    return RANK.get(str(severity).lower(), UNRATED)


def load(path: str | None, what: str, errors: list[str]):
    if path is None:
        return None
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError) as exc:
        errors.append(f"{what} absent or unreadable ({esc(exc)}) — refusing to read it as clean")
        return None


def tree_vulns(tree: dict) -> dict[tuple[str, str], list[dict]]:
    out: dict[tuple[str, str], list[dict]] = {}
    for pkg in tree.get("vulnerabilities") or []:
        out.setdefault((str(pkg.get("name")), str(pkg.get("version"))), []).extend(pkg.get("vulns") or [])
    return out


def check_tree(tree: dict, root: str, errors: list[str]) -> None:
    for diag in tree.get("diagnostics") or []:
        if diag.get("kind") != "vuln_scan_failed":
            print(f"::warning::postmortem diagnostic on {esc(root)}: {esc(diag.get('message'))}")
            continue
        message = str(diag.get("message"))
        if "rate limit" in message.lower():
            errors.append(f"vuln.mlab.sh did not answer (rate limit) — retry later; never read as clean: {esc(message)}")
        else:
            errors.append(f"the vulnerability lookup did not complete: {esc(message)}")
    total = (tree.get("stats") or {}).get("total")
    tier = TOKEN_TIER if os.environ.get("VULN_MLAB_TOKEN") else ANONYMOUS_TIER
    if not isinstance(total, int):
        errors.append(f"{esc(root)}: no package count in the tree JSON — cannot tell a cut scan from a whole one")
    elif total > tier:
        how = "a token scan reads" if tier == TOKEN_TIER else "an anonymous scan reads"
        hint = "" if tier == TOKEN_TIER else " — not scannable without VULN_MLAB_TOKEN"
        errors.append(f"{esc(root)} has {total} packages; {how} {tier}{hint}")


def check_sarif(sarif: dict, vulns: dict, errors: list[str]) -> list[tuple[str, str]]:
    runs = sarif.get("runs") if isinstance(sarif, dict) else None
    if not runs or any(not isinstance(r.get("results"), list) for r in runs):
        errors.append("the SARIF has no runs[].results — refusing to read it as clean")
        return []
    results = [r for run in runs for r in run["results"] if r.get("ruleId") == VULN_RULE]
    expected = sum(len(v) for v in vulns.values())
    if len(results) != expected:
        errors.append(
            f"the SARIF carries {len(results)} advisories and the JSON {expected} — "
            "one of the two runs lost its lookup"
        )
    malicious = []
    for res in results:
        advisory = str((res.get("properties") or {}).get("advisory", ""))
        if advisory.startswith("MAL-"):
            text = str((res.get("message") or {}).get("text", ""))
            malicious.append((text.split(": ", 1)[0], advisory))
    return malicious


def blocking(pkg: str, version: str, found: list[dict], fail_on: int) -> list[str]:
    return [
        f"vulnerable dependency {esc(pkg)}@{esc(version)} — {esc(v.get('id'))} "
        f"({esc(v.get('severity', 'unknown'))}); pick a fixed version"
        for v in found
        if not str(v.get("id", "")).startswith("MAL-") and rank(v.get("severity", "unknown")) >= fail_on
    ]


def check_diff(diff: dict, vulns: dict, root: str, fail_on: int, errors: list[str]) -> None:
    if diff.get("schema_version") != 2:
        errors.append(f"unknown postmortem diff schema {esc(diff.get('schema_version'))} — refusing to judge it")
        return
    summary = diff.get("summary") or {}
    added = diff.get("added") or []
    changed = diff.get("changed") or []
    # `diff` against a side without a lockfile reports the whole tree as added.
    new_root = summary.get("unchanged", 0) == 0 and summary.get("removed", 0) == 0 and not changed and added
    if new_root or len(added) > MAX_ADDED:
        errors.append(
            f"new or moved dependency root ({esc(root)}), or {len(added)} packages added — "
            "needs a maintainer's approving review of this head"
        )
        return
    for pkg in added:
        name, version = str(pkg.get("name")), str(pkg.get("version"))
        errors.extend(blocking(name, version, vulns.get((name, version), []), fail_on))
    for pkg in changed:
        name, version = str(pkg.get("name")), str(pkg.get("to"))
        errors.extend(blocking(name, version, vulns.get((name, version), []), fail_on))
    print(f"{esc(root)}: {len(added)} added, {len(changed)} changed, {summary.get('removed', 0)} removed")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", required=True, help="the dependency root, for the messages")
    ap.add_argument("--tree", required=True)
    ap.add_argument("--sarif")
    ap.add_argument("--diff")
    ap.add_argument("--fail-on", choices=["high", "critical"], default="high")
    args = ap.parse_args(argv)
    fail_on = RANK[args.fail_on]

    errors: list[str] = []
    tree = load(args.tree, "the postmortem tree JSON", errors)
    vulns = tree_vulns(tree) if isinstance(tree, dict) else {}
    if isinstance(tree, dict):
        check_tree(tree, args.root, errors)
    elif tree is not None:
        errors.append("the postmortem tree JSON is not an object — refusing to read it as clean")

    malicious = {(f"{n}@{v}", x["id"]) for (n, v), found in vulns.items() for x in found if str(x.get("id", "")).startswith("MAL-")}
    if args.sarif is not None:
        sarif = load(args.sarif, "the postmortem SARIF", errors)
        if sarif is not None:
            malicious.update(check_sarif(sarif, vulns, errors))
    for pkg, advisory in sorted(malicious):
        errors.append(f"known-malicious package {esc(pkg)} ({esc(advisory)}) — remove it")

    if args.diff is not None:
        diff = load(args.diff, "the postmortem diff JSON", errors)
        if isinstance(diff, dict):
            check_diff(diff, vulns, args.root, fail_on, errors)
    elif isinstance(tree, dict):
        for (name, version), found in sorted(vulns.items()):
            errors.extend(blocking(name, version, found, fail_on))

    for line in errors:
        print(f"::error::{line}")
    if not errors:
        print(f"{esc(args.root)}: postmortem gate passed")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
