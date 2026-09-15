#!/usr/bin/env python3
"""The performance report: one table per suite run, and the diff against a release.

Reads the rows `record_run.py` appended to `perf/results/runs.jsonl` and writes
two things that say the same thing for two readers. Every argument here is a
*file name* and every file lives in `perf/results/`: the directory comes from
this script's own location, so nothing a caller passes can address a path
outside it.

* `perf-report.md` — the table a person reads, peak RSS and peak CPU beside the
  throughput numbers, because "how fast" and "at what cost" are one question.
* `perf-report.json` — the same numbers, for the next release to compare
  against. This is the artifact attached to a GitHub release
  (`.github/workflows/perf-report.yaml`), and the one `--baseline` reads.

**Why a release-bound report rather than a CI gate.** These numbers are
machine-shaped: the same code on a shared runner and on a workstation differs by
more than any regression worth catching, so a threshold that holds on one lies
on the other. What *is* comparable is release to release on the same runner
class, which is exactly what `--baseline` does — and it says so out loud when
the two runs did not come from the same kind of machine.

A comparison is a verdict only when asked (`--fail-on-regression`); by default
the diff is printed and the exit code stays 0, because a report that goes red on
a noisy runner stops being read.
"""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "batlehub.perf-report/1"


# Every file this script reads or writes lives here, and the directory is
# derived from the script's own location rather than from anything a caller
# says: `perf/scripts/perf_report.py` → `perf/results/`.
RESULTS_DIR = Path(__file__).resolve().parents[1] / "results"


def results_file(value: str) -> Path:
    """A *name* in `perf/results/`, never a path.

    The arguments of this script used to be paths, guarded by a resolve-and-
    confine check. The check held — `--out /etc/cron.d/pwn` was refused — but
    it left a tainted value flowing into `read_text`/`write_text`, which is
    both what a scanner reports and a shape that survives only as long as the
    next person keeps the guard. There is nothing to guard here instead: every
    one of these files belongs in one directory, so the argument is the file's
    name and the directory is a constant.

    A separator is refused rather than stripped. `--baseline ../x.json` from a
    caller that means it is a caller to correct, and silently reading
    `results/x.json` instead would be the wrong kind of helpful.
    """
    name = value.strip()
    if not name or name in {".", ".."}:
        raise argparse.ArgumentTypeError(f"{value!r} is not a file name")
    if name != Path(name).name:
        raise argparse.ArgumentTypeError(
            f"{value!r} is a path; this argument takes a file name, and the file "
            f"lives in {RESULTS_DIR}"
        )
    return RESULTS_DIR / name


# Ordering for the table: the suite's own order, then anything unknown, sorted.
# A row is named by its `--label`, which defaults to the scenario's file name
# without `.js` — so most of these are file names, and the conda ones are the
# four arms one file is run as, each its own row because peak RSS is the result.
KNOWN_ORDER = [
    "01_at_rest",
    "02_warm_read",
    "03_cache_miss",
    "04_upload",
    "05_mixed",
    "06_sbom",
    "07_eviction",
    "08_whole_registry_documents",
    "09_authorize_resolution",
    "12_conda_plain_unfiltered",
    "12_conda_plain_filtered",
    "12_conda_zst_unfiltered",
    "12_conda_zst_filtered",
]


def load_runs(path: Path, *, keep_all: bool) -> list[dict]:
    if not path.exists():
        raise SystemExit(f"no runs recorded yet: {path} does not exist (run a perf scenario first)")
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    if not rows:
        raise SystemExit(f"{path} holds no readable rows")
    if keep_all:
        return rows
    # Latest row per label wins: re-running one scenario replaces its line in
    # the table without invalidating the rest of the suite.
    latest: dict[str, dict] = {}
    for row in rows:
        latest[row.get("label", "?")] = row
    return list(latest.values())


def order_key(label: str) -> tuple[int, str]:
    return (KNOWN_ORDER.index(label), "") if label in KNOWN_ORDER else (len(KNOWN_ORDER), label)


def fmt(value, digits: int = 1, suffix: str = "") -> str:
    if value is None:
        return "—"
    if isinstance(value, float):
        return f"{value:,.{digits}f}{suffix}"
    return f"{value:,}{suffix}"


def pct(value) -> str:
    return "—" if value is None else f"{value * 100:.2f} %"


