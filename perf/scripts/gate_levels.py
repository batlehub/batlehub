#!/usr/bin/env python3
"""What each level of security gating costs a request (perf/scripts/gate_levels.sh).

    gate_levels.py layer <level>                a config layer over perf/config.soak.toml
    gate_levels.py report <out-dir>             the comparison, from each level's profile.json

A level is a `--config` layer adding rules to every registry of the base
(layered configs merge `[[registries]]` on name then type). Every rule is set
to *allow* the soak's traffic — the mock upstream's releases are from 2024 or
undated — so what is measured is the cost of evaluating the gate on a
request that passes, not of a refusal:

  none    no rule
  min     release_age_gate (1 h)
  medium  min + cve_gate (block from high)
  high    the verdict gate, [registries.security] in warn mode (osv), + cve_gate
  max     high + license_gate, version_gate, require_signed_release, an SBOM per artifact

What it does not measure: the *hold*. A release_age_gate holds a version
younger than its age, and a `block` security profile holds an unscanned one
until the scan worker answers — minutes to days of wall time on the first
download, by design, which no per-request number shows.
"""

import json
import sys
import tomllib
from pathlib import Path

LEVELS = ["none", "min", "medium", "high", "max"]

# devfile's mock versions are undated and stayed held in the measurement,
# so a security profile there would measure the hold, not the
# gate. (The kinds osv does not cover at all used to be listed here too: a
# required scanner that cannot cover a kind held every version for good.
# Fixed in PR #297, `scan_worker::run_job` now counts it as answered.)
NO_SECURITY = {"devfile"}

AGE = """[[registries.rules]]
kind = "release_age_gate"
min_age_secs = 3600
deny_missing_timestamp = false
"""
CVE = """[[registries.rules]]
kind = "cve_gate"
min_severity = "high"
block = true
"""
SECURITY = """[registries.security]
mode = "warn"
min_age_secs = 3600
mature_age_secs = 3600
hold_missing_timestamp = false
scanners = ["osv"]
required_scanners = ["osv"]
max_severity = "high"
"""
SIGNED = """[[registries.rules]]
kind = "require_signed_release"
enabled = true
deny_missing_signature = false

"""
# The mock's openvsx and vscode extensions say "unsigned", which this rule
# refuses whatever deny_missing_signature says.
NO_SIGNED = {"openvsx", "vscode-marketplace"}
MAX_EXTRA = """[[registries.rules]]
kind = "license_gate"
allow_unknown = true
deny = ["SSPL-1.0"]
block = true

[[registries.rules]]
kind = "version_gate"
block = ["0.0.0-never"]

[registries.sbom]
enabled = true
formats = ["spdx", "cyclonedx"]
fetch_upstream = false
"""


# The profile harness's config: the only base a layer is written over.
BASE = Path(__file__).resolve().parent.parent / "config.soak.toml"


def layer(level: str) -> str:
    if level not in LEVELS:
        sys.exit(f"unknown level {level!r}; one of {', '.join(LEVELS)}")
    body = {
        "none": "",
        "min": AGE,
        "medium": AGE + "\n" + CVE,
        "high": CVE + "\n" + SECURITY,
        "max": CVE + "\n" + SIGNED + MAX_EXTRA + "\n" + SECURITY,
    }[level]
    out = [f"# gate level: {level} (perf/scripts/gate_levels.py)\n"]
    if not body:
        return "".join(out)
    for reg in tomllib.loads(BASE.read_text()).get("registries", []):
        b = body
        if reg["type"] in NO_SECURITY:
            b = b.replace(SECURITY, "")
        if reg["type"] in NO_SIGNED:
            b = b.replace(SIGNED, "")
        out.append(f'\n[[registries]]\nname = "{reg["name"]}"\ntype = "{reg["type"]}"\n\n{b}')
    return "".join(out)


def median_ms(level_dir: Path, op: str):
    """k6's median latency for one arm: steadier than the sampled CPU on a
    shared machine, where a neighbour's load moves the CPU column by 5x."""
    f = level_dir / "arms" / f"{op}.k6.json"
    if not f.exists():
        return None
    return json.loads(f.read_text())["metrics"]["http_req_duration"]["med"]


def report(out: Path) -> str:
    runs = {}
    for level in [*LEVELS, "none-again"]:
        f = out / level / "profile.json"
        if f.exists():
            runs[level] = json.loads(f.read_text())["arms"]
    if "none" not in runs:
        sys.exit(f"no {out}/none/profile.json: the baseline level did not run")
    arms = sorted(runs["none"])
    levels = [lv for lv in [*LEVELS, "none-again"] if lv in runs]
    rows = [
        "| arm | " + " | ".join(levels) + " |",
        "| --- | " + " | ".join("---:" for _ in levels) + " |",
    ]
    for op in arms:
        base = runs["none"][op]
        cells = []
        for lv in levels:
            a = runs[lv].get(op)
            if not a:
                cells.append("—")
                continue
            med, b = median_ms(out / lv, op), median_ms(out / "none", op)
            if med is None:
                cells.append("—")
                continue
            delta = "" if lv == "none" or b is None else f" ({med - b:+.2f})"
            cells.append(f"{med:.2f}{delta} · cpu {a['cpu_ms_per_request']:.2f}")
        rows.append(f"| `{op}` | " + " | ".join(cells) + " |")
    holds = []
    for lv in levels:
        f = out / lv / "preflight_wait_secs"
        holds.append(f.read_text().strip() + " s" if f.exists() else "—")
    rows.append("| first sight, every arm served after | " + " | ".join(holds) + " |")
    return (
        "# Gate levels — median latency ms (vs none) · sampled CPU ms per request\n\n"
        + "\n".join(rows)
        + "\n\nLatency is k6's median at a constant rate, on the soak's mock upstream; "
        "CPU is this process's, sampled. A level's cost is its difference from `none`; "
        "`none-again` ran last, and its gap to `none` is the noise.\n"
    )


def main() -> None:
    if len(sys.argv) == 3 and sys.argv[1] == "layer":
        sys.stdout.write(layer(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "report":
        sys.stdout.write(report(Path(sys.argv[2])))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
