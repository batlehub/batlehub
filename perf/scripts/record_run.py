#!/usr/bin/env python3
"""Turn one k6 run into one row of the results table.

`run_with_metrics.sh` samples the server's RSS and CPU while k6 runs, and k6
writes its own end-of-test summary. Both were printed and then thrown away:
the box on the terminal said what the peak RSS was, and the next run scrolled
it off. This appends the two halves — resources and throughput — as a single
JSON object to `perf/results/runs.jsonl`, which is what `perf_report.py` reads.

**Max, not average, is the number that matters here.** A server that sits at
90 MiB and spikes to 1.4 GiB while filtering a channel index needs the 1.4 GiB
written down: that is the figure a memory limit has to clear, and an average
hides it completely. Median is recorded beside it so a single spike can be told
from a level shift.

One line per run, appended, never rewritten — a run is a measurement and a
measurement is not edited. The report picks the latest row per label.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import statistics
import subprocess
from datetime import datetime, timezone
from pathlib import Path


# Where a run's files live, and the only place this script looks.
#
# `run_with_metrics.sh` writes them here under names derived from the run's
# label, and this script derives the same names from its own location —
# `perf/scripts/record_run.py` → `perf/results/`. No path is passed in: the
# arguments are a label and a few numbers, none of which can name a file
# outside this directory.
RESULTS_DIR = Path(__file__).resolve().parents[1] / "results"

# A label is a row name in the results table *and* half of four file names, so
# it is checked as a name rather than trusted as one: letters, digits, dot,
# dash, underscore. `02_warm_read`, `12_conda_zst_filtered` — the labels the
# suite actually uses — pass; anything with a separator in it does not.
LABEL_RE = re.compile(r"^[A-Za-z0-9._-]+$")


def run_label(value: str) -> str:
    """The run's label, refused unless it is a plain name."""
    label = value.strip()
    if not label or label in {".", ".."} or not LABEL_RE.match(label):
        raise argparse.ArgumentTypeError(
            f"{value!r} is not a label: letters, digits, '.', '-' and '_' only"
        )
    return label


def read_samples(path: Path) -> list[float]:
    """Read one float per line, skipping anything that is not one.

    The sampler writes from a shell loop against `/proc`, which can produce a
    blank line when the process exits between two reads of it.
    """
    if not path or not path.exists():
        return []
    out: list[float] = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(float(line))
        except ValueError:
            continue
    return out


def stats(values: list[float], divisor: float = 1.0) -> dict | None:
    if not values:
        return None
    scaled = [v / divisor for v in values]
    return {
        "min": round(min(scaled), 2),
        "median": round(statistics.median(scaled), 2),
        "max": round(max(scaled), 2),
        "samples": len(scaled),
    }


def k6_metrics(path: Path | None) -> dict:
    """Pull the handful of numbers worth comparing out of k6's summary export.

    Two shapes exist: the legacy one (`metrics.http_req_duration["p(95)"]`,
    what `--summary-export` writes today) and the newer machine-readable one
    (`metrics.http_req_duration.values`). Both are read, because the flag that
    switches between them is k6's and not ours.
    """
    if not path or not path.exists():
        return {}
    try:
        doc = json.loads(path.read_text())
    except json.JSONDecodeError:
        return {}
    metrics = doc.get("metrics") or {}

    def field(metric: str, key: str):
        m = metrics.get(metric)
        if not isinstance(m, dict):
            return None
        if key in m:
            return m[key]
        values = m.get("values")
        if isinstance(values, dict):
            return values.get(key)
        return None

    def ms(metric: str, key: str):
        v = field(metric, key)
        return round(v, 2) if isinstance(v, (int, float)) else None

    error_rate = field("http_req_failed", "value")
    checks = field("checks", "value")
    return {
        "requests": field("http_reqs", "count"),
        "rps": round(field("http_reqs", "rate"), 2)
        if isinstance(field("http_reqs", "rate"), (int, float))
        else None,
        "iterations": field("iterations", "count"),
        "p50_ms": ms("http_req_duration", "med"),
        "p95_ms": ms("http_req_duration", "p(95)"),
        "p99_ms": ms("http_req_duration", "p(99)"),
        "max_ms": ms("http_req_duration", "max"),
        "error_rate": round(error_rate, 6)
        if isinstance(error_rate, (int, float))
        else None,
        "checks_rate": round(checks, 6) if isinstance(checks, (int, float)) else None,
        "data_received": field("data_received", "count"),
    }


def git(*args: str) -> str | None:
    try:
        out = subprocess.run(
            ["git", *args], capture_output=True, text=True, timeout=10, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() or None if out.returncode == 0 else None


def host() -> dict:
    mem_total = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                mem_total = round(int(line.split()[1]) / 1024)
                break
    except OSError:
        pass
    return {
        "cpus": os.cpu_count(),
        "mem_total_mib": mem_total,
        "kernel": platform.release(),
        "machine": platform.machine(),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--label", required=True, type=run_label, help="scenario label, e.g. 02_warm_read"
    )
    ap.add_argument("--scenario", default="", help="the k6 script that ran, recorded in the row")
    ap.add_argument("--pid", type=int, default=0, help="the server PID that was sampled")
    ap.add_argument("--k6-exit", type=int, default=0)
    ap.add_argument("--started-at", default="", help="RFC 3339, from the caller")
    args = ap.parse_args()

    # Derived, not given: the sampler's two files, k6's summary and the table
    # itself, all named after the label in the one results directory.
    rss_file = RESULTS_DIR / f".{args.label}.rss"
    cpu_file = RESULTS_DIR / f".{args.label}.cpu"
    summary_file = RESULTS_DIR / f"{args.label}.k6.json"
    out_file = RESULTS_DIR / "runs.jsonl"

    rss = read_samples(rss_file)
    cpu = read_samples(cpu_file)

    record = {
        "label": args.label,
        "scenario": args.scenario,
        "started_at": args.started_at
        or datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "finished_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "k6_exit": args.k6_exit,
        # What the run was against, so two rows are not compared across
        # different servers by accident. Set by the perf tasks.
        "backend": os.environ.get("PERF_BACKEND", "filesystem+memory"),
        "profile": os.environ.get("PERF_PROFILE", "release"),
        "note": os.environ.get("PERF_NOTE", ""),
        "version": os.environ.get("PERF_VERSION", "") or git("describe", "--tags", "--always"),
        "git_sha": git("rev-parse", "HEAD"),
        "git_dirty": bool(git("status", "--porcelain")),
        "host": host(),
        "pid": args.pid or None,
        # kB in /proc/{pid}/status → MiB, the unit the box on the terminal uses.
        "rss_mib": stats(rss, 1024.0),
        "cpu_pct": stats(cpu),
        "k6": k6_metrics(summary_file),
    }

    out_file.parent.mkdir(parents=True, exist_ok=True)
    with out_file.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, separators=(",", ":")) + "\n")
    peak = record["rss_mib"]["max"] if record["rss_mib"] else "?"
    print(f"  [resource-monitor] recorded {args.label} (peak RSS {peak} MiB) → {out_file}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
