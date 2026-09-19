#!/usr/bin/env python3
"""The escalation, as a table and a sentence.

One row per offered rate, in the order they were offered, and a verdict that
names the knee. Written as Markdown because that is what the workflow comments
and what `perf/README.md` links to, and as JSON beside it because comparing two
backends is a diff of numbers, not of prose.

The report and the rows it reads are named by `perf_paths` from the run's
`--label`; nothing path-shaped is passed in. See that module for why.
"""
from __future__ import annotations

import argparse
import json

import perf_paths

# The clauses of the queue note are joined into one sentence, and the reader
# should see the same conjunction whichever branch wrote it.
AND = " and "

STEP_HEADER = (
    "| offered | served | errors | never placed | min | med | p95 | p98 | max "
    "| RSS peak | CPU peak | pool free | |"
)
STEP_RULE = "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :-- |"

# The backends whose resident memory the sampler can see, and the row key each
# one lands in.
SIDE_BACKENDS = [
    ("Postgres", "pg_rss_peak_mib"),
    ("Redis", "redis_rss_peak_mib"),
    ("RustFS", "s3_rss_peak_mib"),
]


def fmt(v, digits: int = 0) -> str:
    if v is None:
        return "—"
    if isinstance(v, float):
        return f"{v:.{digits}f}"
    return str(v)


def preamble(args: argparse.Namespace) -> list[str]:
    """The heading and the line saying how the escalation was run."""
    pool_note = f", database pool {args.pool_max}" if args.pool_max else ""
    cpu_note = f", {args.cpus} cores" if args.cpus else ""
    return [
        f"<!-- breaking-point:{args.label} -->",
        f"## Breaking point — `{args.label}`",
        "",
        f"`{args.config}`, steps of {args.step}s, each rate {args.factor}x the last, "
        f"budget {args.budget}s{cpu_note}{pool_note}. The workload is the soak mix: "
        f"every registry kind, every shape of request.",
        "",
    ]


def survived(args: argparse.Namespace) -> bool:
    """Whether the server was still running, unpanicked, at the end."""
    return bool(args.alive) and not args.panics


def ceiling_sentence(ceiling: dict | None) -> str:
    """The last rate served cleanly, or that there was none."""
    if not ceiling:
        return "It did not serve any offered rate cleanly."
    return (
        f"The last rate it served cleanly was **{ceiling['rate']} req/s** "
        f"({ceiling['achieved_rps']}/s achieved, p95 {fmt(ceiling['p95_ms'])} ms, "
        f"p98 {fmt(ceiling['p98_ms'])} ms)."
    )


def outcome(args: argparse.Namespace, rows: list[dict], ceiling: dict | None) -> str:
    """The one paragraph that says what happened."""
    if not survived(args):
        return (
            "**The server did not survive.** That is the one outcome this test "
            "treats as a defect rather than a measurement — see the server log "
            "beside this report."
        )
    if args.broke_at:
        return f"**Knee at {args.broke_at} req/s** — {args.reason}. " + ceiling_sentence(ceiling)
    return (
        f"**No knee within the budget.** It served every rate up to "
        f"{rows[-1]['rate'] if rows else '—'} req/s inside {args.budget}s; the "
        f"escalation ran out of time, not of headroom."
    )


def queue_clauses(broke: dict, cpus: int) -> list[str]:
    """What the server and the pool were doing at the breaking rate."""
    bits = []
    if broke.get("cpu_peak_pct") is not None:
        bits.append(
            f"it was using {fmt(broke['cpu_peak_pct'])}% of one core"
            + (f" on a {cpus}-core runner" if cpus else "")
        )
    if broke.get("pool_free_min") is not None:
        bits.append(
            f"the database pool had {fmt(broke['pool_free_min'])} of "
            f"{fmt(broke['pool_size'])} connections free at its tightest"
        )
    return bits


def pool_starved(broke: dict) -> bool:
    """Whether the database pool was a queue at the knee.

    A tenth of the pool, not zero. Measured 2026-09-16: a run whose pool sat at
    1 free of 10 from its very first step — the tightest resource on the
    machine — was reported as "neither exhausted", because the sampler reads
    once a second and a pool at 90% busy simply never shows a zero. Nine
    connections of ten in use is a queue whether or not the tenth was caught
    free at the instant of a sample.
    """
    floor = 0.1 * broke["pool_size"] if broke.get("pool_size") else 0
    return broke.get("pool_free_min") is not None and broke["pool_free_min"] <= floor


def cpu_saturated(broke: dict, cpus: int) -> bool:
    """Whether the server was at 80% or more of every core it had."""
    if not cpus or broke.get("cpu_peak_pct") is None:
        return False
    return broke["cpu_peak_pct"] >= 0.8 * 100 * cpus


