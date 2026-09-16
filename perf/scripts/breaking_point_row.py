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


def window(path: Path, lo: int, hi: int) -> tuple[float | None, float | None, float | None, float | None]:
    """Peak and median RSS (MiB) and CPU (%) between two epochs."""
    rss: list[float] = []
    cpu: list[float] = []
    if not path.exists():
        return (None, None, None, None)
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
    return (
        max(rss) if rss else None,
        statistics.median(rss) if rss else None,
        max(cpu) if cpu else None,
        statistics.median(cpu) if cpu else None,
    )


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
    args = ap.parse_args()

    metrics = {}
    if args.summary.exists():
        try:
            metrics = (json.loads(args.summary.read_text()) or {}).get("metrics") or {}
        except json.JSONDecodeError:
            metrics = {}

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

    rss_peak, rss_med, cpu_peak, cpu_med = window(args.samples, args.lo, args.hi)
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
        "died": bool(args.died),
    }

    reasons = []
    if args.died:
        reasons.append("the server process died")
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
            f"RSS {row['rss_peak_mib']} MiB, CPU {row['cpu_peak_pct']}%"
        )


if __name__ == "__main__":
    main()
