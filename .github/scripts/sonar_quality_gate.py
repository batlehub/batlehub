#!/usr/bin/env python3
"""Wait for a submitted SonarCloud analysis and report its quality gate.

Usage:
    sonar_quality_gate.py [--pull-request N] [--report-task PATH] [--timeout S]

Reads `.scannerwork/report-task.txt` — written by the scanner in the step
before — polls the compute-engine task until the analysis is processed, then
prints a markdown summary of the quality gate to stdout. The gate status
(`PASSED` / `FAILED`) goes to `$GITHUB_OUTPUT` as `status` so the caller can
decide whether to fail the job *after* the summary has been published.

The scanner submits the analysis and returns; it does not wait. Without this,
a job that finished green says nothing about whether the gate went red — which
is how a failing gate stays invisible on a pull request.

`SONAR_TOKEN` is used for authentication when set. Exits non-zero only when the
gate could not be determined at all; a red gate is a successful run of this
script that reports `FAILED`.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

# A rating is reported as the number 1..5. Nothing in the API renders it.
RATINGS = {"1": "A", "2": "B", "3": "C", "4": "D", "5": "E"}

METRIC_NAMES = {
    "new_reliability_rating": "Reliability rating on new code",
    "new_security_rating": "Security rating on new code",
    "new_maintainability_rating": "Maintainability rating on new code",
    "new_security_review_rating": "Security review rating on new code",
    "new_security_hotspots_reviewed": "Security hotspots reviewed on new code",
    "new_coverage": "Coverage on new code",
    "new_duplicated_lines_density": "Duplicated lines on new code",
    "new_violations": "Issues on new code",
    "new_bugs": "Bugs on new code",
    "new_vulnerabilities": "Vulnerabilities on new code",
    "new_code_smells": "Code smells on new code",
    "new_blocker_violations": "Blocker issues on new code",
    "new_critical_violations": "Critical issues on new code",
}

# The gate fails when `actual <comparator> threshold`, so what the reader wants
# stated is the negation: the range that would have passed.
REQUIRED = {"GT": "≤", "LT": "≥", "EQ": "≠", "NE": "="}


def read_report_task(path: Path) -> dict[str, str]:
    """The scanner's `key=value` handoff file."""
    if not path.is_file():
        raise SystemExit(
            f"{path} not found — the scanner step did not run, or it wrote its "
            f"report elsewhere (check the action's projectBaseDir)"
        )
    out: dict[str, str] = {}
    for line in path.read_text().splitlines():
        key, sep, value = line.partition("=")
        if sep:
            out[key.strip()] = value.strip()
    return out


class Sonar:
    def __init__(self, token: str | None) -> None:
        self.token = token

    def get(self, url: str, params: dict[str, str] | None = None) -> dict:
        if params:
            url = f"{url}{'&' if '?' in url else '?'}{urllib.parse.urlencode(params)}"
        request = urllib.request.Request(url)
        if self.token:
            # SonarCloud takes the token as the basic-auth *user*, empty password.
            credentials = base64.b64encode(f"{self.token}:".encode()).decode()
            request.add_header("Authorization", f"Basic {credentials}")
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)


def await_analysis(sonar: Sonar, ce_task_url: str, timeout: float) -> str:
    """Poll the compute-engine task; answer with its analysis id.

    A `PENDING`/`IN_PROGRESS` task means the server has not finished computing
    the gate, and asking for the gate anyway answers about the *previous*
    analysis — the failure mode this loop exists to avoid.
    """
    deadline = time.monotonic() + timeout
    while True:
        task = sonar.get(ce_task_url)["task"]
        status = task["status"]
        if status == "SUCCESS":
            return task["analysisId"]
        if status in ("FAILED", "CANCELED"):
            raise SystemExit(f"the analysis task ended {status}: {ce_task_url}")
        if time.monotonic() >= deadline:
            raise SystemExit(
                f"the analysis was still {status} after {timeout:.0f}s: {ce_task_url}"
            )
        time.sleep(5)


