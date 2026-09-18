#!/usr/bin/env python3
"""The arms, side by side — which is the thing the matrix is for.

The per-arm reports each answer "where does *this* backend give out"; the
question a reader actually brings to a matrix is "which one gives out first, and
what does it cost". That comparison was missing from the first two runs: the
comment concatenated five sections and left the diffing to the reader, who then
has to hold ten numbers in their head to notice that a 50-connection pool cuts
p95 by a factor of eleven at the same rate.

Reads the `breaking-point-*.json` each arm writes and emits one table.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def fmt(v, digits: int = 0, unit: str = "") -> str:
    if v is None:
        return "—"
    return f"{v:.{digits}f}{unit}" if isinstance(v, float) else f"{v}{unit}"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="+", type=Path)
    args = ap.parse_args()

    runs = []
    for f in sorted(args.files):
        try:
            runs.append(json.loads(f.read_text()))
        except (OSError, json.JSONDecodeError):
            continue
    if not runs:
        return

    print("## The arms, side by side")
    print()
    print(
        "*Clean* is the last rate every one of the three conditions passed at; the latency columns "
        "are that rate's, so they compare like with like. A knee that is the same everywhere is "
        "itself a result — it says the limit belongs to something all the arms share."
    )
    print()
    print(
        "| arm | knee | clean at | p95 | p98 | server RSS | CPU peak | pool free | Postgres |"
    )
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for r in runs:
        c = r.get("ceiling") or {}
        broke = next((s for s in r.get("steps", []) if s.get("failed")), {})
        knee = f"{r['broke_at']}/s" if r.get("broke_at") else "none in budget"
        if not r.get("survived"):
            knee = "**died**"
        pool = (
            f"{fmt(broke.get('pool_free_min'))}/{fmt(broke.get('pool_size'))}"
            if broke.get("pool_size") is not None
            else "—"
        )
        print(
            f"| `{r['label']}` | {knee} | {fmt(c.get('rate'))}/s | {fmt(c.get('p95_ms'))} ms "
            f"| {fmt(c.get('p98_ms'))} ms | {fmt(c.get('rss_peak_mib'))} MiB "
            f"| {fmt(broke.get('cpu_peak_pct'))}% | {pool} "
            f"| {fmt(broke.get('pg_rss_peak_mib'))} MiB |"
        )
    print()

    # The comparison that is easy to miss and expensive to learn twice.
    # **Every arm, not every arm that broke.** Filtering the arms without a
    # `broke_at` out of the *set* rather than out of the *conclusion* made this
    # fire whenever exactly one arm broke — one knee in the set, more than one
    # run — and announce a shared ceiling nothing had been shown to share.
    knees = {r.get("broke_at") for r in runs if r.get("broke_at")}
    all_broke = all(r.get("broke_at") for r in runs)
    if all_broke and len(knees) == 1 and len(runs) > 1:
        print(
            f"> **Every arm broke at the same rate ({knees.pop()} req/s).** Backends that differ in "
            "storage, cache and pool size do not usually share a ceiling — when they do, the ceiling "
            "is something they all share too. On this harness that is the runner itself: k6, the "
            "mock upstream and the database sit beside the server they measure. The per-arm CPU "
            "column says how much of the machine the server was actually using when it gave out."
        )
        print()


if __name__ == "__main__":
    main()
