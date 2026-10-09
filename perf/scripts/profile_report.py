#!/usr/bin/env python3
"""Turn `perf/scripts/profile.sh`'s per-arm profiles into a report.

Reads `<dir>/arms/<op>.folded` (stacks), `<op>.k6.json` (k6's summary) and
`<op>.routes.json` (the routes the router matched), and writes
`<dir>/profile.json` (the numbers, and the next run's baseline) and
`<dir>/profile.md`.

Per arm: CPU per request, the leaf functions the samples land in, and the
nearest *BatleHub* function on each stack — the part of our code that either
burned the CPU or handed it to a dependency, which is the one an engineer can
act on. Across arms: the ranking, a delta against `--baseline`, and the route
inventory check.

Exit status: 1 when the route inventory (`perf/profile_routes.txt`) disagrees
with what the run measured. CPU deltas are reported, never failed on: a shared
CI runner's noise has not been measured yet.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

# Ours: a symbol path in one of our crates, or — for an inlined frame, which has
# no path — the `[crates/…]` / `[server/…]` source tag `profiling.rs` appends.
# The two fixed inputs, from this file's own location rather than the command
# line: the script opens no path it was handed except the run's own output.
REPO = Path(__file__).resolve().parents[2]
ROUTES = REPO / "perf/profile_routes.txt"
SPEC = REPO / "ui/openapi.json"

OWN = re.compile(r"(?:^<?batlehub)|(?:\[(?:crates|server|cli)/)")
HASH = re.compile(r"::h[0-9a-f]{16}$")
METHODS = ("get", "put", "post", "delete", "patch", "head")


def canonical(pattern: str) -> str:
    """`{name:regex}` → `{name}`, then `}/@` → `}@`.

    The same canonical form as `canonical()` in
    `crates/web/tests/authz_matrix.rs`: actix reports a pattern with its
    constraints, the OpenAPI spec without them and with a `/` spliced before
    an `@`, and both have to agree before they are compared."""
    out, i = [], 0
    while i < len(pattern):
        c = pattern[i]
        i += 1
        if c != "{":
            out.append(c)
            continue
        name = []
        while i < len(pattern) and pattern[i] not in ":}":
            name.append(pattern[i])
            i += 1
        if i < len(pattern) and pattern[i] == ":":
            depth = 0
            i += 1
            while i < len(pattern) and not (pattern[i] == "}" and depth == 0):
                depth += {"{": 1, "}": -1}.get(pattern[i], 0)
                i += 1
        i += 1  # the closing brace
        out.append("{" + "".join(name) + "}")
    return "".join(out).replace("}/@", "}@")


def route_key(key: str) -> str:
    method, _, pattern = key.partition(" ")
    return f"{method.upper()} {canonical(pattern)}"


def short(symbol: str) -> str:
    return HASH.sub("", symbol)


def read_folded(path: Path) -> list[tuple[list[str], int]]:
    stacks = []
    if not path.exists():
        return stacks
    for line in path.read_text().splitlines():
        stack, _, count = line.rpartition(" ")
        if stack and count.isdigit():
            stacks.append(([short(f) for f in stack.split(";")], int(count)))
    return stacks


PLUMBING = {"core", "alloc", "std"}


def crate_of(frame: str) -> str:
    """`tokio` for `tokio::x`, `<tokio::x as y>::z` or `poll [tokio-1.47.1/src/…]`."""
    if frame.startswith("<["):  # an impl on a slice or array: core's
        return "core"
    tag = re.search(r"\[([A-Za-z0-9_\-]+?)-\d+\.\d+[^/\]]*/", frame)
    if tag:
        return tag.group(1).replace("-", "_")
    path = re.match(r"^<*(\w+)::", frame)
    return path.group(1) if path else frame.split(" ")[0]


def leaf_crate(frames: list[str]) -> str | None:
    """The crate nearest the leaf, skipping `core`/`alloc`/`std`: an
    `Atomic::load` at the leaf names nothing."""
    for frame in reversed(frames):
        crate = crate_of(frame)
        if crate not in PLUMBING:
            return crate
    return None


def nearest_own(frames: list[str]) -> str:
    """The nearest BatleHub frame to the leaf, and — when the CPU went further
    down, into a dependency — that dependency: `<our frame> → regex_automata`.
    Without the arrow, time spent routing under a middleware would read as the
    middleware being hot."""
    for i in range(len(frames) - 1, -1, -1):
        if OWN.search(frames[i]):
            below = leaf_crate(frames[i + 1:])
            return f"{frames[i]} → {below}" if below else frames[i]
    crate = leaf_crate(frames)
    return f"(outside BatleHub: {crate})" if crate else "(outside BatleHub)"


def arm_numbers(arms: Path, op: str, seconds: int, rate: int, frequency: int) -> dict:
    stacks = read_folded(arms / f"{op}.folded")
    samples = sum(c for _, c in stacks)
    leaf, own = Counter(), Counter()
    for frames, count in stacks:
        leaf[frames[-1]] += count
        own[nearest_own(frames)] += count
    k6 = {}
    if (arms / f"{op}.k6.json").exists():
        k6 = json.loads((arms / f"{op}.k6.json").read_text()).get("metrics", {})
    dropped = int(k6.get("dropped_iterations", {}).get("count", 0))
    routes = {}
    if (arms / f"{op}.routes.json").exists():
        routes = {route_key(k): v for k, v in json.loads((arms / f"{op}.routes.json").read_text()).items()}
    requests = rate * seconds
    cpu_ms = samples / frequency * 1000 / requests if requests else 0.0
    wait = {}
    if (arms / f"{op}.wait.json").exists():
        wait = json.loads((arms / f"{op}.wait.json").read_text())
    return {
        "samples": samples,
        "cpu_ms_per_request": cpu_ms,
        "wall": wall_split(wait, cpu_ms),
        "stages": stage_split(wait),
        "dropped_iterations": dropped,
        "p95_ms": k6.get("http_req_duration", {}).get("p(95)"),
        "top_leaf": leaf.most_common(5),
        "top_own": own.most_common(5),
        "routes": routes,
    }


def stage_split(wait: dict) -> dict[str, float]:
    """Per-request milliseconds in each step a request marked with a
    `batlehub::stage` span (`batlehub_core::services::stage`)."""
    n = wait.get("requests", 0)
    if not n:
        return {}
    return {
        key[len("stage."):-len("_ns")]: value / n / 1e6
        for key, value in wait.items()
        if key.startswith("stage.") and key.endswith("_ns")
    }


def wall_split(wait: dict, cpu_ms: float) -> dict | None:
    """Per-request milliseconds of wall time, split by where it went.

    `polled` is the time the request's future ran on a thread; what of it
    was not CPU was spent blocked inside a poll. The rest of the wall time
    was awaited: SQL, upstream, and everything else. SQL time is sqlx's own
    elapsed per statement, which includes a little encoding done while
    polled, so the parts are approximate and each is clamped at zero."""
    n = wait.get("requests", 0)
    if not n:
        return None
    ms = lambda key: wait.get(key, 0) / n / 1e6  # noqa: E731
    wall, polled, db, upstream = ms("wall_ns"), ms("polled_ns"), ms("db_ns"), ms("upstream_ns")
    return {
        "requests": n,
        "wall_ms": wall,
        "cpu_ms": cpu_ms,
        "blocked_in_poll_ms": max(0.0, polled - cpu_ms),
        "sql_ms": db,
        "sql_statements": wait.get("db_statements", 0) / n,
        "upstream_ms": upstream,
        "other_await_ms": max(0.0, wall - polled - db - upstream),
    }


def spec_operations(spec_path: Path) -> set[str]:
    spec = json.loads(spec_path.read_text())
    return {
        f"{m.upper()} {canonical(path)}"
        for path, item in spec["paths"].items()
        for m in item
        if m in METHODS
    }


def read_inventory(path: Path) -> dict[str, str]:
    inventory = {}
    for line in path.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        route, _, status = line.partition("\t")
        inventory[route_key(route.strip())] = status.strip()
    return inventory


def inventory_problems(inventory: dict[str, str], hit: set[str], spec: set[str], partial: bool) -> list[str]:
    problems = []
    for route in sorted(hit & spec):
        if inventory.get(route, "").startswith("skip"):
            problems.append(f"{route} was profiled by this run but the inventory says "
                            f"'{inventory[route]}' — mark it `profiled`")
    if not partial:
        for route, status in sorted(inventory.items()):
            if status == "profiled" and route not in hit:
                problems.append(f"{route} is marked `profiled` but no arm reached it — "
                                "an arm broke or moved; fix the arm or mark the route skipped")
    return problems


def pct(a: float, b: float) -> str:
    return f"{(a - b) / b * 100:+.0f} %" if b else "new"


def render(arms: dict, args, coverage: dict, problems: list[str], baseline: dict) -> str:
    order = sorted(arms, key=lambda op: arms[op]["cpu_ms_per_request"], reverse=True)
    out = [
        "## CPU profile per request path",
        "",
        f"{len(arms)} arm(s), each alone for {args.seconds} s at {args.rate} req/s, "
        f"sampled at {args.frequency} Hz. CPU time only: waiting on the upstream costs nothing here. "
        f"Report-only — nothing below fails the run except the route inventory.",
        "",
        f"**Routes profiled: {coverage['profiled']} of {coverage['spec']} API operations** "
        f"({coverage['skipped']} listed as skipped in `perf/profile_routes.txt`).",
        "",
        "*p95* is k6's latency for the arm. Read the two together: high CPU is "
        "work to optimise; high p95 with little CPU is a path that waits (database, "
        "lock, I/O), which a CPU profile cannot show and the soak's latency can.",
        "",
        "| arm | CPU ms/req | p95 ms | vs base | hottest BatleHub frame → where the CPU went | share |",
        "| --- | ---: | ---: | ---: | --- | ---: |",
    ]
    for op in order:
        a = arms[op]
        top = a["top_own"][0] if a["top_own"] else ("—", 0)
        share = f"{top[1] / a['samples'] * 100:.0f} %" if a["samples"] else "—"
        base = baseline.get(op, {}).get("cpu_ms_per_request")
        delta = pct(a["cpu_ms_per_request"], base) if base is not None else "—"
        if base and a["cpu_ms_per_request"] > base * 1.2:
            delta += " ⚠"
        flag = " (dropped iterations)" if a["dropped_iterations"] else ""
        p95 = f"{a['p95_ms']:.0f}" if a.get("p95_ms") is not None else "—"
        out.append(f"| `{op}`{flag} | {a['cpu_ms_per_request']:.2f} | {p95} | {delta} | `{top[0][:140]}` | {share} |")

    out += ["", "### Where the wall time goes", "",
            "Server-side, per request, in ms. A CPU profile sees only the first column; the others are "
            "what it cannot: *blocked in a poll* is a future that held its thread without computing "
            "(a `std` lock, synchronous I/O — blocking inside async code), *SQL* and *upstream* are "
            "awaited I/O, and *other awaits* is the rest — tokio locks and semaphores, channels, "
            "`spawn_blocking` (all of `tokio::fs`). Approximate: the parts overlap a little and are "
            "clamped at zero.", "",
            "| arm | wall | CPU | blocked in a poll | SQL (stmts) | upstream | other awaits |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for op in sorted(arms, key=lambda o: (arms[o]["wall"] or {}).get("wall_ms", 0), reverse=True):
        w = arms[op]["wall"]
        if not w:
            out.append(f"| `{op}` | — | | | | | |")
            continue
        out.append(f"| `{op}` | {w['wall_ms']:.2f} | {w['cpu_ms']:.2f} | {w['blocked_in_poll_ms']:.2f} | "
                   f"{w['sql_ms']:.2f} ({w['sql_statements']:.1f}) | {w['upstream_ms']:.2f} | "
                   f"{w['other_await_ms']:.2f} |")

    staged = [op for op in arms if arms[op].get("stages")]
    if staged:
        out += ["", "### Where the steps go", "",
                "Per request, in ms, for the paths that mark their steps with a `batlehub::stage` span. "
                "Steps can nest and can run outside the request's own time, so they need not sum to "
                "its wall time; the largest is the one to read first.", ""]
        for op in staged:
            steps = sorted(arms[op]["stages"].items(), key=lambda kv: kv[1], reverse=True)
            out.append(f"**`{op}`**: " + ", ".join(f"`{name}` {ms:.2f}" for name, ms in steps))
            out.append("")

    total = Counter()
    for a in arms.values():
        for fn, n in a["top_own"]:
            total[fn] += n
    all_samples = sum(a["samples"] for a in arms.values()) or 1
    out += ["", "### Where the CPU goes, all paths", "",
            "Each sample is credited to the nearest BatleHub frame on its stack, and to the dependency "
            "below it when the CPU went further down (`→ sha2`). Every arm ran the same length at the "
            "same rate, so the sum compares paths on equal terms.", "", "```"]
    for fn, n in total.most_common(15):
        out.append(f"{n / all_samples * 100:5.1f} %  {fn[:170]}")
    out += ["```", "", "<details><summary>Per arm: leaf functions and routes</summary>", ""]
    for op in order:
        a = arms[op]
        out.append(f"**`{op}`** — {a['samples']} samples")
        out.append("")
        for fn, n in a["top_leaf"][:3]:
            out.append(f"- leaf `{fn[:100]}` {n / (a['samples'] or 1) * 100:.0f} %")
        for route, n in sorted(a["routes"].items()):
            out.append(f"- route `{route}` × {n}")
        out.append("")
    out.append("</details>")
    if problems:
        out += ["", "### ❌ Route inventory disagrees with this run", ""] + [f"- {p}" for p in problems]
    if coverage["outside_spec"]:
        out += ["", "<sub>Matched routes absent from the OpenAPI spec (not counted): "
                + ", ".join(f"`{r}`" for r in sorted(coverage["outside_spec"])) + "</sub>"]
    out += ["", "Flamegraphs (`arms/*.svg`) and folded stacks (`arms/*.folded`, open in "
            "[speedscope](https://www.speedscope.app)) are in the run's artifacts."]
    return "\n".join(out) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", type=Path, required=True)
    ap.add_argument("--seconds", type=int, required=True)
    ap.add_argument("--rate", type=int, required=True)
    ap.add_argument("--frequency", type=int, required=True)
    ap.add_argument("--baseline", type=Path)
    ap.add_argument("--partial", action="store_true",
                    help="only some arms ran: do not require every `profiled` route to be reached")
    args = ap.parse_args()

    arms_dir = args.dir / "arms"
    ops = sorted(p.name[: -len(".k6.json")] for p in arms_dir.glob("*.k6.json"))
    arms = {op: arm_numbers(arms_dir, op, args.seconds, args.rate, args.frequency) for op in ops}

    spec = spec_operations(SPEC)
    inventory = read_inventory(ROUTES)
    hit = {r for a in arms.values() for r in a["routes"]}
    problems = inventory_problems(inventory, hit, spec, args.partial)
    unmatched = [op for op, a in arms.items() if any("<unmatched>" in r for r in a["routes"])]
    problems += [f"arm `{op}` sent requests no route matched" for op in unmatched]
    coverage = {
        "spec": len(spec),
        "profiled": len(hit & spec),
        "skipped": sum(1 for s in inventory.values() if s.startswith("skip")),
        "outside_spec": {r for r in hit - spec if "<unmatched>" not in r},
    }
    baseline = {}
    if args.baseline:
        baseline = json.loads(args.baseline.read_text()).get("arms", {})

    (args.dir / "profile.json").write_text(json.dumps(
        {"seconds": args.seconds, "rate": args.rate, "frequency": args.frequency,
         "coverage": {**coverage, "outside_spec": sorted(coverage["outside_spec"])},
         "arms": arms}, indent=1))
    (args.dir / "profile.md").write_text(render(arms, args, coverage, problems, baseline))
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
