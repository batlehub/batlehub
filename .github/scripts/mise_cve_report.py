#!/usr/bin/env python3
"""What the development toolchain is carrying, and whether that is too much.

`mise.toml` installs some forty tools, and until now nothing looked at them.
The one thing that did — `mise.lock` in `vuln-scan.yaml` — reads **4 of them**:
the scanner's lockfile parser understands the `cargo:` and PyPI backends and
nothing else, so every tool that arrives as a GitHub release asset (trivy, syft,
helm, k6, gitleaks, node, go, task, …) came back as no findings, which reads
exactly like a clean scan.

So the instrument is Trivy over the *installed* toolchain rather than over the
lockfile. Trivy reads the Go module metadata embedded in a Go binary, the Node
and Python trees as they are laid out on disk, and the rest by version — the
same analysis it performs on a container image, pointed at `~/.local/share/mise`
instead. That finds what the lockfile scan cannot.

This script turns the resulting JSON into three things:

* a **markdown report** — per tool, worst severity first, so a bump has an
  obvious target;
* a **budget verdict** — `--budget` is the number of fixable HIGH+CRITICAL
  findings this toolchain is allowed to carry. Over it, the script exits 1;
* a **budget file** (`--write-budget`), so the current number can be recorded
  deliberately rather than discovered during a review.

Why a budget rather than "zero findings": the toolchain is not the product. A
CVE in `k9s` does not reach anything this project ships, and a gate that fails
every pull request on somebody else's release cadence gets switched off within
the week. A budget goes red when the number *grows*, which is the event worth
a human's attention.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
from collections import Counter, defaultdict
from pathlib import Path

SEVERITY_ORDER = ["CRITICAL", "HIGH", "MEDIUM", "LOW", "UNKNOWN"]
BLOCKING = ("CRITICAL", "HIGH")


def tool_of(target: str, root: str) -> str:
    """The mise tool a Trivy target belongs to.

    Trivy names a target by its path *relative to the scanned root*, so with
    `installs/` scanned these arrive as `trivy/0.74.0/trivy` — and with an
    absolute path given, as the whole thing. The language analysers contribute a
    bare `Node.js`, `Python`, `Ruby` or `Java` with no path at all.

    All three shapes reduce to the first path segment after the root, which is
    the directory mise installs a tool into — the name in `mise.toml`, which is
    what someone has to edit. The version stays out of the label deliberately:
    two installed versions of one tool are one row to act on, not two.
    """
    rest = target
    if root and rest.startswith(root):
        rest = rest[len(root) :]
    marker = "installs/"
    if marker in rest:
        rest = rest.split(marker, 1)[1]
    rest = rest.lstrip("/")
    return rest.split("/", 1)[0] or target


def collect(
    report: dict, root: str, *, only_fixable: bool, keep: set[str] | None = None
) -> tuple[dict, Counter, Counter]:
    """Per-tool findings, the severity totals, and what `keep` excluded.

    `keep` is the set of install directories this repository asks for. A
    finding is dropped only when it is attributable to an install directory
    that is *not* in that set: a bare `Python` row that Trivy could not place
    stays, because dropping what cannot be attributed would quietly shrink the
    number. See `--only-tools`.
    """
    per_tool: dict[str, list[dict]] = defaultdict(list)
    totals: Counter = Counter()
    skipped: Counter = Counter()
    root_path = Path(root) if root else None
    for result in report.get("Results") or []:
        for vuln in result.get("Vulnerabilities") or []:
            if only_fixable and not vuln.get("FixedVersion"):
                continue
            # `PkgPath` first: the language analysers name their target by
            # ecosystem (`Node.js`, `Ruby`) and put the install it came from in
            # the package path, so three node versions arrive as one row and
            # the stale ones hide inside it.
            tool = tool_of(vuln.get("PkgPath") or result.get("Target", "?"), root)
            severity = vuln.get("Severity", "UNKNOWN")
            if (
                keep is not None
                and tool not in keep
                and root_path is not None
                and (root_path / tool).is_dir()
            ):
                # Counted in the same unit as the verdict — fixable HIGH and
                # CRITICAL — so "excluded" plus "counted" adds up to what a
                # machine-wide scan reports, and the narrowing is checkable.
                if severity in BLOCKING:
                    skipped[tool] += 1
                continue
            totals[severity] += 1
            per_tool[tool].append(
                {
                    "id": vuln.get("VulnerabilityID"),
                    "severity": severity,
                    "package": vuln.get("PkgName"),
                    "installed": vuln.get("InstalledVersion"),
                    "fixed": vuln.get("FixedVersion"),
                    "title": (vuln.get("Title") or "").strip(),
                }
            )
    return per_tool, totals, skipped


def upstream_latest(wanted: set[str], timeout: float = 30.0) -> dict[str, str | None]:
    """Per install directory, the newest release **a bump could actually take**.

    Every row of the fix column used to read "(fix available)" whenever the
    vulnerable *Go module* had a fixed version — which is not the same question.
    A Go binary vendors its modules, so the only lever is the tool's own release,
    and on 2026-09-18 that lever did not exist for nearly any of this report.
    helm-docs was pinned at 1.14.2 and 1.14.2 was still the newest release
    upstream, two years on, with 92 findings all advertising a fix. gitleaks
    (56), lazydocker (33), k6 (14), task (13), node (9), trivy (6), helm (2) and
    sonar-scanner-cli (2) were at their newest release too. Of the 249 findings
    that report listed, **syft's 19 were the only ones a `mise.toml` edit could
    reach** — which makes the budget a number nobody can move and the table a
    list nobody can act on. A local scan of the same toolchain the same day, on
    a newer vulnerability database, put it the same way in the budget's own
    unit: 13 of 147 fixable HIGH/CRITICAL reachable, 134 with nothing to bump
    to. (The two totals differ because the database moved, not the toolchain —
    which is why CI's number is the one the budget is recorded against.)

    So the newest release is asked for, per tool, and **within the pin**:
    `node = "24"` must be compared against the newest 24.x, not against 26.x,
    or every pinned major reads as out of date for as long as the pin is right.
    `mise ls --current --json` carries the three things that takes — the key to
    ask about, the version requested and the version installed — and the install
    directory's name, which is how Trivy labels a finding.

    Only `wanted` — the tools that actually have findings — is asked about, which
    is eleven questions rather than forty. That is not only speed: each one is a
    release listing from the tool's forge, and the runner's GitHub quota is
    shared with every other job on that IP, so the cheapest version of this is
    the one that keeps working. `GITHUB_TOKEN` in the environment is what mise
    uses to authenticate them; the workflow passes it for that reason.

    Returns `{install_dir: newest_version_or_None}`; a tool that is missing from
    the map was not resolvable and is reported as unknown rather than as either
    answer.
    """
    if not shutil.which("mise"):
        return {}
    env = {**os.environ, "MISE_GLOBAL_CONFIG_FILE": os.devnull}
    try:
        listing = json.loads(
            subprocess.run(
                ["mise", "ls", "--current", "--json"],
                capture_output=True,
                text=True,
                check=True,
                timeout=timeout,
                env=env,
            ).stdout
        )
    except (subprocess.SubprocessError, json.JSONDecodeError, OSError):
        return {}

    out: dict[str, str | None] = {}
    for key, entries in listing.items():
        for entry in entries or []:
            path = entry.get("install_path")
            if not path:
                continue
            install_dir = os.path.basename(os.path.dirname(path))
            if install_dir not in wanted or install_dir in out:
                continue
            requested = entry.get("requested_version") or "latest"
            spec = key if requested in ("latest", "") else f"{key}@{requested}"
            try:
                newest = subprocess.run(
                    ["mise", "latest", spec],
                    capture_output=True,
                    text=True,
                    check=True,
                    timeout=timeout,
                    env=env,
                ).stdout.strip()
            except (subprocess.SubprocessError, OSError):
                continue
            # An empty answer is "mise could not say", not "no newer release":
            # leaving the key out keeps the two apart in the report.
            if newest:
                out[install_dir] = newest
    return out


def installed_versions(report: dict, root: str) -> dict[str, set[str]]:
    """Per install directory, the version segment(s) Trivy scanned.

    `tool_of` drops the version on purpose — two installs of one tool are one
    row to act on. The fix column needs it back, to say what a bump would move
    *from*, so it is recovered here rather than by widening that function.
    """
    seen: dict[str, set[str]] = defaultdict(set)
    for result in report.get("Results") or []:
        for vuln in result.get("Vulnerabilities") or []:
            target = vuln.get("PkgPath") or result.get("Target", "")
            rest = target[len(root) :] if root and target.startswith(root) else target
            if "installs/" in rest:
                rest = rest.split("installs/", 1)[1]
            parts = rest.lstrip("/").split("/")
            if len(parts) >= 2 and parts[1]:
                seen[parts[0]].add(parts[1])
    return seen


def bump_target(
    tool: str,
    findings: list[dict],
    newest: dict[str, str | None],
    installed: dict[str, set[str]],
) -> tuple[str, bool]:
    """The fix column for one tool, and whether a bump can reach it.

    Three answers, and the third is the one that was missing: a newer release to
    take, no newer release at all, or nothing asked.
    """
    has_module_fix = any(f["fixed"] for f in findings)
    if not has_module_fix:
        return "—", False
    if tool not in newest:
        return "fix upstream, tool release unknown", False
    have = installed.get(tool) or set()
    latest = newest[tool]
    if have and latest not in have:
        return f"bump to {latest}", True
    return f"no newer release ({latest})", False


def worst(findings: list[dict]) -> str:
    for severity in SEVERITY_ORDER:
        if any(f["severity"] == severity for f in findings):
            return severity
    return "NONE"


def render(
    per_tool: dict,
    totals: Counter,
    budget: int,
    over: bool,
    only_fixable: bool,
    *,
    budget_recorded: bool,
    skipped: Counter | None = None,
    newest: dict[str, str | None] | None = None,
    installed: dict[str, set[str]] | None = None,
) -> str:
    blocking = sum(totals[s] for s in BLOCKING)
    lines: list[str] = []
    lines.append("<!-- mise-cve -->")
    lines.append("### 🧰 Development toolchain CVEs — `mise.toml`")
    lines.append("")
    if not budget_recorded:
        # Saying "within budget" when no budget was ever set would be a green
        # tick for a check that cannot fail — the failure mode this whole
        # workflow exists to remove.
        verdict = (
            f"⚪ **{blocking} fixable HIGH/CRITICAL** — no budget recorded yet, so this "
            "run reports and cannot fail. Record the current number with "
            "`task mise:cve:budget` to make it a gate."
        )
    elif over:
        verdict = f"🔴 **{blocking} fixable HIGH/CRITICAL**, over the budget of **{budget}**"
    else:
        verdict = f"🟢 **{blocking} fixable HIGH/CRITICAL**, within the budget of **{budget}**"
    lines.append(verdict)
    lines.append("")
    counts = " · ".join(f"{s.title()} {totals[s]}" for s in SEVERITY_ORDER if totals[s])
    lines.append(f"{sum(totals.values())} finding(s) in total — {counts or 'none'}.")
    newest = newest or {}
    installed = installed or {}
    # Which of the counted findings a `mise.toml` edit could actually reach.
    # Computed before the sentence below is written, because that sentence used
    # to claim all of them and the number is usually a fraction — see
    # `upstream_latest`.
    reachable = 0
    stuck: list[tuple[str, int]] = []
    for tool, findings in per_tool.items():
        blocking_here = sum(1 for f in findings if f["severity"] in BLOCKING)
        if not blocking_here:
            continue
        if bump_target(tool, findings, newest, installed)[1]:
            reachable += blocking_here
        else:
            stuck.append((tool, blocking_here))
    if only_fixable:
        lines.append("")
        lines.append(
            "*Counted findings all have a fixed version in the vulnerable **package**. "
            "That is not the same as a fix this repository can take: a Go binary vendors "
            "its modules, so the only lever is the tool's own release.*"
        )
        if newest:
            lines.append("")
            if stuck:
                worst_stuck = ", ".join(
                    f"`{t}` ({n})" for t, n in sorted(stuck, key=lambda kv: -kv[1])[:4]
                )
                lines.append(
                    f"**{reachable} of {blocking} are reachable by a version bump.** The other "
                    f"{blocking - reachable} are in tools already at their newest release — "
                    f"{worst_stuck}{', …' if len(stuck) > 4 else ''} — where there is nothing to "
                    "bump to and the only levers left are dropping the tool or living with it."
                )
            else:
                lines.append(
                    f"**All {reachable} are reachable by a version bump** — every tool below has "
                    "a newer release than the one installed."
                )
    if skipped:
        # Printed, not assumed: a narrowed scope that goes unmentioned is a
        # number that looks like an improvement.
        biggest = ", ".join(f"`{t}` ({n})" for t, n in skipped.most_common(4))
        lines.append("")
        lines.append(
            f"*{sum(skipped.values())} fixable HIGH/CRITICAL in {len(skipped)} install "
            f"director(ies) this "
            f"repository does not ask for are **not** counted — {biggest}"
            f"{', …' if len(skipped) > 4 else ''}. They belong to another project's "
            "`mise.toml` or to the global config, and a clean CI runner never installs "
            "them, which is what makes this number comparable with CI's.*"
        )
    lines.append("")

    if per_tool:
        lines.append("| Tool | Worst | Findings | Vulnerable packages | What to bump to |")
        lines.append("|---|---|---|---|---|")
        ranked = sorted(
            per_tool.items(),
            key=lambda kv: (SEVERITY_ORDER.index(worst(kv[1])), -len(kv[1])),
        )
        for tool, findings in ranked:
            # The packages name *what* is vulnerable; the target says whether
            # anything can be done about it. Both are needed: "(fix available)"
            # alone read as actionable for 195 findings that were not.
            packages = sorted({f["package"] for f in findings if f["package"]})[:3]
            hint = ", ".join(packages) + ("…" if len(packages) == 3 else "")
            target, _ = bump_target(tool, findings, newest, installed)
            lines.append(
                f"| `{tool}` | {worst(findings)} | {len(findings)} | {hint or '—'} | {target} |"
            )
        lines.append("")
        lines.append("<details><summary>Every finding</summary>")
        lines.append("")
        lines.append("| Tool | CVE | Severity | Package | Installed | Fixed in |")
        lines.append("|---|---|---|---|---|---|")
        for tool, findings in ranked:
            for f in sorted(findings, key=lambda f: SEVERITY_ORDER.index(f["severity"])):
                lines.append(
                    f"| `{tool}` | {f['id']} | {f['severity']} | {f['package']} | "
                    f"{f['installed']} | {f['fixed'] or '—'} |"
                )
        lines.append("")
        lines.append("</details>")
    else:
        lines.append("No findings. 🎉")
    lines.append("")
    lines.append(
        "*The toolchain is not the product — none of this ships. What a growing number "
        "means is that the tools this repository builds and scans with are themselves "
        "going stale, which is why this is a budget rather than a zero.*"
    )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--trivy-json", type=Path, required=True, help="`trivy rootfs --format json` output")
    ap.add_argument("--root", default="", help="the scanned directory, stripped from target names")
    ap.add_argument("--out", type=Path, default=Path("mise-cve-report.md"))
    ap.add_argument("--json-out", type=Path, help="also write the per-tool counts as JSON")
    ap.add_argument(
        "--budget-file",
        type=Path,
        default=Path(".github/mise-cve-budget.json"),
        help="the recorded budget; missing means no verdict",
    )
    ap.add_argument("--budget", type=int, help="override the recorded budget")
    ap.add_argument(
        "--include-unfixed",
        action="store_true",
        help="count findings with no fixed version (default: only fixable ones)",
    )
    ap.add_argument("--write-budget", action="store_true", help="record the current count as the budget")
    ap.add_argument(
        "--no-upstream-check",
        action="store_true",
        help="skip asking mise for each tool's newest release (offline, or mise unavailable). "
        "The fix column then names the vulnerable packages only, as it did before.",
    )
    ap.add_argument(
        "--only-tools",
        default="",
        help="comma-separated install directories to count (e.g. from `mise ls --current --json` "
        "with the global config excluded); anything else attributable to an install directory is "
        "reported as excluded and left out of the number",
    )
    args = ap.parse_args()

    keep = {t.strip() for t in args.only_tools.split(",") if t.strip()} or None
    report = json.loads(args.trivy_json.read_text(encoding="utf-8"))
    per_tool, totals, skipped = collect(
        report, args.root, only_fixable=not args.include_unfixed, keep=keep
    )
    if skipped:
        print(
            f"scope: {sum(skipped.values())} fixable HIGH/CRITICAL in {len(skipped)} install "
            "director(ies) outside this repository's mise.toml, not counted"
        )
    blocking = sum(totals[s] for s in BLOCKING)
    newest = {} if args.no_upstream_check else upstream_latest(set(per_tool))
    if not args.no_upstream_check and not newest:
        # Said out loud: a report that silently lost the upstream column reads
        # like one where every tool happens to be current.
        print("upstream check: mise could not be asked — the fix column will not judge releases")
    installed = installed_versions(report, args.root)

    budget = args.budget
    budget_recorded = budget is not None
    if budget is None and args.budget_file.exists():
        budget = int(json.loads(args.budget_file.read_text()).get("max_fixable_high_critical", 0))
        budget_recorded = True
    if budget is None:
        # No budget has ever been recorded — report the number, do not judge it.
        # A first run on a new machine (or a first CI run) has nothing to compare
        # against, and inventing a threshold here would either fail for no
        # reason or pass for no reason.
        budget = blocking

    if args.write_budget:
        args.budget_file.parent.mkdir(parents=True, exist_ok=True)
        args.budget_file.write_text(
            json.dumps(
                {
                    "max_fixable_high_critical": blocking,
                    "recorded_totals": dict(totals),
                    "note": "Raise this deliberately, in a commit that says why. "
                    "`task mise:cve:budget` rewrites it.",
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        print(f"recorded budget: {blocking} → {args.budget_file}")

    over = budget_recorded and blocking > budget
    markdown = render(
        per_tool,
        totals,
        budget,
        over,
        not args.include_unfixed,
        budget_recorded=budget_recorded,
        skipped=skipped,
        newest=newest,
        installed=installed,
    )
    args.out.write_text(markdown, encoding="utf-8")
    print(markdown)

    if args.json_out:
        args.json_out.write_text(
            json.dumps(
                {
                    "totals": dict(totals),
                    "fixable_high_critical": blocking,
                    "budget": budget,
                    "over_budget": over,
                    "per_tool": {t: len(f) for t, f in per_tool.items()},
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

    if over:
        print(
            f"::error::the development toolchain carries {blocking} fixable HIGH/CRITICAL "
            f"findings, over its budget of {budget} — bump the tools at the top of the table, "
            "or raise the budget in a commit that says why"
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