def pool_note(bits: list[str]) -> str:
    return (
        "> **The database pool was saturated at the knee, and this server's compute was "
        "not.** At the breaking rate " + AND.join(bits) + ". A pool with a tenth or "
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


def compute_note(bits: list[str]) -> str:
    return (
        "> **The knee is this server's compute.** At the breaking rate "
        + AND.join(bits)
        + " — the machine was the limit, which is what this test is for."
    )


def elsewhere_note(bits: list[str]) -> str:
    return (
        "> **The knee is not this server's compute.** At the breaking rate "
        + AND.join(bits)
        + ". Neither was exhausted, so the ceiling is somewhere else — and the first "
        "suspect is this harness: k6, the mock upstream and the database share the "
        "runner with the server they measure. Read the number as a comparison between "
        "backends run the same way, never as a capacity."
    )


def queue_section(args: argparse.Namespace, rows: list[dict]) -> list[str]:
    """Where the queue was, when there was one.

    The first run of this test broke at the same rate on all four backends with
    the server at 58-67% of *one* core, and the report said nothing about that
    — "knee at 400 req/s" reads as a capacity until someone checks the CPU
    column. A step that fails on iterations never placed has a queue in front
    of it; this says whether the queue was the server's.
    """
    broke = next((r for r in rows if r.get("failed")), None)
    if broke is None or not survived(args):
        return []
    bits = queue_clauses(broke, args.cpus)
    if not bits:
        return []
    starved = pool_starved(broke)
    saturated = cpu_saturated(broke, args.cpus)
    if starved and not saturated:
        note = pool_note(bits)
    elif saturated:
        note = compute_note(bits)
    else:
        note = elsewhere_note(bits)
    return ["", note]


def step_row(r: dict) -> str:
    mark = "**broke**" if r.get("failed") else "ok"
    pool = (
        f"{fmt(r['pool_free_min'])}/{fmt(r['pool_size'])}"
        if r.get("pool_size") is not None
        else "—"
    )
    return (
        f"| {r['rate']}/s | {fmt(r['achieved_rps'], 1)}/s | {fmt(r['error_pct'], 2)}% "
        f"| {fmt(r['dropped_pct'], 2)}% | {fmt(r['min_ms'])} ms | {fmt(r['med_ms'])} ms "
        f"| {fmt(r['p95_ms'])} ms | {fmt(r['p98_ms'])} ms | {fmt(r['max_ms'])} ms "
        f"| {fmt(r['rss_peak_mib'])} MiB | {fmt(r['cpu_peak_pct'])}% | {pool} | {mark} |"
    )


def steps_table(rows: list[dict]) -> list[str]:
    return ["", STEP_HEADER, STEP_RULE, *(step_row(r) for r in rows)]


def backends_table(rows: list[dict]) -> list[str]:
    """What the backends cost, per rate.

    A table and not a single line at the knee: these move with what the server
    absorbs — Postgres grows with the connections in flight and the work they
    carry, the object store with the bytes it serves, Redis with what it is
    asked to hold — so the *shape* across rates is the point. A backend whose
    memory climbs faster than the server's is where the next ceiling will be.
    """
    present = [(n, k) for n, k in SIDE_BACKENDS if any(r.get(k) for r in rows)]
    if not present:
        if not rows:
            return []
        return [
            "",
            "<sub>The backends' own memory was not sampled: they are not visible in this "
            "machine's process table — a Kubernetes sidecar, or any backend in its own PID "
            "namespace, is out of reach of `ps`. Empty, not zero.</sub>",
        ]

    out = [
        "",
        "**What the backends cost, per rate** — peak resident, summed per process name.",
        "",
        "| offered | " + " | ".join(n for n, _ in present) + " | server, for comparison |",
        "| ---: | " + " | ".join("---:" for _ in present) + " | ---: |",
    ]
    for r in rows:
        cells = " | ".join((f"{r[k]:.0f} MiB" if r.get(k) else "—") for _, k in present)
        mark = " **(broke)**" if r.get("failed") else ""
        out.append(f"| {r['rate']}/s{mark} | {cells} | {fmt(r['rss_peak_mib'])} MiB |")
    out.append("")
    out.append(
        "<sub>A server that looks cheap because a backend is doing the work has moved the cost, "
        "not saved it — which is only visible with both columns side by side.</sub>"
    )
    return out


def footnote() -> list[str]:
    return [
        "",
        "<sub>*served* is what k6 actually placed, and it falling behind *offered* is "
        "itself a result: the server is refusing the rate. *never placed* counts the "
        "iterations k6 could not start because every VU was still waiting — a queue "
        "in front of the server, which a latency percentile alone does not show. "
        "CPU is a percentage of one core, so above 100% means more than one. *pool free* is the "
        "fewest database connections available at any sample of the step: zero means every "
        "request that needed one waited.</sub>",
        "",
    ]


def render(args: argparse.Namespace, rows: list[dict], ceiling: dict | None) -> str:
    lines = [
        *preamble(args),
        outcome(args, rows, ceiling),
        *queue_section(args, rows),
        *steps_table(rows),
        *backends_table(rows),
        *footnote(),
    ]
    return "\n".join(lines) + "\n"


def summary_json(args: argparse.Namespace, rows: list[dict], ceiling: dict | None) -> str:
    return (
        json.dumps(
            {
                "label": args.label,
                "config": args.config,
                "broke_at": int(args.broke_at) if args.broke_at else None,
                "reason": args.reason or None,
                "survived": survived(args),
                "ceiling": ceiling,
                "steps": rows,
            },
            indent=2,
        )
        + "\n"
    )


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--label", type=perf_paths.label, required=True)
    ap.add_argument("--config", required=True)
    ap.add_argument("--broke-at", default="")
    ap.add_argument("--reason", default="")
    ap.add_argument("--panics", type=int, default=0)
    ap.add_argument("--alive", type=int, default=1)
    ap.add_argument("--budget", type=int, default=1200)
    ap.add_argument("--step", type=int, default=60)
    ap.add_argument("--factor", type=int, default=2)
    ap.add_argument("--cpus", type=int, default=0)
    ap.add_argument("--pool-max", default="")
    return ap.parse_args()


def main() -> None:
    args = parse_args()

    rows_file = perf_paths.rows_file(args.label)
    rows = [
        json.loads(line)
        for line in rows_file.read_text().splitlines()
        if line.strip()
    ]
    passed = [r for r in rows if not r.get("failed")]
    ceiling = passed[-1] if passed else None

    report = perf_paths.report_file(args.label)
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(render(args, rows, ceiling))
    report.with_suffix(".json").write_text(summary_json(args, rows, ceiling))


if __name__ == "__main__":
    main()
