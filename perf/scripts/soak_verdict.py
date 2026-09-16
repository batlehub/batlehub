#!/usr/bin/env python3
"""The verdict for `perf/scripts/soak.sh`: did the server give anything back?

Reads the sampler's CSV and the phase marks, measures five things in the two
*idle* windows the script arranged, and fails the run when any of them grew
past its threshold.

The five are deliberately different kinds of resource, because they fail
differently and a leak in one is invisible in the others:

* **RSS** — memory still held with nothing in flight. Reported as a percentage
  because the absolute number depends on the allocator and on how much was
  cached during warm-up. It is a statement about the *process* only as long as
  jemalloc purges in the background (`server/Cargo.toml`, the
  `background_threads` feature): without that, an idle process never hands a
  dirty page back and this row measures how much longer the load phase ran than
  the warm-up did.
* **File descriptors** — a socket or file not closed. The one that takes a
  server down hardest, because it ends in `EMFILE` on *accept*, which looks
  like a network fault rather than like a bug.
* **Live heap** — jemalloc's `stats.allocated`, bytes in live allocations. The
  row that says *whose* memory the RSS row is measuring: RSS counts pages, and
  an allocator may hold pages the program has freed, so a clean server can fail
  the RSS row while this one is flat. Absent (not zero) on a build without the
  `jemalloc` feature.
* **Threads** — a spawned worker that never joins.
* **Database connections held** (`pool_size - available`) — a handler that took
  a connection and did not return it. Bounded by the pool, so it does not grow
  without limit: it stops at "every request now waits forever".

A sixth signal is not a growth measurement at all: the **slope** of RSS during
the sustained load. A leak that is slower than the run is long shows up as a
line that never flattens, hours before the idle windows differ enough to fail.

Panics in the server log and a non-zero k6 exit are reported alongside, because
a soak that leaked nothing because it stopped serving is not a pass.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import statistics
import tempfile
from dataclasses import dataclass
from pathlib import Path


# Every file this script reads or writes, and the one place they live.
#
# `soak.sh` writes them here under these names, and this script resolves the
# directory from its **own location** — `perf/scripts/soak_verdict.py` →
# `perf/results/`. Nothing is passed in, which is the point: a path-shaped
# argument is a path-shaped injection, and the way to be sure there is none is
# not to take one. What crosses the process boundary is three numbers (the k6
# exit code, the duration, the rate) and nothing that can name a file.
#
# It replaced a resolve-and-confine validator on seven path arguments. That
# guard held — `--report /root/.ssh/authorized_keys` was refused — but it kept
# a tainted value flowing into `open()`, which is a shape that survives only as
# long as the next person keeps the guard (docs/internal/sonar-triage-2026-09-15.md).
RESULTS_DIR = Path(__file__).resolve().parents[1] / "results"

SAMPLES_FILE = RESULTS_DIR / "soak-samples.csv"
MARKS_FILE = RESULTS_DIR / "soak-marks.txt"
SERVER_LOG_FILE = RESULTS_DIR / "soak-server.log"
K6_SUMMARY_FILE = RESULTS_DIR / "soak-k6-steady.json"
METRICS_BEFORE_FILE = RESULTS_DIR / "soak-metrics-steady-start.txt"
METRICS_AFTER_FILE = RESULTS_DIR / "soak-metrics-steady-end.txt"
CHART_FILE = RESULTS_DIR / "soak-chart.svg"
REPORT_FILE = RESULTS_DIR / "soak.md"


# A window is measured over its last third rather than its whole span: the
# first part of a quiesce is the server still draining, which is the transient
# the quiesce exists to skip.
WINDOW_TAIL_FRACTION = 1 / 3
MIN_WINDOW_SAMPLES = 5

# The slope is fitted over the **last two thirds** of the sustained load, and is
# a verdict only when that load ran for at least this long.
#
# Both bounds exist because the first minutes of load are not a trend: caches,
# pools and allocator arenas fill, and RSS climbs steeply and then flattens. Fit
# from the first sample and a short run reads as a leak of tens of MiB a minute
# — measured at 10.5 MiB/min on a 30-second window whose *idle* comparison was
# +4.2 %, which is the fill and nothing else. So the fill is skipped, and under
# five minutes the number is reported without being judged: a window that short
# cannot tell a leak from a cache warming up, and a gate that cannot tell should
# say so rather than guess.
SLOPE_SKIP_FRACTION = 1 / 3
MIN_SLOPE_WINDOW_SECONDS = 300


@dataclass
class Sample:
    epoch_s: int
    rss_mib: float
    fds: int | None
    threads: int | None
    pool_size: float | None
    pool_idle: float | None
    heap_mib: float | None

    @property
    def pool_held(self) -> float | None:
        if self.pool_size is None or self.pool_idle is None:
            return None
        return self.pool_size - self.pool_idle


@dataclass
class Check:
    name: str
    baseline: float | None
    final: float | None
    growth: float | None
    threshold: float
    unit: str
    ok: bool
    note: str = ""


def read_samples(path: Path) -> list[Sample]:
    out: list[Sample] = []
    with path.open() as fh:
        for row in csv.DictReader(fh):
            try:
                epoch = int(row["epoch_s"])
                rss = float(row["rss_kb"]) / 1024.0
            except (KeyError, TypeError, ValueError):
                continue

            def num(key: str):
                raw = (row.get(key) or "").strip()
                if not raw:
                    return None
                try:
                    return float(raw)
                except ValueError:
                    return None

            fds = num("fds")
            threads = num("threads")
            out.append(
                Sample(
                    epoch_s=epoch,
                    rss_mib=rss,
                    fds=int(fds) if fds is not None else None,
                    threads=int(threads) if threads is not None else None,
                    pool_size=num("pool_size"),
                    pool_idle=num("pool_idle"),
                    heap_mib=(
                        heap / 1024.0 if (heap := num("heap_kb")) is not None else None
                    ),
                )
            )
    return out


def read_marks(path: Path) -> dict[str, int]:
    marks: dict[str, int] = {}
    if not path.exists():
        return marks
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) == 2:
            try:
                marks[parts[0]] = int(parts[1])
            except ValueError:
                pass
    return marks


def window(samples: list[Sample], start: int | None, end: int | None) -> list[Sample]:
    """The last third of [start, end], with a floor so a short window still has points."""
    if start is None or end is None:
        return []
    inside = [s for s in samples if start <= s.epoch_s <= end]
    if not inside:
        return []
    tail = max(MIN_WINDOW_SAMPLES, int(len(inside) * WINDOW_TAIL_FRACTION))
    return inside[-tail:]


def median_of(samples: list[Sample], attr: str) -> float | None:
    values = [
        getattr(s, attr) for s in samples if getattr(s, attr, None) is not None
    ]
    if not values:
        return None
    return float(statistics.median(values))


def slope_mib_per_min(samples: list[Sample]) -> tuple[float, float] | None:
    """Least-squares slope of RSS over the trend part of the load, and its
    standard error. Both in MiB/minute.

    Least squares rather than (last - first) / span: a soak's RSS sawtooths
    with every cache sweep, and two endpoints landing on different teeth is a
    number with no relationship to the trend.

    **The standard error is what makes the slope a verdict rather than a
    reading.** The plateau this fits across is not a line with noise on it, it
    is a sawtooth: measured on three 10-minute runs of the 24-kind mix, the
    residual scatter is 9–14 MiB and the peak-to-peak swing 48–73 MiB, against a
    2.00 MiB/min limit that describes 13 MiB over the same window. Three runs of
    the *same* plateau fitted 0.28, 1.27 and 2.08 MiB/min — the differences
    being which tooth the window opened and closed on, and the third one failing
    a gate the other two passed.

    So the caller compares the limit against the **lower bound** of the fit's
    95 % interval (`slope - 2·se`), which is the strongest claim the data
    supports: *the trend is above the limit even allowing for the scatter*. What
    that gives up is measurable — planted on those same three runs, a leak of
    +2.5 MiB/min is caught in all three and +2.0 in two of three — and what it
    buys is that a clean run cannot fail on the phase of a sawtooth.

    The first `SLOPE_SKIP_FRACTION` of the window is dropped — that part is the
    fill, not the trend.
    """
    if len(samples) < 10:
        return None
    samples = samples[int(len(samples) * SLOPE_SKIP_FRACTION):]
    if len(samples) < 10:
        return None
    t0 = samples[0].epoch_s
    xs = [(s.epoch_s - t0) / 60.0 for s in samples]
    ys = [s.rss_mib for s in samples]
    n = len(xs)
    mean_x = sum(xs) / n
    mean_y = sum(ys) / n
    denom = sum((x - mean_x) ** 2 for x in xs)
    if denom == 0:
        return None
    slope = sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) / denom
    intercept = mean_y - slope * mean_x
    residual_var = sum((y - (intercept + slope * x)) ** 2 for x, y in zip(xs, ys)) / (
        n - 2
    )
    return slope, math.sqrt(residual_var / denom)


# ── Per-registry accounting ──────────────────────────────────────────────────
#
# Read from the server's own `/metrics`, twice, and subtracted. The server is
# the only party that knows what a request *cost* it: a load generator can say
# how many requests a registry took and how many bytes came back, and neither is
# how long the server spent nor how much it had to pull from upstream to answer
# them.


def parse_prometheus(text: str) -> dict[tuple[str, tuple[tuple[str, str], ...]], float]:
    """`{(name, sorted labels): value}` for every sample in an exposition.

    A deliberately small parser: no HELP/TYPE handling, no histograms beyond the
    `_sum`/`_count` series the exposition already flattens, and unknown lines
    skipped rather than rejected. It reads one server's own output, and the
    alternative is a dependency for twenty lines.
    """
    out: dict[tuple[str, tuple[tuple[str, str], ...]], float] = {}
    for raw_line in text.splitlines():
        parsed = parse_prometheus_line(raw_line)
        if parsed is not None:
            key, value = parsed
            out[key] = value
    return out


def parse_labels(rest: str) -> tuple[tuple[str, str], ...]:
    """The `{a="1",b="2"}` half of a sample line, sorted so two are comparable."""
    labels: list[tuple[str, str]] = []
    for pair in rest.rstrip("}").split(","):
        key, _, val = pair.partition("=")
        if key:
            labels.append((key.strip(), val.strip().strip('"')))
    return tuple(sorted(labels))


def parse_prometheus_line(
    raw_line: str,
) -> tuple[tuple[str, tuple[tuple[str, str], ...]], float] | None:
    """One exposition line as `((name, labels), value)`, or `None` to skip it."""
    line = raw_line.strip()
    if not line or line.startswith("#"):
        return None
    try:
        head, raw_value = line.rsplit(" ", 1)
        value = float(raw_value)
    except ValueError:
        return None
    if "{" not in head:
        return (head, ()), value
    name, _, rest = head.partition("{")
    return (name, parse_labels(rest)), value


def by_registry(samples: dict, metric: str) -> dict[str, float]:
    """Every value of `metric` that carries a `registry` label, summed per registry."""
    out: dict[str, float] = {}
    for (name, labels), value in samples.items():
        if name != metric:
            continue
        registry = dict(labels).get("registry")
        if registry:
            out[registry] = out.get(registry, 0.0) + value
    return out


def delta(before: dict, after: dict, metric: str) -> dict[str, float]:
    """What each registry accrued between the two scrapes.

    Clamped at zero: these are counters, so a negative difference means the
    process restarted mid-run, and reporting "-4 requests" would be worse than
    reporting none.
    """
    b, a = by_registry(before, metric), by_registry(after, metric)
    return {
        registry: max(0.0, value - b.get(registry, 0.0)) for registry, value in a.items()
    }


@dataclass
class RegistryCost:
    name: str
    requests: float
    seconds: float
    upstream_bytes: float
    artifact_misses: float
    metadata_misses: float
    from_document: float

    @property
    def ms_per_request(self) -> float | None:
        return (self.seconds / self.requests * 1000) if self.requests else None


def registry_costs(before: dict, after: dict) -> list[RegistryCost]:
    """Every registry that did anything during the load, dearest first.

    Ranked by **seconds of request handling**, which is the closest thing the
    server knows to "what this registry cost me": it is wall-clock inside the
    handler, so it counts the upstream wait, the parse and the filter alike.
    Bytes and misses are reported beside it because they are what an operator
    can act on — a registry that is dear because it misses is a caching problem,
    one that is dear per request is a document problem.
    """
    requests = delta(before, after, "batlehub_requests_total")
    seconds = delta(before, after, "batlehub_request_duration_seconds_sum")
    cached = delta(before, after, "batlehub_cached_bytes_total")
    amiss = delta(before, after, "batlehub_artifact_cache_misses_total")
    mmiss = delta(before, after, "batlehub_metadata_cache_misses_total")
    fromdoc = delta(before, after, "batlehub_metadata_from_document_total")

    names = set(requests) | set(seconds) | set(cached)
    costs = [
        RegistryCost(
            name=n,
            requests=requests.get(n, 0.0),
            seconds=seconds.get(n, 0.0),
            upstream_bytes=cached.get(n, 0.0),
            artifact_misses=amiss.get(n, 0.0),
            metadata_misses=mmiss.get(n, 0.0),
            from_document=fromdoc.get(n, 0.0),
        )
        for n in names
    ]
    costs = [c for c in costs if c.requests or c.seconds or c.upstream_bytes]
    costs.sort(key=lambda c: (c.seconds, c.requests), reverse=True)
    return costs


def human_bytes(n: float) -> str:
    for unit in ("B", "KiB", "MiB", "GiB"):
        if abs(n) < 1024 or unit == "GiB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} GiB"


def bar(value: float, peak: float, width: int = 24) -> str:
    """A proportional bar. Eighth-blocks, so a small share is still visible."""
    if peak <= 0:
        return ""
    filled = value / peak * width
    full = int(filled)
    rest = filled - full
    eighths = " ▏▎▍▌▋▊▉█"
    tail = eighths[min(8, int(rest * 8 + 0.5))] if full < width else ""
    return "█" * full + tail


def plot(
    series: list[tuple[str, list[float], str]],
    height: int = 8,
    width: int = 64,
) -> list[str]:
    """A small multi-row line chart, as text.

    Text rather than an image because this is read in three places — a terminal,
    a job summary and a pull request comment — and only one of them can show a
    picture that is not a URL. The SVG beside it is for when a picture is wanted;
    this is the one that always renders.

    Each series gets its own axis: RSS in hundreds of MiB and a descriptor count
    in tens share no scale, and normalising them onto one would draw a flat line
    under a jagged one and say nothing about either.
    """
    lines: list[str] = []
    for label, values, unit in series:
        lines.extend(plot_one(label, values, unit, height, width))
    return lines


def bucket(values: list[float], width: int) -> list[float]:
    """`values` reduced to at most `width` points, each the median of its bucket."""
    step = max(1, len(values) // width)
    buckets = [values[i : i + step] for i in range(0, len(values), step)][:width]
    return [statistics.median(b) for b in buckets if b]


def plot_grid(points: list[float], lo: float, hi: float, height: int) -> list[list[str]]:
    """The character grid for one series, rows top to bottom."""
    span = hi - lo or 1.0

    def row_of(value: float) -> int:
        # A point lands in a row when it reaches that band.
        return height - 1 - int((value - lo) / span * (height - 1))

    grid = [[" "] * len(points) for _ in range(height)]
    for x, v in enumerate(points):
        row = row_of(v)
        grid[row][x] = "●"
        # Join to the previous point so the eye follows a line rather than a
        # scatter — the shape is the finding.
        if x:
            prev = row_of(points[x - 1])
            for r in range(min(row, prev) + 1, max(row, prev)):
                grid[r][x] = "│"
    return grid


def axis_gutter(row_index: int, height: int, lo: float, hi: float) -> str:
    """The value printed beside a row: the top and bottom bands carry one."""
    if row_index == 0:
        axis = hi
    elif row_index == height - 1:
        axis = lo
    else:
        return " " * 7 + " │"
    return f"{axis:7.0f} │"


def plot_one(
    label: str,
    values: list[float],
    unit: str,
    height: int,
    width: int,
) -> list[str]:
    """One labelled series, axis gutter and baseline included."""
    if len(values) < 2:
        return []
    points = bucket(values, width)
    lo, hi = min(points), max(points)
    grid = plot_grid(points, lo, hi, height)

    lines = [f"{label} ({lo:.0f}–{hi:.0f} {unit})"]
    for r, row in enumerate(grid):
        lines.append(axis_gutter(r, height, lo, hi) + "".join(row))
    lines.append(" " * 8 + "└" + "─" * len(points))
    lines.append("")
    return lines


def svg_chart(samples: list[Sample], marks: dict[str, int], path: Path) -> None:
    """The same curves as a picture, for whoever opens the artifact.

    Hand-written SVG rather than a plotting library: this runs on a CI runner
    with nothing installed but Python, and the chart is four polylines, three
    shaded bands and a legend.

    Each curve is scaled to its own range and says so. RSS in hundreds of MiB
    and a descriptor count in tens share no axis, and drawing them on one would
    flatten the smaller into a straight line at the bottom — which is exactly
    the curve a leak in it would live on.
    """
    if len(samples) < 2:
        return
    width, height, pad = 900, 320, 44
    t0, t1 = samples[0].epoch_s, samples[-1].epoch_s
    span = (t1 - t0) or 1
    plot_w, plot_h = width - 2 * pad, height - 2 * pad

    def x_of(epoch: float) -> float:
        return pad + (epoch - t0) / span * plot_w

    def curve(attr: str):
        points = [(s, getattr(s, attr)) for s in samples if getattr(s, attr) is not None]
        if len(points) < 2:
            return None
        lo = min(v for _, v in points)
        hi = max(v for _, v in points)
        scale = (hi - lo) or 1
        xy = [(x_of(s.epoch_s), height - pad - (v - lo) / scale * plot_h) for s, v in points]
        return xy, lo, hi

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" height="{height}" '
        f'font-family="ui-monospace,SFMono-Regular,Menlo,monospace" font-size="11">',
        f'<rect width="{width}" height="{height}" fill="#0d1117"/>',
    ]

    # The phases, shaded: the two idle windows the verdict compares are the
    # point of the picture, and an unlabelled curve hides which part is which.
    for start_mark, end_mark, colour, label in (
        ("warmup_end", "baseline_end", "#1f6feb", "baseline"),
        ("steady_start", "steady_end", "#8957e5", "load"),
        ("steady_end", "final_end", "#1f6feb", "final"),
    ):
        if start_mark in marks and end_mark in marks:
            xa, xb = x_of(marks[start_mark]), x_of(marks[end_mark])
            parts.append(
                f'<rect x="{xa:.1f}" y="{pad}" width="{max(1.0, xb - xa):.1f}" '
                f'height="{plot_h}" fill="{colour}" opacity="0.13"/>'
            )
            parts.append(
                f'<text x="{(xa + xb) / 2:.1f}" y="{pad - 8}" fill="#8b949e" '
                f'text-anchor="middle">{label}</text>'
            )

    legend = []
    for attr, colour, label in (
        ("rss_mib", "#3fb950", "RSS MiB"),
        ("fds", "#d29922", "open fds"),
        ("threads", "#58a6ff", "threads"),
        ("pool_held", "#f85149", "db conns held"),
    ):
        got = curve(attr)
        if not got:
            continue
        xy, lo, hi = got
        points = " ".join(f"{x:.1f},{y:.1f}" for x, y in xy)
        parts.append(
            f'<polyline points="{points}" fill="none" stroke="{colour}" stroke-width="1.6"/>'
        )
        legend.append((colour, f"{label} {lo:.0f}\u2013{hi:.0f}"))

    for i, (colour, text) in enumerate(legend):
        x = pad + i * 210
        parts.append(f'<rect x="{x}" y="{height - 24}" width="10" height="3" fill="{colour}"/>')
        parts.append(f'<text x="{x + 16}" y="{height - 19}" fill="#8b949e">{text}</text>')

    parts.append(
        f'<text x="{pad}" y="{height - 34}" fill="#6e7681">each curve is scaled to its '
        f'own range \u2014 {len(samples)} samples over {span}s</text>'
    )
    parts.append("</svg>")
    path.write_text("\n".join(parts) + "\n")


def env_float(name: str, default: float) -> float:
    try:
        return float(os.environ.get(name, default))
    except ValueError:
        return default


def check_growth(
    name: str,
    baseline: float | None,
    final: float | None,
    threshold: float,
    unit: str,
    *,
    percent: bool = False,
) -> Check:
    if baseline is None or final is None:
        return Check(name, baseline, final, None, threshold, unit, True, "not measured")
    if percent:
        growth = ((final - baseline) / baseline * 100.0) if baseline else 0.0
    else:
        growth = final - baseline
    return Check(name, baseline, final, growth, threshold, unit, growth <= threshold)


def fmt(value: float | None, digits: int = 1) -> str:
    return "—" if value is None else f"{value:.{digits}f}"


def resource_checks(baseline_w: list[Sample], final_w: list[Sample]) -> list[Check]:
    """The five idle-window comparisons, in report order. RSS is first: the
    warm-up exemption below reaches for `checks[0]`."""
    return [
        check_growth(
            "RSS (idle)",
            median_of(baseline_w, "rss_mib"),
            median_of(final_w, "rss_mib"),
            env_float("SOAK_MAX_RSS_GROWTH_PCT", 10.0),
            "%",
            percent=True,
        ),
        # The row that says *whose* memory it is. RSS counts pages, and an
        # allocator is free to hold pages the program has already freed — which
        # is how a clean server fails a leak gate. `stats.allocated` counts
        # bytes in live allocations, so it moves only when the program is
        # holding more than it was.
        #
        # Tighter than the RSS limit (5 % against 10 %) because it does not
        # carry the allocator's bookkeeping: a live heap that is 5 % larger
        # after ten minutes of the same workload is the process keeping
        # something, and the caches that legitimately grow — metadata, storage —
        # do not live in this process's heap (`config.soak.toml` puts the cache
        # in Postgres and the artifacts on disk).
        #
        # Absent on a build without the `jemalloc` feature: the sampler writes
        # an empty column and `check_growth` reports it as not measured, which
        # is the honest answer rather than a zero.
        check_growth(
            "Live heap (idle)",
            median_of(baseline_w, "heap_mib"),
            median_of(final_w, "heap_mib"),
            env_float("SOAK_MAX_HEAP_GROWTH_PCT", 5.0),
            "%",
            percent=True,
        ),
        check_growth(
            "File descriptors",
            median_of(baseline_w, "fds"),
            median_of(final_w, "fds"),
            env_float("SOAK_MAX_FD_GROWTH", 16.0),
            "fds",
        ),
        check_growth(
            "Threads",
            median_of(baseline_w, "threads"),
            median_of(final_w, "threads"),
            env_float("SOAK_MAX_THREAD_GROWTH", 4.0),
            "threads",
        ),
        check_growth(
            "DB connections held",
            median_of(baseline_w, "pool_held"),
            median_of(final_w, "pool_held"),
            env_float("SOAK_MAX_POOL_GROWTH", 2.0),
            "conns",
        ),
    ]


def read_panics(path: Path | None) -> list[str]:
    """Every `panicked at` line in the server log, whole."""
    if not path or not path.exists():
        return []
    return [
        line.strip()
        for line in path.read_text(errors="replace").splitlines()
        if "panicked at" in line
    ]


def read_k6(path: Path | None) -> tuple[float | None, float | None, float | None]:
    """`(failure rate, request count, p95 ms)` from a k6 summary.

    The p95 is context, never a verdict: this is a leak gate and a latency
    threshold here would fail runs for reasons that have nothing to do with what
    the process is holding. It is reported because the two questions are asked
    together often enough — an allocator change, a cache backend, a new registry
    kind in the mix all move both — and because a report that says "no leak" on
    a run whose p95 quadrupled has answered the smaller half of the question.
    """
    if not path or not path.exists():
        return None, None, None
    try:
        metrics = json.loads(path.read_text()).get("metrics", {})
    except (json.JSONDecodeError, AttributeError):
        return None, None, None
    failed = metrics.get("http_req_failed", {})
    duration = metrics.get("http_req_duration", {})
    return (
        failed.get("value", failed.get("rate")),
        metrics.get("http_reqs", {}).get("count"),
        duration.get("p(95)"),
    )


def read_costs(before_path: Path | None, after_path: Path | None) -> list[RegistryCost]:
    """The per-registry ranking, or an empty list when either exposition is missing."""
    if not before_path or not after_path:
        return []
    try:
        before = parse_prometheus(before_path.read_text())
        after = parse_prometheus(after_path.read_text())
    except OSError:
        return []
    return registry_costs(before, after)


def check_verdict(c: Check) -> str:
    """The right-hand cell: a note when the check was not judged, else the verdict."""
    if c.note:
        return c.note
    return "ok" if c.ok else "**over**"


def check_growth_cell(c: Check, digits: int) -> str:
    """The growth cell, signed, or an em dash when there was nothing to compare."""
    if c.growth is None:
        return "—"
    return f"{c.growth:+.{digits}f} {c.unit}"


def checks_table(checks: list[Check]) -> list[str]:
    """The four idle-window rows. The RSS-trend row is appended by the caller,
    which is the only one that knows whether the load ran long enough to judge it."""
    lines = [
        "| | baseline | final | growth | limit | |",
        "| --- | ---: | ---: | ---: | ---: | :-- |",
    ]
    for c in checks:
        digits = 1 if c.unit == "%" else 0
        lines.append(
            f"| {c.name} | {fmt(c.baseline, digits)} | {fmt(c.final, digits)} | "
            f"{check_growth_cell(c, digits)} | {fmt(c.threshold, digits)} {c.unit} | "
            f"{check_verdict(c)} |"
        )
    return lines


def chart_section(samples: list[Sample]) -> list[str]:
    """The text chart, in a collapsible block. Empty when there is nothing to draw."""
    chart = plot(
        [
            ("RSS", [s.rss_mib for s in samples], "MiB"),
            ("Open descriptors", [float(s.fds) for s in samples if s.fds is not None], "fds"),
        ]
    )
    if not chart:
        return []
    return [
        "<details open><summary>Resource use over the run</summary>",
        "",
        "```",
        "phases: warm-up | quiesce | BASELINE | load | quiesce | FINAL (left to right)",
        "",
        *chart,
        "```",
        "</details>",
        "",
    ]


def svg_section(
    samples: list[Sample],
    marks: dict[str, int],
    path: Path | None,
) -> list[str]:
    """The note pointing at the SVG, having written it. Empty when none was asked for."""
    if not path:
        return []
    try:
        svg_chart(samples, marks, path)
    except OSError as e:
        return [f"(the SVG chart could not be written: {e})", ""]
    return [
        f"A plotted version of the same run is in `{path.name}`, "
        "beside this report in the run's artifacts.",
        "",
    ]


def costs_section(costs: list[RegistryCost]) -> list[str]:
    """Which registry cost the most, ranked by handling time. Empty without metrics."""
    if not costs:
        return []
    lines: list[str] = []
    peak = costs[0].seconds or 1.0
    total = sum(c.seconds for c in costs) or 1.0
    lines.append(f"### Worst consumer: `{costs[0].name}`")
    lines.append("")
    lines.append(
        f"{costs[0].seconds / total * 100:.0f}% of all request-handling time "
        "during the load. Ranked by seconds spent inside the handler, which "
        "counts the upstream wait, the parse and the filter alike — the "
        "closest thing the server knows to what a registry cost it."
    )
    lines.append("")
    lines.append("```")
    for c in costs:
        lines.append(
            f"{c.name:<18} {bar(c.seconds, peak):<25} {c.seconds:7.1f}s"
            f"  {c.requests:6.0f} req"
            + (f"  {c.ms_per_request:6.1f} ms/req" if c.ms_per_request else "")
        )
    lines.append("```")
    lines.append("")
    lines.append("| registry | handling time | requests | ms/req | pulled from upstream | artifact misses | document misses | resolved from cache |")
    lines.append("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for c in costs:
        lines.append(
            f"| `{c.name}` | {c.seconds:.1f} s | {c.requests:.0f} | "
            f"{fmt(c.ms_per_request)} | {human_bytes(c.upstream_bytes)} | "
            f"{c.artifact_misses:.0f} | {c.metadata_misses:.0f} | {c.from_document:.0f} |"
        )
    lines.append("")
    lines.append(
        "<sub>*ms/req* is where the cost is, not the traffic: a registry "
        "that is dear because it misses is a caching problem, one that is "
        "dear per request is a document problem. *resolved from cache* "
        "counts the coordinates answered from a listing this proxy already "
        "held, which are the upstream round trips it did **not** make.<br>"
        "**What this counts:** proxied reads — artifacts and listings — "
        "attributed by `batlehub_requests_total` and "
        "`batlehub_request_duration_seconds`. A **local** registry's own "
        "publishes and reads go through `LocalRegistryService`, which emits "
        "neither, so a local-mode registry is absent from this table rather "
        "than cheap.</sub>"
    )
    lines.append("")
    return lines


def parse_args() -> argparse.Namespace:
    """Three numbers describing the run. No file is named here — see `RESULTS_DIR`."""
    ap = argparse.ArgumentParser()
    ap.add_argument("--k6-exit", type=int, default=0)
    ap.add_argument("--duration", default="?")
    ap.add_argument("--rate", default="?")
    return ap.parse_args()


def exempt_rss_during_warmup(rss_check: Check, slope_judged: bool, steady_seconds: int) -> None:
    """Un-fail the idle-RSS comparison when the load was too short to judge it.

    It gets the same bound as the slope, for the same reason: under five minutes
    of load the caches are still filling, and "idle RSS grew" is then "the cache
    got bigger" — which is the server working. The descriptor, thread and
    connection counts have no such warm-up, so they are judged at any length.
    """
    if slope_judged or rss_check.ok:
        return
    rss_check.ok = True
    rss_check.note = (
        f"not judged — {steady_seconds}s of load, under the "
        f"{MIN_SLOPE_WINDOW_SECONDS}s the caches need to fill"
    )


def slope_row(
    fitted: tuple[float, float] | None,
    slope_limit: float,
    slope_ok: bool,
    slope_judged: bool,
    steady_seconds: int,
) -> str:
    """The RSS-trend row, which only the caller knows whether to judge.

    The measured value carries its interval, because the interval is the
    difference between "this run trended upward" and "this run cannot tell":
    a `2.08 ±0.56` that reads `ok` is not a threshold being lenient, it is the
    run saying the fit does not resolve the limit.
    """
    slope = fitted[0] if fitted else None
    interval = f" ±{2 * fitted[1]:.2f}" if fitted else ""
    if slope_judged:
        verdict = "ok" if slope_ok else "**over**"
    else:
        verdict = (
            f"not judged — {steady_seconds}s of load, under the "
            f"{MIN_SLOPE_WINDOW_SECONDS}s a trend needs"
        )
    return (
        f"| RSS trend under load | — | — | {fmt(slope, 2)}{interval} MiB/min | "
        f"{slope_limit:.2f} MiB/min | {verdict} |"
    )


def header_lines(duration: str, rate: str, reqs: float | None, ok: bool) -> list[str]:
    """The verdict heading and the one sentence that says what was offered."""
    offered = f", {int(reqs):,} requests" if reqs else ""
    return [
        "<!-- soak-report -->",
        f"## Soak — {'no leak detected' if ok else 'FAILED'}",
        "",
        f"`{duration}` of load at `{rate}` req/s{offered}. Both windows below "
        "are **idle**: warm-up, quiesce, *baseline*, load, quiesce, *final*.",
        "",
    ]


def tail_lines(
    failed_rate: float | None,
    k6_exit: int,
    panics: list[str],
    ok: bool,
    p95_ms: float | None = None,
) -> list[str]:
    """What k6 and the server log had to say, and how to reproduce a failure."""
    lines: list[str] = []
    if failed_rate is not None:
        served = f"k6 request failure rate: {failed_rate * 100:.2f}%"
        if p95_ms is not None:
            served += f" · p95 {p95_ms:.0f} ms (reported, not judged)"
        lines.append(served)
    if k6_exit != 0:
        lines.append(
            f"**k6 exited {k6_exit}** — a threshold in the scenario was "
            "breached, or the load could not be offered."
        )
    if panics:
        lines.append("")
        lines.append(f"**{len(panics)} panic(s) in the server log:**")
        lines.extend(f"- `{p}`" for p in panics[:5])
    if not ok:
        lines.append("")
        lines.append(
            "A row marked **over** is a resource the process was still holding "
            "with nothing in flight. Reproduce with "
            "`task perf:soak DURATION=… RATE=…`; the per-second samples are in "
            "`soak-samples.csv` beside this report."
        )
    return lines


def main() -> int:
    args = parse_args()

    samples = read_samples(SAMPLES_FILE)
    marks = read_marks(MARKS_FILE)

    if len(samples) < 2 * MIN_WINDOW_SAMPLES:
        REPORT_FILE.write_text(
            "<!-- soak-report -->\n## Soak — inconclusive\n\n"
            f"Only {len(samples)} samples were collected; the server process was "
            "not observable for long enough to say anything.\n"
        )
        return 1

    baseline_w = window(samples, marks.get("warmup_end"), marks.get("baseline_end"))
    final_w = window(samples, marks.get("steady_end"), marks.get("final_end"))
    steady_w = [
        s
        for s in samples
        if marks.get("steady_start", 0) <= s.epoch_s <= marks.get("steady_end", 0)
    ]

    checks = resource_checks(baseline_w, final_w)

    steady_seconds = (
        steady_w[-1].epoch_s - steady_w[0].epoch_s if len(steady_w) > 1 else 0
    )
    fitted = slope_mib_per_min(steady_w)
    slope_limit = env_float("SOAK_MAX_RSS_SLOPE_MIB_PER_MIN", 2.0)
    slope_judged = steady_seconds >= MIN_SLOPE_WINDOW_SECONDS
    # The lower bound of the fit's 95 % interval, not the fit: see
    # `slope_mib_per_min`. A run fails when the *scatter cannot explain* a trend
    # above the limit.
    slope_ok = (
        fitted is None
        or not slope_judged
        or (fitted[0] - 2 * fitted[1]) <= slope_limit
    )

    panics = read_panics(SERVER_LOG_FILE)
    failed_rate, reqs, p95_ms = read_k6(K6_SUMMARY_FILE)

    exempt_rss_during_warmup(checks[0], slope_judged, steady_seconds)

    ok = all(c.ok for c in checks) and slope_ok and not panics and args.k6_exit == 0

    lines = header_lines(args.duration, args.rate, reqs, ok)
    lines.extend(checks_table(checks))
    lines.append(slope_row(fitted, slope_limit, slope_ok, slope_judged, steady_seconds))
    lines.append("")

    # ── What consumed what, over time ────────────────────────────────────────
    lines.extend(chart_section(samples))
    lines.extend(svg_section(samples, marks, CHART_FILE))

    # ── Which registry cost the most ─────────────────────────────────────────
    costs = read_costs(METRICS_BEFORE_FILE, METRICS_AFTER_FILE)

    lines.extend(costs_section(costs))

    lines.extend(tail_lines(failed_rate, args.k6_exit, panics, ok, p95_ms))

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    REPORT_FILE.write_text("\n".join(lines) + "\n")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
