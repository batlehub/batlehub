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
import statistics
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path


def measurement_path(value: str) -> Path:
    """Resolve a path given on the command line, or refuse it.

    Everything this script reads or appends to belongs to one of three places:
    the repository being measured (`perf/results/…`), whatever directory the
    run was launched from, and the temporary directory the sampler writes its
    RSS and CPU files into. A path that resolves outside all three is a mistake
    or an injection, never a use — and resolving before comparing is what makes
    one check cover `../../etc/x`, a symlink and an absolute path alike.
    """
    roots = [
        Path(__file__).resolve().parents[2],  # the repository this script lives in
        Path.cwd().resolve(),
        Path(tempfile.gettempdir()).resolve(),
    ]
    candidate = Path(value).expanduser()
    resolved = (candidate if candidate.is_absolute() else Path.cwd() / candidate).resolve()
    if not any(resolved == root or root in resolved.parents for root in roots):
        raise argparse.ArgumentTypeError(
            f"{value!r} resolves outside the repository, the working directory "
            f"and {roots[2]}"
        )
    return resolved


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
    ap.add_argument("--label", required=True, help="scenario label, e.g. 02_warm_read")
    ap.add_argument("--scenario", default="", help="path of the k6 script that ran")
    ap.add_argument("--rss-file", type=measurement_path, help="one RSS sample (kB) per line")
    ap.add_argument("--cpu-file", type=measurement_path, help="one CPU sample (percent) per line")
    ap.add_argument("--summary", type=measurement_path, help="k6 --summary-export JSON")
    ap.add_argument("--pid", type=int, default=0, help="the server PID that was sampled")
    ap.add_argument("--k6-exit", type=int, default=0)
    ap.add_argument("--started-at", default="", help="RFC 3339, from the caller")
    ap.add_argument("--out", type=measurement_path, required=True, help="runs.jsonl to append to")
    args = ap.parse_args()

    rss = read_samples(args.rss_file)
    cpu = read_samples(args.cpu_file)

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
        "k6": k6_metrics(args.summary),
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, separators=(",", ":")) + "\n")
    peak = record["rss_mib"]["max"] if record["rss_mib"] else "?"
    print(f"  [resource-monitor] recorded {args.label} (peak RSS {peak} MiB) → {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
