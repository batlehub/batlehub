#!/usr/bin/env python3
"""Where the breaking-point scripts read and write — derived, never passed in.

`perf_report.py` explains the reasoning at length and this is the same
construction for the escalation's two helpers: the directory comes from *this
file's own location* (`perf/scripts/` → `perf/results/`), and the only thing
that crosses a process boundary is the escalation's **label** and the offered
**rate**, neither of which is path-shaped.

The escalation writes more than one file per run, and they are not all readable
artifacts: the summary k6 exports for a step and the JSONL the rows accumulate
into are scratch, thrown away when the run ends. Those used to live in a
`mktemp -d` that the shell passed in by path, which is exactly the argument
shape this module exists to avoid. They live in `work_dir(label)` now — under
`perf/results/` beside the report, removed by the script's own `cleanup` trap,
and gitignored.

A label is validated twice, and the two checks answer different questions:

* the pattern says it is **one path component** of a conservative alphabet, so
  there is no separator, no leading dot, and nothing a shell or a filesystem
  reads as anything but a name;
* `confined` then resolves the path it built and refuses one whose parent is
  not `perf/results/`, which is a statement about the *result* rather than
  about the input — the check that still holds if someone widens the pattern.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

# `perf/scripts/perf_paths.py` → `perf/results/`.
# `.resolve()` on the whole thing, not just on `__file__`: `confined()` below
# compares against this value after resolving what it built, and a symlinked
# `perf/results` would otherwise make the two sides disagree and refuse
# every path.
RESULTS_DIR = (Path(__file__).resolve().parents[1] / "results").resolve()

# A label names a run and nothing else: it must start alphanumeric and may
# carry only `.`, `-` and `_` after that. `breaking_point.sh` derives it from
# the config's file name (`config.soak-s3.toml` → `soak-s3`), so this is the
# shape it already produces.
LABEL_PATTERN = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")


def label(value: str) -> str:
    """An escalation's name, as an `argparse` type."""
    name = value.strip()
    if not LABEL_PATTERN.match(name):
        raise argparse.ArgumentTypeError(
            f"{value!r} is not a label: a label starts with a letter or digit and "
            f"carries only letters, digits, '.', '-' and '_' — it names a run, and "
            f"the files it names live in {RESULTS_DIR}"
        )
    return name


def confined(*parts: str) -> Path:
    """A path under `perf/results/`, proven rather than assumed.

    The parent is compared against the directory itself, so a component that
    climbed out of it — or into a subdirectory of it — is refused instead of
    being written to.
    """
    path = (RESULTS_DIR / Path(*parts)).resolve()
    if path.parent != RESULTS_DIR:
        raise ValueError(f"{path} is not directly inside {RESULTS_DIR}")
    return path


def samples_file(run: str) -> Path:
    """The sampler's CSV for a run — written by the shell, read by both scripts."""
    return confined(f"breaking-point-{run}-samples.csv")


def report_file(run: str) -> Path:
    """The Markdown report. The JSON sibling is this with `.json` in place of `.md`."""
    return confined(f"breaking-point-{run}.md")


def work_dir(run: str) -> Path:
    """Scratch for one run: k6's per-step summaries, the rows, the build log."""
    return confined(f"breaking-point-{run}-work")


def rows_file(run: str) -> Path:
    """One JSON row per step, appended as the escalation climbs."""
    path = (work_dir(run) / "rows.jsonl").resolve()
    if path.parent != work_dir(run):
        raise ValueError(f"{path} is not inside {work_dir(run)}")
    return path


def step_summary_file(run: str, rate: int) -> Path:
    """k6's `--summary-export` for one offered rate. `rate` is an `int`."""
    path = (work_dir(run) / f"step-{int(rate)}.json").resolve()
    if path.parent != work_dir(run):
        raise ValueError(f"{path} is not inside {work_dir(run)}")
    return path