def delta(now, before, *, lower_is_better: bool) -> tuple[float | None, str]:
    """Percent change and a marker, or (None, '—') when either side is missing."""
    if now is None or before in (None, 0):
        return None, "—"
    change = (now - before) / before * 100.0
    if abs(change) < 2.0:  # below the noise floor of a single run
        return change, f"{change:+.1f} %"
    worse = change > 0 if lower_is_better else change < 0
    return change, f"{change:+.1f} % {'🔻' if worse else '🔹'}"


def build(rows: list[dict]) -> dict:
    scenarios = {}
    for row in sorted(rows, key=lambda r: order_key(r.get("label", "?"))):
        scenarios[row.get("label", "?")] = {
            "scenario": row.get("scenario"),
            "finished_at": row.get("finished_at"),
            "k6_exit": row.get("k6_exit"),
            "rss_mib": row.get("rss_mib"),
            "cpu_pct": row.get("cpu_pct"),
            "k6": row.get("k6") or {},
            "note": row.get("note") or "",
        }
    first = rows[0] if rows else {}
    return {
        "schema": SCHEMA,
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "version": first.get("version"),
        "git_sha": first.get("git_sha"),
        "git_dirty": first.get("git_dirty"),
        "backend": first.get("backend"),
        "profile": first.get("profile"),
        "host": first.get("host") or {},
        "scenarios": scenarios,
    }


def comparable(a: dict, b: dict) -> str | None:
    """Why two reports should not be compared, or None when they should."""
    ha, hb = a.get("host") or {}, b.get("host") or {}
    if ha.get("cpus") and hb.get("cpus") and ha["cpus"] != hb["cpus"]:
        return f"different CPU counts ({hb['cpus']} → {ha['cpus']})"
    if ha.get("machine") and hb.get("machine") and ha["machine"] != hb["machine"]:
        return f"different architectures ({hb['machine']} → {ha['machine']})"
    if a.get("backend") != b.get("backend"):
        return f"different storage backends ({b.get('backend')} → {a.get('backend')})"
    return None


COLUMNS = [
    "Scenario",
    "req/s",
    "p95 (ms)",
    "p99 (ms)",
    "errors",
    "RSS max (MiB)",
    "RSS med (MiB)",
    "CPU max (%)",
    "CPU med (%)",
]


def preamble(report: dict, baseline: dict | None) -> list[str]:
    """The title, the line saying what was measured, and the baseline caveat."""
    host = report.get("host") or {}
    dirty = " (working tree dirty)" if report.get("git_dirty") else ""
    out = [
        f"# Performance report — {report.get('version') or 'unreleased'}",
        "",
        f"`{(report.get('git_sha') or '?')[:12]}`{dirty} · {report.get('backend')} · "
        f"{report.get('profile')} profile · {host.get('cpus')} CPU · "
        f"{fmt(host.get('mem_total_mib'), 0)} MiB RAM · {host.get('machine')} · "
        f"{report.get('generated_at')}",
        "",
    ]
    if not baseline:
        return out
    out.append(
        f"Compared against **{baseline.get('version') or 'baseline'}** "
        f"(`{(baseline.get('git_sha') or '?')[:12]}`)."
    )
    why = comparable(report, baseline)
    if why:
        out.append("")
        out.append(
            f"> ⚠️ **Not a like-for-like comparison** — {why}. The deltas below are "
            "printed because they are still the best available signal, but a number "
            "that moved may be the machine and not the code."
        )
    out.append("")
    return out


def measured_cells(scenario: dict) -> list[str]:
    """The nine columns every row has, baseline or not."""
    k6 = scenario.get("k6") or {}
    rss = scenario.get("rss_mib") or {}
    cpu = scenario.get("cpu_pct") or {}
    return [
        fmt(k6.get("rps")),
        fmt(k6.get("p95_ms")),
        fmt(k6.get("p99_ms")),
        pct(k6.get("error_rate")),
        fmt(rss.get("max")),
        fmt(rss.get("median")),
        fmt(cpu.get("max")),
        fmt(cpu.get("median")),
    ]


