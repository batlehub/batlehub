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

Every file is named by `perf_paths` from the run's `--label` and the step's
`--rate`; nothing path-shaped is passed in. See that module for why.
"""
from __future__ import annotations

import argparse
import csv
import json
import statistics
from pathlib import Path

import perf_paths

# The sampler's numeric columns: which list each accumulates into, and what to
# divide by to reach the unit the row reports. Kept as data rather than as one
# `if row.get(...)` per column, because the loop below is then the same three
# lines whatever the sampler learns to record next.
SAMPLED_COLUMNS: dict[str, tuple[str, float]] = {
    "rss_kb": ("rss", 1024.0),
    "cpu_pct": ("cpu", 1.0),
    "pool_avail": ("pool_avail", 1.0),
    "pool_size": ("pool_size", 1.0),
    "pg_rss_kb": ("pg", 1024.0),
    "redis_rss_kb": ("redis", 1024.0),
    "s3_rss_kb": ("s3", 1024.0),
}


def field(metrics: dict, metric: str, key: str):
    """k6 writes either `{metric: {key: v}}` or `{metric: {values: {key: v}}}`."""
    m = metrics.get(metric)
    if not isinstance(m, dict):
        return None
    if key in m:
        return m[key]
    values = m.get("values")
    return values.get(key) if isinstance(values, dict) else None


def within(row: dict[str, str], lo: int, hi: int) -> bool:
    """Whether a sampler row falls in the step's window, epochs inclusive."""
    try:
        return lo <= int(row["epoch_s"]) <= hi
    except (KeyError, ValueError):
        return False


def collect(path: Path, lo: int, hi: int) -> dict[str, list[float]]:
    """Every sampled column over the window, in the units the row reports."""
    got: dict[str, list[float]] = {name: [] for name, _ in SAMPLED_COLUMNS.values()}
    with path.open() as fh:
        for row in csv.DictReader(fh):
            if not within(row, lo, hi):
                continue
            for column, (name, divisor) in SAMPLED_COLUMNS.items():
                if row.get(column):
                    got[name].append(float(row[column]) / divisor)
    return got


def peak(values: list[float]) -> float | None:
    return max(values) if values else None


