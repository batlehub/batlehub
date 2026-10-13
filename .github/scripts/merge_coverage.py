#!/usr/bin/env python3
"""Merge per-job LCOV files and emit a markdown coverage table.

Usage:
    merge_coverage.py [--output merged.lcov] NAME=PATH [NAME=PATH ...]

Each NAME=PATH pair is one test category (e.g. `unit=coverage-unit.lcov`).
The markdown report goes to stdout (append it to $GITHUB_STEP_SUMMARY); the
merged LCOV is written to --output. A missing input file yields an "absent"
row instead of failing, so a partially-run pipeline still reports.

Merging rule: per (file, line) and per (file, function), execution counts are
summed across categories — the standard LCOV aggregation.
"""

from __future__ import annotations

import argparse
import sys
from collections import defaultdict
from pathlib import Path

# file -> line -> count
Lines = dict[str, dict[int, int]]
# file -> function name -> (line, count)
Funcs = dict[str, dict[str, tuple[int, int]]]


def _int_pair(body: str) -> tuple[int, str] | None:
    """`<int>,<rest>` → `(int, rest)`, or `None` when the record is malformed."""
    head, sep, rest = body.partition(",")
    try:
        return (int(head), rest) if sep else None
    except ValueError:
        return None


def parse_lcov(path: Path) -> tuple[Lines, Funcs]:
    lines: Lines = defaultdict(dict)
    funcs: Funcs = defaultdict(dict)
    current: str | None = None
    fn_lines: dict[str, int] = {}
    for raw in path.read_text(errors="replace").splitlines():
        raw = raw.strip()
        if raw.startswith("SF:"):
            current = raw[3:]
            fn_lines = {}
        elif raw == "end_of_record":
            current = None
        elif current is not None:
            _record(raw, current, lines, funcs, fn_lines)
    return lines, funcs


def _record(raw: str, current: str, lines: Lines, funcs: Funcs, fn_lines: dict[str, int]) -> None:
    """Fold one record of the current `SF:` section into its file's tallies."""
    if raw.startswith("DA:"):
        pair = _int_pair(raw[3:])
        if pair is None:
            return
        try:
            count = int(pair[1].split(",")[0])
        except ValueError:
            return
        lines[current][pair[0]] = lines[current].get(pair[0], 0) + count
    elif raw.startswith("FN:"):
        # FN:<line>,<name>  (llvm-cov emits one per function)
        parts = raw[3:].split(",", 1)
        if len(parts) == 2 and parts[0].isdigit():
            fn_lines[parts[1]] = int(parts[0])
    elif raw.startswith("FNDA:"):
        pair = _int_pair(raw[5:])
        if pair is None:
            return
        count, name = pair
        prev = funcs[current].get(name, (fn_lines.get(name, 0), 0))
        funcs[current][name] = (prev[0] or fn_lines.get(name, 0), prev[1] + count)


def merge_into(dst_lines: Lines, dst_funcs: Funcs, src_lines: Lines, src_funcs: Funcs) -> None:
    for f, per_line in src_lines.items():
        for line, count in per_line.items():
            dst_lines[f][line] = dst_lines[f].get(line, 0) + count
    for f, per_fn in src_funcs.items():
        for name, (line, count) in per_fn.items():
            prev = dst_funcs[f].get(name, (line, 0))
            dst_funcs[f][name] = (prev[0] or line, prev[1] + count)


def totals(lines: Lines, funcs: Funcs) -> tuple[int, int, int, int]:
    lf = sum(len(v) for v in lines.values())
    lh = sum(1 for v in lines.values() for c in v.values() if c > 0)
    fnf = sum(len(v) for v in funcs.values())
    fnh = sum(1 for v in funcs.values() for (_, c) in v.values() if c > 0)
    return lh, lf, fnh, fnf


def pct(hit: int, total: int) -> str:
    return f"{100.0 * hit / total:.2f}%" if total else "—"


