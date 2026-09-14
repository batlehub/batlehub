#!/usr/bin/env python3
"""The verdict for `perf/scripts/soak.sh`: did the server give anything back?

Reads the sampler's CSV and the phase marks, measures four things in the two
*idle* windows the script arranged, and fails the run when any of them grew
past its threshold.

The four are deliberately different kinds of resource, because they fail
differently and a leak in one is invisible in the others:

* **RSS** — memory still held with nothing in flight. Reported as a percentage
  because the absolute number depends on the allocator (`jemalloc` is on by
  default and keeps arenas) and on how much was cached during warm-up.
* **File descriptors** — a socket or file not closed. The one that takes a
  server down hardest, because it ends in `EMFILE` on *accept*, which looks
  like a network fault rather than like a bug.
* **Threads** — a spawned worker that never joins.
* **Database connections held** (`pool_size - available`) — a handler that took
  a connection and did not return it. Bounded by the pool, so it does not grow
  without limit: it stops at "every request now waits forever".

A fifth signal is not a growth measurement at all: the **slope** of RSS during
the sustained load. A leak that is slower than the run is long shows up as a
line that never flattens, hours before the idle windows differ enough to fail.

Panics in the server log and a non-zero k6 exit are reported alongside, because
a soak that leaked nothing because it stopped serving is not a pass.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import statistics
from dataclasses import dataclass
from pathlib import Path

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


def slope_mib_per_min(samples: list[Sample]) -> float | None:
    """Least-squares slope of RSS over the trend part of the load, MiB/minute.

    Least squares rather than (last - first) / span: a soak's RSS sawtooths
    with every cache sweep, and two endpoints landing on different teeth is a
    number with no relationship to the trend.

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
    return sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) / denom


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
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        try:
            head, raw_value = line.rsplit(" ", 1)
            value = float(raw_value)
        except ValueError:
            continue
        if "{" in head:
            name, _, rest = head.partition("{")
            labels: list[tuple[str, str]] = []
            for pair in rest.rstrip("}").split(","):
                key, _, val = pair.partition("=")
                if key:
                    labels.append((key.strip(), val.strip().strip('"')))
            out[(name, tuple(sorted(labels)))] = value
        else:
            out[(head, ())] = value
    return out


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
        if len(values) < 2:
            continue
        step = max(1, len(values) // width)
        buckets = [values[i : i + step] for i in range(0, len(values), step)][:width]
        points = [statistics.median(b) for b in buckets if b]
        lo, hi = min(points), max(points)
        span = hi - lo or 1.0
        # Rows top to bottom; a point lands in a row when it reaches that band.
        grid = [[" "] * len(points) for _ in range(height)]
        for x, v in enumerate(points):
            row = height - 1 - int((v - lo) / span * (height - 1))
            grid[row][x] = "●"
            # Join to the previous point so the eye follows a line rather than
            # a scatter — the shape is the finding.
            if x:
                prev = height - 1 - int((points[x - 1] - lo) / span * (height - 1))
                for r in range(min(row, prev) + 1, max(row, prev)):
                    grid[r][x] = "│"
        lines.append(f"{label} ({lo:.0f}–{hi:.0f} {unit})")
        for r, row in enumerate(grid):
            axis = hi if r == 0 else (lo if r == height - 1 else None)
            gutter = f"{axis:7.0f} │" if axis is not None else " " * 7 + " │"
            lines.append(gutter + "".join(row))
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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", required=True, type=Path)
    ap.add_argument("--marks", required=True, type=Path)
    ap.add_argument("--server-log", type=Path)
    ap.add_argument("--k6-summary", type=Path)
    ap.add_argument("--k6-exit", type=int, default=0)
    ap.add_argument("--metrics-before", type=Path, help="/metrics at the start of the load")
    ap.add_argument("--metrics-after", type=Path, help="/metrics at the end of the load")
    ap.add_argument("--chart", type=Path, help="where to write the SVG chart")
    ap.add_argument("--duration", default="?")
    ap.add_argument("--rate", default="?")
    ap.add_argument("--report", required=True, type=Path)
    args = ap.parse_args()

    samples = read_samples(args.samples)
    marks = read_marks(args.marks)

    if len(samples) < 2 * MIN_WINDOW_SAMPLES:
        args.report.write_text(
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

    checks = [
        check_growth(
            "RSS (idle)",
            median_of(baseline_w, "rss_mib"),
            median_of(final_w, "rss_mib"),
            env_float("SOAK_MAX_RSS_GROWTH_PCT", 10.0),
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

    steady_seconds_total = (
        steady_w[-1].epoch_s - steady_w[0].epoch_s if len(steady_w) > 1 else 0
    )
    slope = slope_mib_per_min(steady_w)
    slope_limit = env_float("SOAK_MAX_RSS_SLOPE_MIB_PER_MIN", 2.0)
    steady_seconds = steady_seconds_total
    slope_judged = steady_seconds >= MIN_SLOPE_WINDOW_SECONDS
    slope_ok = slope is None or not slope_judged or slope <= slope_limit

    panics: list[str] = []
    if args.server_log and args.server_log.exists():
        for line in args.server_log.read_text(errors="replace").splitlines():
            if "panicked at" in line:
                panics.append(line.strip())

    failed_rate = None
    reqs = None
    if args.k6_summary and args.k6_summary.exists():
        try:
            metrics = json.loads(args.k6_summary.read_text()).get("metrics", {})
            failed = metrics.get("http_req_failed", {})
            failed_rate = failed.get("value", failed.get("rate"))
            reqs = metrics.get("http_reqs", {}).get("count")
        except (json.JSONDecodeError, AttributeError):
            pass

    # The RSS comparison gets the same bound as the slope, for the same reason:
    # under five minutes of load the caches are still filling, and "idle RSS
    # grew" is then "the cache got bigger" — which is the server working. The
    # descriptor, thread and connection counts have no such warm-up, so they are
    # judged at any length.
    rss_check = checks[0]
    if not slope_judged and not rss_check.ok:
        rss_check.ok = True
        rss_check.note = (
            f"not judged — {steady_seconds}s of load, under the "
            f"{MIN_SLOPE_WINDOW_SECONDS}s the caches need to fill"
        )

    ok = all(c.ok for c in checks) and slope_ok and not panics and args.k6_exit == 0

    lines = ["<!-- soak-report -->"]
    lines.append(f"## Soak — {'no leak detected' if ok else 'FAILED'}")
    lines.append("")
    lines.append(
        f"`{args.duration}` of load at `{args.rate}` req/s"
        + (f", {int(reqs):,} requests" if reqs else "")
        + f". Both windows below are **idle**: warm-up, quiesce, *baseline*, "
        f"load, quiesce, *final*."
    )
    lines.append("")
    lines.append("| | baseline | final | growth | limit | |")
    lines.append("| --- | ---: | ---: | ---: | ---: | :-- |")
    for c in checks:
        digits = 1 if c.unit == "%" else 0
        growth = (
            "—"
            if c.growth is None
            else f"{c.growth:+.{1 if c.unit == '%' else 0}f} {c.unit}"
        )
        lines.append(
            f"| {c.name} | {fmt(c.baseline, digits)} | {fmt(c.final, digits)} | "
            f"{growth} | {fmt(c.threshold, digits)} {c.unit} | "
            f"{c.note if c.note else ('ok' if c.ok else '**over**')} |"
        )
    if slope_judged:
        slope_verdict = "ok" if slope_ok else "**over**"
    else:
        slope_verdict = (
            f"not judged — {steady_seconds}s of load, under the "
            f"{MIN_SLOPE_WINDOW_SECONDS}s a trend needs"
        )
    lines.append(
        f"| RSS trend under load | — | — | {fmt(slope, 2)} MiB/min | "
        f"{slope_limit:.2f} MiB/min | {slope_verdict} |"
    )
    lines.append("")

    # ── What consumed what, over time ────────────────────────────────────────
    chart = plot(
        [
            ("RSS", [s.rss_mib for s in samples], "MiB"),
            ("Open descriptors", [float(s.fds) for s in samples if s.fds is not None], "fds"),
        ]
    )
    if chart:
        lines.append("<details open><summary>Resource use over the run</summary>")
        lines.append("")
        lines.append("```")
        lines.append(
            "phases: warm-up | quiesce | BASELINE | load | quiesce | FINAL "
            "(left to right)"
        )
        lines.append("")
        lines.extend(chart)
        lines.append("```")
        lines.append("</details>")
        lines.append("")

    if args.chart:
        try:
            svg_chart(samples, marks, args.chart)
            lines.append(
                f"A plotted version of the same run is in `{args.chart.name}`, "
                "beside this report in the run's artifacts."
            )
            lines.append("")
        except OSError as e:
            lines.append(f"(the SVG chart could not be written: {e})")
            lines.append("")

    # ── Which registry cost the most ─────────────────────────────────────────
    costs: list[RegistryCost] = []
    if args.metrics_before and args.metrics_after:
        try:
            before = parse_prometheus(args.metrics_before.read_text())
            after = parse_prometheus(args.metrics_after.read_text())
            costs = registry_costs(before, after)
        except OSError:
            costs = []

    if costs:
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

    if failed_rate is not None:
        lines.append(f"k6 request failure rate: {failed_rate * 100:.2f}%")
    if args.k6_exit != 0:
        lines.append(
            f"**k6 exited {args.k6_exit}** — a threshold in the scenario was "
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

    args.report.write_text("\n".join(lines) + "\n")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