def render_value(metric: str, value: str) -> str:
    if metric.endswith("_rating"):
        return RATINGS.get(value, value)
    if metric.endswith(("_density", "coverage", "reviewed")):
        # Sonar returns the full float; two places is what its own UI shows.
        try:
            return f"{float(value):.1f}%"
        except ValueError:
            return value
    return value


def condition_row(condition: dict) -> str:
    metric = condition["metricKey"]
    name = METRIC_NAMES.get(metric, metric)
    actual = render_value(metric, condition.get("actualValue", "—"))
    required = (
        f"{REQUIRED.get(condition['comparator'], condition['comparator'])} "
        f"{render_value(metric, condition['errorThreshold'])}"
    )
    mark = "❌" if condition["status"] == "ERROR" else "✅"
    return f"| {mark} | {name} | {actual} | {required} |"


def count(sonar: Sonar, url: str, params: dict[str, str]) -> int | None:
    """A total from a search endpoint, or `None` when it cannot be had.

    Decoration must not fail because a count was unavailable — the gate status
    is the part that matters, and it has already been fetched.
    """
    try:
        payload = sonar.get(url, params)
    except (OSError, ValueError, KeyError):  # URLError is an OSError
        return None
    return payload.get("total") or payload.get("paging", {}).get("total")


def build_summary(
    status: str,
    conditions: list[dict],
    dashboard_url: str,
    issues: int | None,
    hotspots: int | None,
) -> str:
    headline = {
        "OK": "✅ **Passed**",
        "ERROR": "❌ **Failed**",
        "NONE": "➖ **No gate configured**",
    }.get(status, f"**{status}**")

    lines = [
        "<!-- sonar-quality-gate -->",
        f"### SonarCloud quality gate — {headline}",
        "",
    ]

    failing = [c for c in conditions if c["status"] == "ERROR"]
    if failing:
        lines += [
            "| | Condition | Actual | Required |",
            "| --- | --- | --- | --- |",
            *(condition_row(c) for c in failing),
            "",
        ]
    elif conditions:
        plural = "" if len(conditions) == 1 else "s"
        lines += [f"All {len(conditions)} condition{plural} met.", ""]

    counted = []
    if issues is not None:
        counted.append(f"**{issues}** open issue{'' if issues == 1 else 's'} on new code")
    if hotspots:
        counted.append(f"**{hotspots}** security hotspot{'' if hotspots == 1 else 's'} to review")
    if counted:
        lines += [" · ".join(counted), ""]

    lines.append(f"[Full report on SonarCloud]({dashboard_url})")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--pull-request",
        default="",
        help="pull request number; empty for a branch analysis",
    )
    ap.add_argument("--report-task", default=".scannerwork/report-task.txt", type=Path)
    ap.add_argument("--timeout", default=600.0, type=float)
    args = ap.parse_args()

    report = read_report_task(args.report_task)
    sonar = Sonar(os.environ.get("SONAR_TOKEN") or None)

    analysis_id = await_analysis(sonar, report["ceTaskUrl"], args.timeout)
    server = report["serverUrl"].rstrip("/")
    project_status = sonar.get(
        f"{server}/api/qualitygates/project_status", {"analysisId": analysis_id}
    )["projectStatus"]

    # `pullRequest` scopes the counts to the same new code the gate measured;
    # without it a branch analysis reports the whole project.
    scope = {"pullRequest": args.pull_request} if args.pull_request else {}
    issues = count(
        sonar,
        f"{server}/api/issues/search",
        {"componentKeys": report["projectKey"], "resolved": "false", "ps": "1", **scope},
    )
    hotspots = count(
        sonar,
        f"{server}/api/hotspots/search",
        {"projectKey": report["projectKey"], "ps": "1", **scope},
    )

    status = project_status["status"]
    print(
        build_summary(
            status,
            project_status.get("conditions", []),
            report.get("dashboardUrl", f"{server}/dashboard?id={report['projectKey']}"),
            issues,
            hotspots,
        ),
        end="",
    )

    github_output = os.environ.get("GITHUB_OUTPUT")
    if github_output:
        with open(github_output, "a", encoding="utf-8") as fh:
            fh.write(f"status={'PASSED' if status == 'OK' else 'FAILED'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
