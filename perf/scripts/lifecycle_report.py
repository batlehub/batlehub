#!/usr/bin/env python3
"""The report for `perf/scripts/lifecycle.sh`: what the two ends of a process's
life cost.

Reads the per-iteration samples the script wrote and turns them into one table.
There is no verdict and no exit code to fail on, deliberately: a startup budget
is a property of a *deployment* — a readiness probe's `failureThreshold`, a
rollout's `maxUnavailable`, a `terminationGracePeriodSeconds` — and this
repository does not own any of those numbers. What it owns is the measurement,
and a number that moves is visible in the diff of this report.

Paths are resolved from this file's own location, the same construction
`soak_verdict.py` uses and for the same reason: nothing path-shaped crosses a
process boundary.
"""

from __future__ import annotations

import argparse
import csv
import statistics
from pathlib import Path

RESULTS_DIR = Path(__file__).resolve().parents[1] / "results"
SAMPLES_FILE = RESULTS_DIR / "lifecycle-samples.csv"
REPORT_FILE = RESULTS_DIR / "lifecycle.md"


def read_samples() -> list[dict[str, str]]:
    if not SAMPLES_FILE.exists():
        return []
    with SAMPLES_FILE.open() as fh:
        return list(csv.DictReader(fh))


def column(rows: list[dict[str, str]], phase: str, field: str) -> list[int]:
    return [int(r[field]) for r in rows if r["phase"] == phase and r[field]]


def stat_cells(values: list[int]) -> tuple[str, str, str]:
    """median, min, max — as milliseconds, or em-dashes when nothing ran."""
    if not values:
        return "—", "—", "—"
    return (
        f"{statistics.median(values):.0f}",
        f"{min(values)}",
        f"{max(values)}",
    )


def row(label: str, values: list[int], note: str) -> str:
    median, low, high = stat_cells(values)
    return f"| {label} | {median} | {low} | {high} | {note} |"


def build(args: argparse.Namespace, rows: list[dict[str, str]]) -> list[str]:
    cold_healthy = column(rows, "cold", "healthy_ms")
    warm_healthy = column(rows, "warm", "healthy_ms")
    warm_accept = column(rows, "warm", "accept_ms")
    warm_stop = column(rows, "warm", "stop_ms")
    cold_stop = column(rows, "cold", "stop_ms")
    drain_stop = column(rows, "draining", "stop_ms")

    lines = [
        "<!-- lifecycle-report -->",
        "## Startup and shutdown",
        "",
        f"One cold start, {args.iterations} warm ones, and one shutdown with "
        f"`{args.in_flight}` requests in flight against a "
        f"`{args.upstream_delay_ms}`ms upstream. Milliseconds.",
        "",
        "| | median | min | max | |",
        "| --- | ---: | ---: | ---: | :-- |",
        row(
            "Cold start → healthy",
            cold_healthy,
            "empty database: every migration runs",
        ),
        row(
            "Warm start → healthy",
            warm_healthy,
            "what a restart costs once the schema is there",
        ),
        row(
            "Warm start → port accepts",
            warm_accept,
            "traffic can arrive from here; readiness is the row above",
        ),
        row("Stop (idle)", warm_stop + cold_stop, "SIGTERM → process gone"),
        row(
            "Stop (draining)",
            drain_stop,
            f"SIGTERM → process gone, {args.in_flight} requests mid-flight",
        ),
        "",
    ]

    # The two derived numbers, each of which is a decision someone has to make.
    if cold_healthy and warm_healthy:
        migrations = statistics.median(cold_healthy) - statistics.median(warm_healthy)
        lines.append(
            f"**Migrations cost {migrations:.0f} ms** of the cold start — the "
            "difference between the two rows above, which is the only way to "
            "get that number without parsing it out of a log. A replica joining "
            "a rollout pays it once; a readiness probe has to allow for it."
        )
        lines.append("")
    if warm_accept and warm_healthy:
        gap = statistics.median(warm_healthy) - statistics.median(warm_accept)
        lines.append(
            f"**The port accepts {gap:.0f} ms before `/healthz` answers.** "
            "Anything routing on the port rather than on the probe sends traffic "
            "into that window."
        )
        lines.append("")
    if drain_stop and (warm_stop or cold_stop):
        idle = statistics.median(warm_stop + cold_stop)
        lines.append(
            f"**Draining costs {statistics.median(drain_stop) - idle:.0f} ms more "
            "than an idle stop.** actix stops accepting on the signal and then "
            "waits for what is in flight, so this scales with how long the "
            "*slowest upstream* keeps a request open, not with how much this "
            "process has to tear down. It is the number a "
            "`terminationGracePeriodSeconds` has to cover."
        )
        lines.append("")

    lines.append(
        "<sub>Measured by `perf/scripts/lifecycle.sh`, which starts and stops a "
        "real release binary against `perf/config.soak.toml` — 25 registries, so "
        "the startup includes constructing every registry client. Raw "
        "per-iteration samples are in `lifecycle-samples.csv` beside this "
        "report.</sub>",
    )
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iterations", type=int, default=0)
    parser.add_argument("--in-flight", type=int, default=0)
    parser.add_argument("--upstream-delay-ms", type=int, default=0)
    args = parser.parse_args()

    rows = read_samples()
    if not rows:
        REPORT_FILE.write_text(
            "<!-- lifecycle-report -->\n## Startup and shutdown\n\n"
            "No samples were recorded.\n"
        )
        return 1

    REPORT_FILE.write_text("\n".join(build(args, rows)) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
