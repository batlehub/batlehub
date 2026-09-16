#!/usr/bin/env python3
"""The escalation, as a table and a sentence.

One row per offered rate, in the order they were offered, and a verdict that
names the knee. Written as Markdown because that is what the workflow comments
and what `perf/README.md` links to, and as JSON beside it because comparing two
backends is a diff of numbers, not of prose.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def fmt(v, digits: int = 0) -> str:
    if v is None:
        return "—"
    if isinstance(v, float):
        return f"{v:.{digits}f}"
    return str(v)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True)
    ap.add_argument("--config", required=True)
    ap.add_argument("--rows", type=Path, required=True)
    ap.add_argument("--report", type=Path, required=True)
    ap.add_argument("--broke-at", default="")
    ap.add_argument("--reason", default="")
    ap.add_argument("--panics", type=int, default=0)
    ap.add_argument("--alive", type=int, default=1)
    ap.add_argument("--budget", type=int, default=1200)
    ap.add_argument("--step", type=int, default=60)
    ap.add_argument("--factor", type=int, default=2)
    ap.add_argument("--cpus", type=int, default=0)
    ap.add_argument("--pool-max", default="")
    args = ap.parse_args()

    rows = [json.loads(l) for l in args.rows.read_text().splitlines() if l.strip()]
    passed = [r for r in rows if not r.get("failed")]
    ceiling = passed[-1] if passed else None

    out: list[str] = []
    out.append(f"<!-- breaking-point:{args.label} -->")
    out.append(f"## Breaking point — `{args.label}`")
    out.append("")
    pool_note = f", database pool {args.pool_max}" if args.pool_max else ""
    cpu_note = f", {args.cpus} cores" if args.cpus else ""
    out.append(
        f"`{args.config}`, steps of {args.step}s, each rate {args.factor}x the last, "
        f"budget {args.budget}s{cpu_note}{pool_note}. The workload is the soak mix: "
        f"every registry kind, every shape of request."
    )
    out.append("")

    if args.panics or not args.alive:
        out.append(
            "**The server did not survive.** That is the one outcome this test "
            "treats as a defect rather than a measurement — see the server log "
            "beside this report."
        )
    elif args.broke_at:
        out.append(
            f"**Knee at {args.broke_at} req/s** — {args.reason}. "
            + (
                f"The last rate it served cleanly was **{ceiling['rate']} req/s** "
                f"({ceiling['achieved_rps']}/s achieved, p95 {fmt(ceiling['p95_ms'])} ms, "
                f"p98 {fmt(ceiling['p98_ms'])} ms)."
                if ceiling
                else "It did not serve any offered rate cleanly."
            )
        )
    else:
        out.append(
            f"**No knee within the budget.** It served every rate up to "
            f"{rows[-1]['rate'] if rows else '—'} req/s inside {args.budget}s; the "
            f"escalation ran out of time, not of headroom."
        )
    # Where the queue was, when there was one.
    #
    # The first run of this test broke at the same rate on all four backends
    # with the server at 58-67% of *one* core, and the report said nothing about
    # that — "knee at 400 req/s" reads as a capacity until someone checks the
    # CPU column. A step that fails on iterations never placed has a queue in
    # front of it; this says whether the queue was the server's.
    broke = next((r for r in rows if r.get("failed")), None)
    if broke is not None and not (args.panics or not args.alive):
        saturated = args.cpus and broke.get("cpu_peak_pct") is not None and (
            broke["cpu_peak_pct"] >= 0.8 * 100 * args.cpus
        )
        # A tenth of the pool, not zero. Measured 2026-09-16: a run whose pool sat
        # at 1 free of 10 from its very first step — the tightest resource on the
        # machine — was reported as "neither exhausted", because the sampler reads
        # once a second and a pool at 90% busy simply never shows a zero. Nine
        # connections of ten in use is a queue whether or not the tenth was caught
        # free at the instant of a sample.
        floor = 0.1 * broke["pool_size"] if broke.get("pool_size") else 0
        starved = broke.get("pool_free_min") is not None and broke["pool_free_min"] <= floor
        bits = []
        if broke.get("cpu_peak_pct") is not None:
            bits.append(
                f"it was using {fmt(broke['cpu_peak_pct'])}% of one core"
                + (f" on a {args.cpus}-core runner" if args.cpus else "")
            )
        if broke.get("pool_free_min") is not None:
            bits.append(
                f"the database pool had {fmt(broke['pool_free_min'])} of "
                f"{fmt(broke['pool_size'])} connections free at its tightest"
            )
        if bits:
            out.append("")
            if starved and not saturated:
                out.append(
                    "> **The database pool was saturated at the knee, and this server's compute was "
                    "not.** At the breaking rate " + " and ".join(bits) + ". A pool with a tenth or "
                    "less of it free is a queue in front of every request that needs the database — "
                    "but *a* queue is not necessarily *the* ceiling, and one run cannot tell the two "
                    "apart: a pool runs out both when it caps the rate and when everything behind it "
                    "has become slow. **Raise `max_connections` and re-run.** If the knee moves, the "
                    "pool was the ceiling; if it does not, the pool was costing latency rather than "
                    "throughput — which is worth knowing on its own, and is what the matrix's "
                    "50-connection arm exists to measure. `config.soak.toml` sizes the pool at 10 "
                    "**on purpose**, small enough that a leaked connection shows up inside one soak, "
                    "so neither number here is a recommendation for a deployment."
                )
            elif saturated:
                out.append(
                    "> **The knee is this server's compute.** At the breaking rate "
                    + " and ".join(bits)
                    + " — the machine was the limit, which is what this test is for."
                )
            else:
                out.append(
                    "> **The knee is not this server's compute.** At the breaking rate "
                    + " and ".join(bits)
                    + ". Neither was exhausted, so the ceiling is somewhere else — and the first "
                    "suspect is this harness: k6, the mock upstream and the database share the "
                    "runner with the server they measure. Read the number as a comparison between "
                    "backends run the same way, never as a capacity."
                )
    out.append("")
    out.append(
        "| offered | served | errors | never placed | min | med | p95 | p98 | max | RSS peak | CPU peak | pool free | |"
    )
    out.append("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :-- |")
    for r in rows:
        mark = "**broke**" if r.get("failed") else "ok"
        pool = (
            f"{fmt(r['pool_free_min'])}/{fmt(r['pool_size'])}"
            if r.get("pool_size") is not None
            else "—"
        )
        out.append(
            f"| {r['rate']}/s | {fmt(r['achieved_rps'], 1)}/s | {fmt(r['error_pct'], 2)}% "
            f"| {fmt(r['dropped_pct'], 2)}% | {fmt(r['min_ms'])} ms | {fmt(r['med_ms'])} ms "
            f"| {fmt(r['p95_ms'])} ms | {fmt(r['p98_ms'])} ms | {fmt(r['max_ms'])} ms "
            f"| {fmt(r['rss_peak_mib'])} MiB | {fmt(r['cpu_peak_pct'])}% | {pool} | {mark} |"
        )
    # What the backends cost, per rate.
    #
    # A table and not a single line at the knee: these move with what the server
    # absorbs — Postgres grows with the connections in flight and the work they
    # carry, the object store with the bytes it serves, Redis with what it is
    # asked to hold — so the *shape* across rates is the point. A backend whose
    # memory climbs faster than the server's is where the next ceiling will be.
    side = [
        ("Postgres", "pg_rss_peak_mib"),
        ("Redis", "redis_rss_peak_mib"),
        ("RustFS", "s3_rss_peak_mib"),
    ]
    present = [(n, k) for n, k in side if any(r.get(k) for r in rows)]
    if present:
        out.append("")
        out.append("**What the backends cost, per rate** — peak resident, summed per process name.")
        out.append("")
        out.append("| offered | " + " | ".join(n for n, _ in present) + " | server, for comparison |")
        out.append("| ---: | " + " | ".join("---:" for _ in present) + " | ---: |")
        for r in rows:
            cells = " | ".join(
                (f"{r[k]:.0f} MiB" if r.get(k) else "—") for _, k in present
            )
            mark = " **(broke)**" if r.get("failed") else ""
            out.append(
                f"| {r['rate']}/s{mark} | {cells} | {fmt(r['rss_peak_mib'])} MiB |"
            )
        out.append("")
        out.append(
            "<sub>A server that looks cheap because a backend is doing the work has moved the cost, "
            "not saved it — which is only visible with both columns side by side.</sub>"
        )
    elif rows:
        out.append("")
        out.append(
            "<sub>The backends' own memory was not sampled: they are not visible in this "
            "machine's process table — a Kubernetes sidecar, or any backend in its own PID "
            "namespace, is out of reach of `ps`. Empty, not zero.</sub>"
        )
    out.append("")
    out.append(
        "<sub>*served* is what k6 actually placed, and it falling behind *offered* is "
        "itself a result: the server is refusing the rate. *never placed* counts the "
        "iterations k6 could not start because every VU was still waiting — a queue "
        "in front of the server, which a latency percentile alone does not show. "
        "CPU is a percentage of one core, so above 100% means more than one. *pool free* is the "
        "fewest database connections available at any sample of the step: zero means every "
        "request that needed one waited.</sub>"
    )
    out.append("")

    args.report.write_text("\n".join(out) + "\n")
    args.report.with_suffix(".json").write_text(
        json.dumps(
            {
                "label": args.label,
                "config": args.config,
                "broke_at": int(args.broke_at) if args.broke_at else None,
                "reason": args.reason or None,
                "survived": bool(args.alive and not args.panics),
                "ceiling": ceiling,
                "steps": rows,
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
