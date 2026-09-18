#!/usr/bin/env python3
"""One step of the escalation, judged and recorded.

Reads k6's summary export for the step and the runner's `/proc` samples over the
same window, writes one JSON row, and prints a one-line verdict the runner greps
for. Kept out of the shell because the two shapes of k6 summary and the window
arithmetic are exactly the things a `jq` one-liner gets quietly wrong.

A step fails on any of three counts, and they catch different failures:

  errors     the server answered 5xx, or the connection did not complete;
  p95        it answered, slowly enough that no client would wait;
  dropped    k6 could not even place the iterations — the server is refusing the
             offered rate, and the requests it *did* answer can look perfectly
             healthy while it does. This is the one a latency-only check misses.
"""
from __future__ import annotations

import argparse
import csv
import json
import statistics
from pathlib import Path


def field(metrics: dict, metric: str, key: str):
    """k6 writes either `{metric: {key: v}}` or `{metric: {values: {key: v}}}`."""
    m = metrics.get(metric)
    if not isinstance(m, dict):
        return None
    if key in m:
        return m[key]
    values = m.get("values")
    return values.get(key) if isinstance(values, dict) else None


def window(path: Path, lo: int, hi: int) -> dict:
    """What the sampler saw between two epochs.

    The pool is here for the same reason as RSS and CPU: a step that fails on
    "never placed" has a queue somewhere, and the three of them together say
    where. A server at half a core with no free connection is a pool ceiling; a
    server at four cores with the pool idle is a compute ceiling.
    """
    rss: list[float] = []
    cpu: list[float] = []
    avail: list[float] = []
    size: list[float] = []
    side: dict[str, list[float]] = {"pg": [], "redis": [], "s3": []}
    if not path.exists():
        return {}
    for row in csv.DictReader(path.open()):
        try:
            t = int(row["epoch_s"])
        except (KeyError, ValueError):
            continue
        if not (lo <= t <= hi):
            continue
        if row.get("rss_kb"):
            rss.append(float(row["rss_kb"]) / 1024)
        if row.get("cpu_pct"):
            cpu.append(float(row["cpu_pct"]))
        if row.get("pool_avail"):
            avail.append(float(row["pool_avail"]))
        if row.get("pool_size"):
            size.append(float(row["pool_size"]))
        for key, col in (("pg", "pg_rss_kb"), ("redis", "redis_rss_kb"), ("s3", "s3_rss_kb")):
            if row.get(col):
                side[key].append(float(row[col]) / 1024)
    return {
        "rss_peak": max(rss) if rss else None,
        "rss_med": statistics.median(rss) if rss else None,
        "cpu_peak": max(cpu) if cpu else None,
        "cpu_med": statistics.median(cpu) if cpu else None,
        # The *minimum* free, not the median: a pool that touched zero was a
        # queue, even for a second, and the median would hide it.
        "pool_free_min": min(avail) if avail else None,
        "pool_size": max(size) if size else None,
        # Peak, like the server's own RSS, and `None` when the backend was not
        # visible from this machine rather than 0.
        "pg_rss_peak": max(side["pg"]) if side["pg"] else None,
        "redis_rss_peak": max(side["redis"]) if side["redis"] else None,
        "s3_rss_peak": max(side["s3"]) if side["s3"] else None,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rate", type=int, required=True)
    ap.add_argument("--summary", type=Path, required=True)
    ap.add_argument("--samples", type=Path, required=True)
    ap.add_argument("--from", dest="lo", type=int, required=True)
    ap.add_argument("--to", dest="hi", type=int, required=True)
    ap.add_argument("--max-fail-pct", type=float, default=5.0)
    ap.add_argument("--max-p95-ms", type=float, default=5000.0)
    ap.add_argument("--max-drop-pct", type=float, default=5.0)
    ap.add_argument("--append", type=Path)
    ap.add_argument("--died", action="store_true")
    # k6's own exit status. Defaulted so an older caller still works, but
    # `breaking_point.sh` passes it: a non-zero k6 is a step that failed even
    # when it managed to write a plausible-looking summary first.
    ap.add_argument("--k6-exit", dest="k6_exit", type=int, default=0)
    args = ap.parse_args()

    # **A summary that could not be read is a failed step, not a clean one.**
    # Every threshold below is guarded by `is not None`, so an empty `metrics`
    # skipped all of them, left `reasons` empty and printed `OK` — and since
    # `breaking_point.sh` never checks k6's own exit status, a k6 that was
    # OOM-killed at a high rate made the search keep doubling and the report
    # conclude "No knee within the budget". That is the one outcome the
    # escalation is driving toward, so it is the one it must not misread.
    metrics = {}
    no_summary = None
    if not args.summary.exists():
        no_summary = "k6 wrote no summary"
    else:
        try:
            parsed = json.loads(args.summary.read_text())
        except json.JSONDecodeError as e:
            parsed = None
            no_summary = f"k6's summary is not valid JSON ({e.msg})"
        if parsed is not None:
            metrics = (parsed or {}).get("metrics") or {}
            if not metrics:
                no_summary = "k6's summary carries no metrics"

    reqs = field(metrics, "http_reqs", "count")
    achieved = field(metrics, "http_reqs", "rate")
    completed = field(metrics, "iterations", "count") or 0
    dropped = field(metrics, "dropped_iterations", "count") or 0
    # `http_req_failed` is a Rate metric and k6 exports it under "value";
    # "rate" is the newer machine-readable spelling. `record_run.py` reads
    # "value" for the same reason — asking for the wrong key gives None,
    # which prints as "None% errors" and judges nothing.
    err = field(metrics, "http_req_failed", "value")
    if not isinstance(err, (int, float)):
        err = field(metrics, "http_req_failed", "rate")
    err_pct = (err * 100.0) if isinstance(err, (int, float)) else None
    drop_pct = (
        dropped / (dropped + completed) * 100.0 if (dropped + completed) else 0.0
    )

    def ms(key: str):
        v = field(metrics, "http_req_duration", key)
        return round(v, 2) if isinstance(v, (int, float)) else None

    w = window(args.samples, args.lo, args.hi)
    rss_peak, rss_med = w.get("rss_peak"), w.get("rss_med")
    cpu_peak, cpu_med = w.get("cpu_peak"), w.get("cpu_med")
    row = {
        "rate": args.rate,
        "achieved_rps": round(achieved, 1) if isinstance(achieved, (int, float)) else None,
        "requests": reqs,
        "error_pct": round(err_pct, 3) if err_pct is not None else None,
        "dropped_pct": round(drop_pct, 3),
        "min_ms": ms("min"),
        "med_ms": ms("med"),
        "p95_ms": ms("p(95)"),
        "p98_ms": ms("p(98)"),
        "max_ms": ms("max"),
        "rss_peak_mib": round(rss_peak, 1) if rss_peak else None,
        "rss_med_mib": round(rss_med, 1) if rss_med else None,
        "cpu_peak_pct": round(cpu_peak, 1) if cpu_peak else None,
        "cpu_med_pct": round(cpu_med, 1) if cpu_med else None,
        "pool_free_min": w.get("pool_free_min"),
        "pool_size": w.get("pool_size"),
        "pg_rss_peak_mib": round(w["pg_rss_peak"], 1) if w.get("pg_rss_peak") else None,
        "redis_rss_peak_mib": round(w["redis_rss_peak"], 1) if w.get("redis_rss_peak") else None,
        "s3_rss_peak_mib": round(w["s3_rss_peak"], 1) if w.get("s3_rss_peak") else None,
        "died": bool(args.died),
    }

    reasons = []
    if args.died:
        reasons.append("the server process died")
    if no_summary:
        reasons.append(f"{no_summary} — this step measured nothing")
    if args.k6_exit:
        reasons.append(f"k6 exited {args.k6_exit}")
    if err_pct is not None and err_pct > args.max_fail_pct:
        reasons.append(f"{err_pct:.1f}% errors (limit {args.max_fail_pct:.0f}%)")
    if row["p95_ms"] is not None and row["p95_ms"] > args.max_p95_ms:
        reasons.append(f"p95 {row['p95_ms']:.0f} ms (limit {args.max_p95_ms:.0f} ms)")
    if drop_pct > args.max_drop_pct:
        reasons.append(f"{drop_pct:.1f}% of iterations never placed (limit {args.max_drop_pct:.0f}%)")
    row["failed"] = bool(reasons)
    row["reasons"] = reasons

    if args.append:
        with args.append.open("a") as fh:
            fh.write(json.dumps(row) + "\n")

    if reasons:
        print(f"FAILED — {', '.join(reasons)}")
    else:
        print(
            f"OK — offered {args.rate}, served {row['achieved_rps']}/s, "
            f"p95 {row['p95_ms']} ms, p98 {row['p98_ms']} ms, {row['error_pct']}% errors, "
            f"RSS {row['rss_peak_mib']} MiB, CPU {row['cpu_peak_pct']}%, "
            f"pool {row['pool_free_min']}/{row['pool_size']} free at its tightest"
        )


if __name__ == "__main__":
    main()
