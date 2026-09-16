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
    args = ap.parse_args()

    rows = [json.loads(l) for l in args.rows.read_text().splitlines() if l.strip()]
    passed = [r for r in rows if not r.get("failed")]
    ceiling = passed[-1] if passed else None

    out: list[str] = []
    out.append(f"<!-- breaking-point:{args.label} -->")
    out.append(f"## Breaking point — `{args.label}`")
    out.append("")
    out.append(
        f"`{args.config}`, steps of {args.step}s, each rate {args.factor}x the last, "
        f"budget {args.budget}s. The workload is the soak mix: every registry kind, "
        f"every shape of request."
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
    out.append("")
    out.append(
        "| offered | served | errors | never placed | min | med | p95 | p98 | max | RSS peak | CPU peak | |"
    )
    out.append("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :-- |")
    for r in rows:
        mark = "**broke**" if r.get("failed") else "ok"
        out.append(
            f"| {r['rate']}/s | {fmt(r['achieved_rps'], 1)}/s | {fmt(r['error_pct'], 2)}% "
            f"| {fmt(r['dropped_pct'], 2)}% | {fmt(r['min_ms'])} ms | {fmt(r['med_ms'])} ms "
            f"| {fmt(r['p95_ms'])} ms | {fmt(r['p98_ms'])} ms | {fmt(r['max_ms'])} ms "
            f"| {fmt(r['rss_peak_mib'])} MiB | {fmt(r['cpu_peak_pct'])}% | {mark} |"
        )
    out.append("")
    out.append(
        "<sub>*served* is what k6 actually placed, and it falling behind *offered* is "
        "itself a result: the server is refusing the rate. *never placed* counts the "
        "iterations k6 could not start because every VU was still waiting — a queue "
        "in front of the server, which a latency percentile alone does not show. "
        "CPU is a percentage of one core, so above 100% means more than one.</sub>"
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
