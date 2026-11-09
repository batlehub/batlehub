#!/usr/bin/env python3
"""The postmortem gate of one dependency root (RFC 0041 §6.5).

Reads what the mise-pinned postmortem wrote and decides. Its one network call
is GitHub's reviews API, made only when a block is one a maintainer may lift.

    postmortem_new_risk.py --root ui --tree tree.json [--sarif tree.sarif]
                           [--diff diff.json] [--fail-on high|critical]
                           [--online-diff online.json --scripts-base base.json
                            --scripts-head head.json] [--pr N]

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
- without `--diff` (push, schedule): any advisory at or above `--fail-on`;
- with `--online-diff` (`block-new` on reputation, decisions 23, 17, 49): the
  `postmortem diff --online --json` of the change, with `postmortem scripts
  --json` of both sides for the install scripts `diff` does not report. An
  added package blocks on `typosquat`, on `newborn-package` under 7 days or on
  an install script; a version change blocks on `new-publisher`, on an install
  script the old version did not have, or on `fresh-release` under 7 days. The
  other signals are reported. A `stats-failed` signal — postmortem 2.9.2 exits
  0 when GitHub does not answer (§17 M20) — fails closed and is not liftable.

`--pr` (decision 46): a reputation block, or a new or moved root, turns
report-only when the pull request's head carries an `APPROVED` review — its
`commit_id` equal to `head.sha` — from an account with the `admin` or
`maintain` role that is not the author. Read from `GITHUB_API_URL`,
`GITHUB_REPOSITORY` and `GITHUB_TOKEN`; an API error or a call over 30 s is red.
A `MAL-` package, an advisory or a lookup that did not complete is never lifted.

`unknown` ranks as `high` (§4.1). Every value read from lockfile data is
escaped before printing, so a package name cannot issue a workflow command.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

RANK = {"low": 1, "medium": 2, "moderate": 2, "high": 3, "critical": 4}
# §4.1: a source that never rates (RUSTSEC) ranks as high; so does a word
# this script does not know.
UNRATED = RANK["high"]
ANONYMOUS_TIER = 512
TOKEN_TIER = 1024
MAX_ADDED = 50
YOUNG_SECS = 7 * 86_400
API_BUDGET_SECS = 30
# `permission` reports `maintain` as `write`; `role_name` keeps it.
LIFTING_ROLES = {"admin", "maintain"}
AGE = re.compile(r"\((\d+)([smhdwy]) old\)")
UNIT_SECS = {"s": 1, "m": 60, "h": 3600, "d": 86_400, "w": 7 * 86_400, "y": 365 * 86_400}
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


def young(signal: str) -> bool:
    """`newborn-package (3d old)` / `fresh-release (3h old)`: under 7 days.
    An age this script cannot read counts as young — fail closed."""
    m = AGE.search(signal)
    return m is None or int(m.group(1)) * UNIT_SECS[m.group(2)] < YOUNG_SECS


def install_scripts(scripts: dict) -> set[str]:
    """Names with an install script. Cargo's build.rs and proc-macros are not
    install scripts (every crate may have them, decision 17)."""
    return {
        str(p.get("name"))
        for p in scripts.get("packages") or []
        if str(p.get("ecosystem")) not in ("rust", "cargo")
    }


def check_online(online: dict, base_scripts: dict, head_scripts: dict, root: str,
                 errors: list[str], liftable: list[str]) -> None:
    if online.get("schema_version") != 2:
        errors.append(f"unknown postmortem diff schema {esc(online.get('schema_version'))} — refusing to judge it")
        return
    before, after = install_scripts(base_scripts), install_scripts(head_scripts)
    failed = []
    for kind in ("added", "changed"):
        for pkg in online.get(kind) or []:
            name = str(pkg.get("name"))
            label = f"{esc(name)}@{esc(pkg.get('version') or pkg.get('to'))}"
            signals = [str(x) for x in (pkg.get("assessment") or {}).get("signals") or []]
            # `stats-failed` (GitHub) and `resolve-failed` (the registry) both
            # mean a signal was never computed; postmortem exits 0 on either.
            lookup_failed = [x for x in signals if x.endswith("-failed")]
            if lookup_failed:
                failed.append(f"{label} ({esc(', '.join(lookup_failed))})")
            if kind == "added":
                hits = [x for x in signals if x.startswith("typosquat") or (x.startswith("newborn-package") and young(x))]
                if name in after:
                    hits.append("install script")
            else:
                hits = [x for x in signals if x == "new-publisher" or (x.startswith("fresh-release") and young(x))]
                if name in after and name not in before:
                    hits.append("install script the old version did not have")
            if hits:
                liftable.append(f"{label} — {esc(', '.join(hits))}")
            others = [x for x in signals if x not in hits and x not in lookup_failed]
            if others:
                print(f"{label}: reported, not blocking — {esc(', '.join(others))}")
    if failed:
        errors.append(
            f"{esc(root)}: the reputation lookup did not complete for {len(failed)} package(s) "
            f"({', '.join(failed[:5])}) — GitHub or the registry did not answer; never read as clean{rate_limit_reset()}"
        )


def api_get(url: str, token: str, timeout: float) -> object:
    req = urllib.request.Request(url, headers={
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    })
    with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 — a fixed https API base
        return json.load(resp)


def rate_limit_reset() -> str:
    """When GitHub's core budget comes back, if it is spent — best effort."""
    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        return ""
    try:
        core = api_get(f"{os.environ.get('GITHUB_API_URL', 'https://api.github.com')}/rate_limit", token, 10)["resources"]["core"]
    except Exception:  # noqa: BLE001 — a hint, never the verdict
        return ""
    if core.get("remaining", 1) > 0:
        return ""
    return f" (GitHub's rate limit resets at {time.strftime('%H:%M:%S UTC', time.gmtime(core['reset']))})"


