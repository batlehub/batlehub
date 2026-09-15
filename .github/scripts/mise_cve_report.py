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


def collect(report: dict, root: str, *, only_fixable: bool) -> tuple[dict, Counter]:
    per_tool: dict[str, list[dict]] = defaultdict(list)
    totals: Counter = Counter()
    for result in report.get("Results") or []:
        tool = tool_of(result.get("Target", "?"), root)
        for vuln in result.get("Vulnerabilities") or []:
            if only_fixable and not vuln.get("FixedVersion"):
                continue
            severity = vuln.get("Severity", "UNKNOWN")
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
    return per_tool, totals


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
    if only_fixable:
        lines.append("")
        lines.append("*Only findings with a fixed version upstream are counted: the fix for every")
        lines.append("one of them is a version bump in `mise.toml`.*")
    lines.append("")

    if per_tool:
        lines.append("| Tool | Worst | Findings | What to bump to |")
        lines.append("|---|---|---|---|")
        ranked = sorted(
            per_tool.items(),
            key=lambda kv: (SEVERITY_ORDER.index(worst(kv[1])), -len(kv[1])),
        )
        for tool, findings in ranked:
            fixes = sorted({f["fixed"] for f in findings if f["fixed"]})
            # The fix column names the packages, not versions: a Go binary's
            # findings are against its vendored modules, and "bump the tool"
            # is the only lever whatever the module versions say.
            packages = sorted({f["package"] for f in findings if f["package"]})[:3]
            hint = ", ".join(packages) + ("…" if len(packages) == 3 else "")
            lines.append(
                f"| `{tool}` | {worst(findings)} | {len(findings)} | {hint or '—'}"
                f"{' (fix available)' if fixes else ''} |"
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
    args = ap.parse_args()

    report = json.loads(args.trivy_json.read_text(encoding="utf-8"))
    per_tool, totals = collect(report, args.root, only_fixable=not args.include_unfixed)
    blocking = sum(totals[s] for s in BLOCKING)

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