def mid(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def window(path: Path, lo: int, hi: int) -> dict:
    """What the sampler saw between two epochs.

    The pool is here for the same reason as RSS and CPU: a step that fails on
    "never placed" has a queue somewhere, and the three of them together say
    where. A server at half a core with no free connection is a pool ceiling; a
    server at four cores with the pool idle is a compute ceiling.
    """
    if not path.exists():
        return {}
    got = collect(path, lo, hi)
    return {
        "rss_peak": peak(got["rss"]),
        "rss_med": mid(got["rss"]),
        "cpu_peak": peak(got["cpu"]),
        "cpu_med": mid(got["cpu"]),
        # The *minimum* free, not the median: a pool that touched zero was a
        # queue, even for a second, and the median would hide it.
        "pool_free_min": min(got["pool_avail"]) if got["pool_avail"] else None,
        "pool_size": peak(got["pool_size"]),
        # Peak, like the server's own RSS, and `None` when the backend was not
        # visible from this machine rather than 0.
        "pg_rss_peak": peak(got["pg"]),
        "redis_rss_peak": peak(got["redis"]),
        "s3_rss_peak": peak(got["s3"]),
    }


def read_summary(path: Path) -> tuple[dict, str | None]:
    """k6's metrics for the step, and why there are none when there are none.

    **A summary that could not be read is a failed step, not a clean one.**
    Every threshold below is guarded by `is not None`, so an empty `metrics`
    skipped all of them, left `reasons` empty and printed `OK` — and since
    `breaking_point.sh` never checks k6's own exit status, a k6 that was
    OOM-killed at a high rate made the search keep doubling and the report
    conclude "No knee within the budget". That is the one outcome the
    escalation is driving toward, so it is the one it must not misread.
    """
    if not path.exists():
        return {}, "k6 wrote no summary"
    try:
        parsed = json.loads(path.read_text())
    except json.JSONDecodeError as e:
        return {}, f"k6's summary is not valid JSON ({e.msg})"
    metrics = (parsed or {}).get("metrics") or {}
    return metrics, None if metrics else "k6's summary carries no metrics"


def error_pct(metrics: dict) -> float | None:
    """The failure rate as a percentage, or None when k6 did not report one.

    `http_req_failed` is a Rate metric and k6 exports it under "value"; "rate"
    is the newer machine-readable spelling. `record_run.py` reads "value" for
    the same reason — asking for the wrong key gives None, which prints as
    "None% errors" and judges nothing.
    """
    err = field(metrics, "http_req_failed", "value")
    if not isinstance(err, (int, float)):
        err = field(metrics, "http_req_failed", "rate")
    return err * 100.0 if isinstance(err, (int, float)) else None


def dropped_pct(metrics: dict) -> float:
    """What share of the offered iterations k6 never managed to place."""
    completed = field(metrics, "iterations", "count") or 0
    dropped = field(metrics, "dropped_iterations", "count") or 0
    total = dropped + completed
    return dropped / total * 100.0 if total else 0.0


def duration_ms(metrics: dict, key: str) -> float | None:
    v = field(metrics, "http_req_duration", key)
    return round(v, 2) if isinstance(v, (int, float)) else None


def rounded(value, digits: int = 1):
    return round(value, digits) if value else None


def build_row(args: argparse.Namespace, metrics: dict, w: dict) -> dict:
    """The JSON row for this step: what was offered, served and consumed."""
    achieved = field(metrics, "http_reqs", "rate")
    err_pct = error_pct(metrics)
    return {
        "rate": args.rate,
        "achieved_rps": round(achieved, 1) if isinstance(achieved, (int, float)) else None,
        "requests": field(metrics, "http_reqs", "count"),
        "error_pct": round(err_pct, 3) if err_pct is not None else None,
        "dropped_pct": round(dropped_pct(metrics), 3),
        "min_ms": duration_ms(metrics, "min"),
        "med_ms": duration_ms(metrics, "med"),
        "p95_ms": duration_ms(metrics, "p(95)"),
        "p98_ms": duration_ms(metrics, "p(98)"),
        "max_ms": duration_ms(metrics, "max"),
        "rss_peak_mib": rounded(w.get("rss_peak")),
        "rss_med_mib": rounded(w.get("rss_med")),
        "cpu_peak_pct": rounded(w.get("cpu_peak")),
        "cpu_med_pct": rounded(w.get("cpu_med")),
        "pool_free_min": w.get("pool_free_min"),
        "pool_size": w.get("pool_size"),
        "pg_rss_peak_mib": rounded(w.get("pg_rss_peak")),
        "redis_rss_peak_mib": rounded(w.get("redis_rss_peak")),
        "s3_rss_peak_mib": rounded(w.get("s3_rss_peak")),
        "died": bool(args.died),
    }


def judge(args: argparse.Namespace, row: dict, no_summary: str | None) -> list[str]:
    """Every reason this step failed, in the order a reader wants them."""
    reasons: list[str] = []
    if args.died:
        reasons.append("the server process died")
    if no_summary:
        reasons.append(f"{no_summary} — this step measured nothing")
    if args.k6_exit:
        reasons.append(f"k6 exited {args.k6_exit}")
    if row["error_pct"] is not None and row["error_pct"] > args.max_fail_pct:
        reasons.append(f"{row['error_pct']:.1f}% errors (limit {args.max_fail_pct:.0f}%)")
    if row["p95_ms"] is not None and row["p95_ms"] > args.max_p95_ms:
        reasons.append(f"p95 {row['p95_ms']:.0f} ms (limit {args.max_p95_ms:.0f} ms)")
    if row["dropped_pct"] > args.max_drop_pct:
        reasons.append(
            f"{row['dropped_pct']:.1f}% of iterations never placed "
            f"(limit {args.max_drop_pct:.0f}%)"
        )
    return reasons


def verdict(row: dict) -> str:
    """The one line `breaking_point.sh` greps for."""
    if row["reasons"]:
        return f"FAILED — {', '.join(row['reasons'])}"
    return (
        f"OK — offered {row['rate']}, served {row['achieved_rps']}/s, "
        f"p95 {row['p95_ms']} ms, p98 {row['p98_ms']} ms, {row['error_pct']}% errors, "
        f"RSS {row['rss_peak_mib']} MiB, CPU {row['cpu_peak_pct']}%, "
        f"pool {row['pool_free_min']}/{row['pool_size']} free at its tightest"
    )


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--label", type=perf_paths.label, required=True)
    ap.add_argument("--rate", type=int, required=True)
    ap.add_argument("--from", dest="lo", type=int, required=True)
    ap.add_argument("--to", dest="hi", type=int, required=True)
    ap.add_argument("--max-fail-pct", type=float, default=5.0)
    ap.add_argument("--max-p95-ms", type=float, default=5000.0)
    ap.add_argument("--max-drop-pct", type=float, default=5.0)
    ap.add_argument("--died", action="store_true")
    # k6's own exit status. Defaulted so an older caller still works, but
    # `breaking_point.sh` passes it: a non-zero k6 is a step that failed even
    # when it managed to write a plausible-looking summary first.
    ap.add_argument("--k6-exit", dest="k6_exit", type=int, default=0)
    return ap.parse_args()


def main() -> None:
    args = parse_args()

    metrics, no_summary = read_summary(perf_paths.step_summary_file(args.label, args.rate))
    row = build_row(args, metrics, window(perf_paths.samples_file(args.label), args.lo, args.hi))
    row["reasons"] = judge(args, row, no_summary)
    row["failed"] = bool(row["reasons"])

    rows_file = perf_paths.rows_file(args.label)
    rows_file.parent.mkdir(parents=True, exist_ok=True)
    with rows_file.open("a") as fh:
        fh.write(json.dumps(row) + "\n")

    print(verdict(row))


if __name__ == "__main__":
    main()