def approval(pr: int) -> tuple[str | None, str | None]:
    """(approver, None) when a maintainer approved the head, (None, why) otherwise."""
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    repo, token = os.environ.get("GITHUB_REPOSITORY"), os.environ.get("GITHUB_TOKEN")
    if not repo or not token:
        return None, "GITHUB_REPOSITORY or GITHUB_TOKEN unset — cannot read the reviews"
    deadline = time.monotonic() + API_BUDGET_SECS

    def get(path: str) -> object:
        left = deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError(f"the reviews API took over {API_BUDGET_SECS}s")
        return api_get(f"{api}/repos/{repo}/{path}", token, left)

    try:
        pull = get(f"pulls/{pr}")
        author, head = pull["user"]["login"], pull["head"]["sha"]
        reviews: list[dict] = []
        page = 1
        while True:
            batch = get(f"pulls/{pr}/reviews?per_page=100&page={page}")
            reviews.extend(batch)
            if len(batch) < 100:
                break
            page += 1
        # Each reviewer's latest decision on this head (the API lists oldest
        # first): an approval later replaced by a change request lifts nothing.
        latest: dict[str, str] = {}
        for review in reviews:
            login = (review.get("user") or {}).get("login")
            if review.get("commit_id") != head or not login or login == author:
                continue
            if review.get("state") in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
                latest[login] = review["state"]
        for login, state in latest.items():
            if state == "APPROVED" and get(f"collaborators/{login}/permission").get("role_name") in LIFTING_ROLES:
                return login, None
    except (urllib.error.URLError, TimeoutError, OSError, KeyError, TypeError, ValueError) as exc:
        return None, f"the reviews API failed — {esc(exc)}"
    return None, f"no approving review of head {esc(head)} by a maintainer other than the author"


def check_diff(diff: dict, vulns: dict, root: str, fail_on: int, errors: list[str],
               liftable: list[str]) -> None:
    if diff.get("schema_version") != 2:
        errors.append(f"unknown postmortem diff schema {esc(diff.get('schema_version'))} — refusing to judge it")
        return
    summary = diff.get("summary") or {}
    added = diff.get("added") or []
    changed = diff.get("changed") or []
    # `diff` against a side without a lockfile reports the whole tree as added.
    new_root = summary.get("unchanged", 0) == 0 and summary.get("removed", 0) == 0 and not changed and added
    if new_root or len(added) > MAX_ADDED:
        # Liftable, but the advisories below are judged anyway: an approval
        # never lifts a vulnerable package, however large the change.
        liftable.append(
            f"new or moved dependency root ({esc(root)}), or {len(added)} packages added — "
            "needs a maintainer's approving review of this head"
        )
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
    ap.add_argument("--online-diff")
    ap.add_argument("--scripts-base")
    ap.add_argument("--scripts-head")
    ap.add_argument("--pr", type=int, help="the pull request whose head approval lifts a liftable block")
    args = ap.parse_args(argv)
    fail_on = RANK[args.fail_on]
    if args.online_diff is not None and (args.scripts_base is None or args.scripts_head is None):
        ap.error("--online-diff needs --scripts-base and --scripts-head")

    errors: list[str] = []
    liftable: list[str] = []
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
            check_diff(diff, vulns, args.root, fail_on, errors, liftable)
    elif isinstance(tree, dict):
        for (name, version), found in sorted(vulns.items()):
            errors.extend(blocking(name, version, found, fail_on))

    if args.online_diff is not None:
        online = load(args.online_diff, "the postmortem online diff JSON", errors)
        base_scripts = load(args.scripts_base, "the base's postmortem scripts JSON", errors)
        head_scripts = load(args.scripts_head, "the head's postmortem scripts JSON", errors)
        if all(isinstance(x, dict) for x in (online, base_scripts, head_scripts)):
            check_online(online, base_scripts, head_scripts, args.root, errors, liftable)

    if liftable:
        approver, why = approval(args.pr) if args.pr is not None else (None, "no pull request to read an approval from")
        if approver:
            print(f"::notice::approved on this head by {esc(approver)} — reported, not blocking (decision 46)")
            for line in liftable:
                print(f"::warning::{line}")
        else:
            errors.extend(liftable)
            errors.append(f"{why} — a maintainer's approving review of this head turns the lines above report-only")

    for line in errors:
        print(f"::error::{line}")
    if not errors:
        print(f"{esc(args.root)}: postmortem gate passed")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