def delta_cells(
    label: str, scenario: dict, old: dict
) -> tuple[list[str], list[tuple[str, str, float]]]:
    """The two Δ columns, and whichever of them moved the wrong way."""
    k6, old_k6 = scenario.get("k6") or {}, old.get("k6") or {}
    rss, old_rss = scenario.get("rss_mib") or {}, old.get("rss_mib") or {}
    dp95, dp95_s = delta(k6.get("p95_ms"), old_k6.get("p95_ms"), lower_is_better=True)
    drss, drss_s = delta(rss.get("max"), old_rss.get("max"), lower_is_better=True)
    regressions = [
        (label, what, d)
        for what, d in (("p95", dp95), ("peak RSS", drss))
        if d is not None and d > 0
    ]
    return [dp95_s, drss_s], regressions


def table(report: dict, baseline: dict | None) -> tuple[list[str], list[tuple[str, str, float]]]:
    """The table itself, and the regressions its Δ columns found."""
    header = COLUMNS + (["Δ p95", "Δ RSS max"] if baseline else [])
    out = [
        "| " + " | ".join(header) + " |",
        "|" + "|".join(["---"] * len(header)) + "|",
    ]
    regressions: list[tuple[str, str, float]] = []
    for label, scenario in report["scenarios"].items():
        row = [f"`{label}`", *measured_cells(scenario)]
        if baseline:
            old = (baseline.get("scenarios") or {}).get(label) or {}
            cells, found = delta_cells(label, scenario, old)
            row += cells
            regressions += found
        out.append("| " + " | ".join(row) + " |")
    return out, regressions


def footnotes(report: dict) -> list[str]:
    """What the numbers above do and do not say."""
    out = [""]
    failed = [l for l, s in report["scenarios"].items() if s.get("k6_exit") not in (0, None)]
    if failed:
        out.append(
            "> ⚠️ k6 exited non-zero for "
            + ", ".join(f"`{l}`" for l in failed)
            + " — a threshold was crossed or the run errored. The numbers above are "
            "still what was measured; read them with that in mind."
        )
        out.append("")
    out.append(
        "*Peak RSS is the number a memory limit has to clear; the median beside it is "
        "what the process sits at. CPU is instantaneous (`/proc` tick deltas), not a "
        "lifetime average. Both are the whole server process, so a scenario that runs "
        "two k6 scenarios at once reports their combined cost.*"
    )
    out.append("")
    return out


def render(report: dict, baseline: dict | None) -> tuple[str, list[tuple[str, str, float]]]:
    """Markdown, plus the list of regressions worth failing on."""
    rows, regressions = table(report, baseline)
    out = preamble(report, baseline) + rows + footnotes(report)
    return "\n".join(out), regressions


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--runs",
        type=results_file,
        default="runs.jsonl",
        help="the recorded runs, a file name in perf/results/",
    )
    ap.add_argument(
        "--out",
        type=results_file,
        default="perf-report.md",
        help="the markdown report to write, a file name in perf/results/",
    )
    ap.add_argument(
        "--json",
        type=results_file,
        default="perf-report.json",
        help="the JSON report to write, a file name in perf/results/",
    )
    ap.add_argument(
        "--baseline",
        type=results_file,
        help="a previous perf-report.json to diff against (e.g. the last release's) — "
        "a file name, placed in perf/results/ first",
    )
    ap.add_argument("--all", action="store_true", help="keep every row, not the latest per scenario")
    ap.add_argument(
        "--fail-on-regression",
        action="store_true",
        help="exit 1 when a scenario is slower or heavier than the baseline by more than the margin",
    )
    ap.add_argument("--margin-pct", type=float, default=25.0, help="regression margin (default 25%%)")
    args = ap.parse_args()

    report = build(load_runs(args.runs, keep_all=args.all))
    baseline = None
    if args.baseline:
        if not args.baseline.exists():
            print(f"baseline {args.baseline} not found — reporting without a comparison")
        else:
            baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
            if baseline.get("schema") != SCHEMA:
                print(
                    f"baseline schema is {baseline.get('schema')!r}, not {SCHEMA!r} — "
                    "comparing anyway, fields it lacks read as '—'"
                )

    markdown, regressions = render(report, baseline)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(markdown, encoding="utf-8")
    args.json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(markdown)
    print(f"→ {args.out}\n→ {args.json}")

    over = [(l, what, d) for (l, what, d) in regressions if d > args.margin_pct]
    if over:
        print()
        print(f"Regressions past {args.margin_pct:.0f} %:")
        for label, what, d in over:
            print(f"  {label}: {what} +{d:.1f} %")
        if args.fail_on_regression:
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