def component_of(path: str) -> str:
    # Paths from cargo-llvm-cov are absolute; group by repo top-level dir.
    for marker in ("crates/", "server/", "cli/", "fuzz/"):
        idx = path.find("/" + marker)
        if idx == -1 and path.startswith(marker):
            idx = -1  # relative path starting with the marker
        elif idx == -1:
            continue
        rel = path[idx + 1 :] if idx >= 0 else path
        if marker == "crates/":
            parts = rel.split("/")
            if len(parts) >= 2:
                return f"crates/{parts[1]}"
        return marker.rstrip("/")
    return "other"


def write_lcov(path: Path, lines: Lines, funcs: Funcs) -> None:
    with path.open("w") as out:
        for f in sorted(set(lines) | set(funcs)):
            out.write(f"SF:{f}\n")
            per_fn = funcs.get(f, {})
            for name, (line, _count) in sorted(per_fn.items(), key=lambda kv: kv[1][0]):
                out.write(f"FN:{line},{name}\n")
            for name, (_line, count) in sorted(per_fn.items(), key=lambda kv: kv[1][0]):
                out.write(f"FNDA:{count},{name}\n")
            out.write(f"FNF:{len(per_fn)}\n")
            out.write(f"FNH:{sum(1 for (_, c) in per_fn.values() if c > 0)}\n")
            per_line = lines.get(f, {})
            for line in sorted(per_line):
                out.write(f"DA:{line},{per_line[line]}\n")
            out.write(f"LF:{len(per_line)}\n")
            out.write(f"LH:{sum(1 for c in per_line.values() if c > 0)}\n")
            out.write("end_of_record\n")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", default="merged.lcov")
    ap.add_argument("inputs", nargs="+", metavar="NAME=PATH")
    args = ap.parse_args()

    merged_lines: Lines = defaultdict(dict)
    merged_funcs: Funcs = defaultdict(dict)
    rows: list[tuple[str, str, str, str, str]] = []

    for spec in args.inputs:
        name, _, path_s = spec.partition("=")
        path = Path(path_s)
        if not path_s or not path.is_file():
            rows.append((name, "—", "absent", "—", "absent"))
            continue
        lines, funcs = parse_lcov(path)
        lh, lf, fnh, fnf = totals(lines, funcs)
        rows.append((name, pct(lh, lf), f"{lh}/{lf}", pct(fnh, fnf), f"{fnh}/{fnf}"))
        merge_into(merged_lines, merged_funcs, lines, funcs)

    lh, lf, fnh, fnf = totals(merged_lines, merged_funcs)
    rows.append(("**merged**", f"**{pct(lh, lf)}**", f"{lh}/{lf}", f"**{pct(fnh, fnf)}**", f"{fnh}/{fnf}"))

    print("## Code coverage")
    print()
    print("### By test category")
    print()
    print("| Category | Line coverage | Lines | Function coverage | Functions |")
    print("|---|---:|---:|---:|---:|")
    for row in rows:
        print("| " + " | ".join(row) + " |")

    per_comp: dict[str, tuple[int, int]] = defaultdict(lambda: (0, 0))
    for f, per_line in merged_lines.items():
        comp = component_of(f)
        covered, total = per_comp[comp]
        per_comp[comp] = (
            covered + sum(1 for c in per_line.values() if c > 0),
            total + len(per_line),
        )

    print()
    print("### Merged, by component")
    print()
    print("| Component | Line coverage | Lines |")
    print("|---|---:|---:|")
    for comp in sorted(per_comp):
        covered, total = per_comp[comp]
        print(f"| `{comp}` | {pct(covered, total)} | {covered}/{total} |")
    print(f"| **total** | **{pct(lh, lf)}** | {lh}/{lf} |")

    write_lcov(Path(args.output), merged_lines, merged_funcs)
    print(f"\n<!-- merged lcov written to {args.output} -->", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
