"""Replay recorded audit-stream lines through the shipped Sigma rules.

`sigma check` proves a rule is well-formed. It cannot prove the rule still
matches what the server writes: rename `event.action` in
`crates/core/src/services/audit_stream.rs` and every rule here is valid and
blind. This script closes that gap (RFC 0036 §6.2):

1. every field a rule reads must be one the stream emits — read from the
   emitter's own source, so the two cannot drift apart;
2. each rule file is run over a recorded stream, and whether it fires must
   match the expectations file next to it.

The matcher implements the subset of Sigma these rules use and nothing more:
selection maps, the `contains` / `startswith` / `endswith` / `gte` / `gt` /
`lte` / `lt` modifiers, `and` / `or` / `not` conditions, and `event_count` /
`value_count` correlations. A rule that needs more fails loudly here rather
than being judged by a matcher that does not understand it.

Usage: uv run --with pyyaml python deploy/siem/replay.py [--stdin --expect-json JSON]
With no arguments it replays `fixtures/`. A recorded stream comes in on stdin
and its expectations inline: the script takes no paths, so it opens nothing
but its own fixtures.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from datetime import datetime, timedelta
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
EMITTER = ROOT / "crates/core/src/services/audit_stream.rs"
RULES = HERE / "sigma"

MODIFIERS = {"contains", "startswith", "endswith", "gte", "gt", "lte", "lt"}


def emitted_fields() -> set[str]:
    """The field names `audit_stream.rs` writes: `"a.b" = …` and `a.b = …`."""
    src = EMITTER.read_text()
    quoted = re.findall(r'^\s+"([a-z_]+(?:\.[a-z_]+)+)" =', src, re.M)
    bare = re.findall(r"^\s+([a-z_]+(?:\.[a-z_]+)+) = ", src, re.M)
    fields = set(quoted) | set(bare)
    if "event.action" not in fields:
        sys.exit(f"replay: found no fields in {EMITTER}; has the emitter moved?")
    return fields


def load_rule_files() -> dict[str, list[dict]]:
    return {
        p.name: [d for d in yaml.safe_load_all(p.read_text()) if d]
        for p in sorted(RULES.glob("*.yml"))
    }


def fields_read(doc: dict) -> set[str]:
    out: set[str] = set()
    for name, sel in (doc.get("detection") or {}).items():
        if name == "condition":
            continue
        for key in sel:
            out.add(key.split("|")[0])
    corr = doc.get("correlation") or {}
    out.update(corr.get("group-by") or [])
    if isinstance(corr.get("condition"), dict) and "field" in corr["condition"]:
        out.add(corr["condition"]["field"])
    return out


# ── matching ──────────────────────────────────────────────────────────────────


def sigma_equals(value, pattern) -> bool:
    """Sigma equality: case-insensitive, `*` and `?` wildcards, `\\*` literal."""
    if isinstance(pattern, (int, float)) and not isinstance(pattern, bool):
        return value == pattern
    text, pat = str(value).lower(), str(pattern).lower()
    # Escaped wildcards are literals: park them, translate the rest, restore.
    pat = pat.replace("\\*", "\x00").replace("\\?", "\x01")
    pat = re.escape(pat).replace(r"\*", ".*").replace(r"\?", ".")
    pat = pat.replace("\x00", r"\*").replace("\x01", r"\?")
    return re.fullmatch(pat, text, re.S) is not None


def field_matches(event: dict, key: str, expected) -> bool:
    field, *mods = key.split("|")
    unknown = set(mods) - MODIFIERS
    if unknown:
        sys.exit(f"replay: modifier(s) {unknown} not implemented by this matcher")
    if field not in event:
        return False
    value = event[field]
    options = expected if isinstance(expected, list) else [expected]

    def one(opt) -> bool:
        if not mods:
            return sigma_equals(value, opt)
        m = mods[0]
        if m in {"gte", "gt", "lte", "lt"}:
            try:
                v = float(value)
            except (TypeError, ValueError):
                return False
            return {"gte": v >= opt, "gt": v > opt, "lte": v <= opt, "lt": v < opt}[m]
        # As Sigma defines them: the modifier wraps the value in wildcards,
        # and an escaped `\*` inside it stays a literal.
        o = str(opt)
        pattern = {"contains": f"*{o}*", "startswith": f"{o}*", "endswith": f"*{o}"}[m]
        return sigma_equals(value, pattern)

    return any(one(o) for o in options)


def detection_matches(detection: dict, event: dict) -> bool:
    results = {
        name: all(field_matches(event, k, v) for k, v in sel.items())
        for name, sel in detection.items()
        if name != "condition"
    }
    cond = detection["condition"]
    tokens = re.findall(r"\(|\)|[A-Za-z_][\w]*", cond)
    expr = []
    for t in tokens:
        if t in {"and", "or", "not", "(", ")"}:
            expr.append(t)
        elif t in results:
            expr.append(str(results[t]))
        else:
            sys.exit(f"replay: condition token {t!r} not implemented by this matcher")
    return eval(" ".join(expr), {"__builtins__": {}})  # noqa: S307 — only True/False/and/or/not


def parse_timespan(span: str) -> timedelta:
    n, unit = int(span[:-1]), span[-1]
    name = {"s": "seconds", "m": "minutes", "h": "hours", "d": "days"}[unit]
    return timedelta(**{name: n})


def compare(n: int, cond: dict) -> bool:
    ops = {"gte": n.__ge__, "gt": n.__gt__, "lte": n.__le__, "lt": n.__lt__, "eq": n.__eq__}
    return all(ops[k](v) for k, v in cond.items() if k != "field")


def correlation_fires(corr: dict, by_name: dict[str, dict], events: list[dict]) -> bool:
    kind = corr["type"]
    if kind not in {"event_count", "value_count"}:
        sys.exit(f"replay: correlation type {kind!r} not implemented by this matcher")
    base = [by_name[r] for r in corr["rules"]]
    hits = [e for e in events if any(detection_matches(b["detection"], e) for b in base)]
    groups: dict[tuple, list[dict]] = defaultdict(list)
    for e in hits:
        groups[tuple(e.get(f) for f in corr.get("group-by", []))].append(e)
    span = parse_timespan(corr["timespan"])
    cond = corr["condition"]
    for members in groups.values():
        members.sort(key=lambda e: e["_ts"])
        for i, first in enumerate(members):
            window = [e for e in members[i:] if e["_ts"] - first["_ts"] <= span]
            n = len(window) if kind == "event_count" else len({e.get(cond["field"]) for e in window})
            if compare(n, cond):
                return True
    return False


def file_fires(docs: list[dict], events: list[dict]) -> bool:
    """A file fires when its *alerting* document does: the correlation if it
    has one, else its single rule. A correlation's base rule is an input, not
    an alert."""
    by_name = {d["name"]: d for d in docs if "name" in d}
    alert = next((d for d in docs if "correlation" in d), docs[-1])
    if "correlation" in alert:
        return correlation_fires(alert["correlation"], by_name, events)
    return any(detection_matches(alert["detection"], e) for e in events)


def load_stream(text: str, source: str) -> list[dict]:
    events = []
    for line in text.splitlines():
        if not line.strip():
            continue
        e = json.loads(line)
        if e.get("event.dataset") != "batlehub.audit":
            continue
        e["_ts"] = datetime.fromisoformat(e["timestamp"].replace("Z", "+00:00"))
        events.append(e)
    if not events:
        sys.exit(f"replay: {source} holds no audit lines")
    return events


def field_failures(files: dict[str, list[dict]]) -> list[str]:
    """Every field a rule reads that the audit stream never emits."""
    emitted = emitted_fields()
    return [
        f"{name}: reads {sorted(missing)}, which the stream never emits"
        for name, docs in files.items()
        for missing in (fields_read(doc) - emitted for doc in docs)
        if missing
    ]


def replay_failures(files: dict[str, list[dict]], events: list[dict],
                    expected: dict[str, bool], expect_name: str) -> list[str]:
    """Replay the stream through each rule file and compare with `expected`."""
    failures = [f"{expect_name} names {name}, which is not a rule file"
                for name in sorted(set(expected) - set(files))]
    for name, docs in files.items():
        if name not in expected:
            continue
        fired = file_fires(docs, events)
        ok = fired == expected[name]
        print(f"  {'ok ' if ok else 'BAD'} {name}: {'fires' if fired else 'quiet'}")
        if not ok:
            failures.append(f"{name}: expected {'to fire' if expected[name] else 'silence'}")
    return failures


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--stdin", action="store_true", help="read the stream from stdin")
    ap.add_argument("--expect-json", help="the expectations, as JSON (required with --stdin)")
    args = ap.parse_args()
    if args.stdin != (args.expect_json is not None):
        ap.error("--stdin and --expect-json go together")

    if args.stdin:
        text, source, expect_raw = sys.stdin.read(), "stdin", args.expect_json
    else:
        text, source = (HERE / "fixtures/stream.jsonl").read_text(), "fixtures/stream.jsonl"
        expect_raw = (HERE / "fixtures/expected.json").read_text()

    files = load_rule_files()
    events = load_stream(text, source)
    expected: dict[str, bool] = json.loads(expect_raw)
    failures = field_failures(files) + replay_failures(files, events, expected, "the expectations")

    if failures:
        print("\nreplay failed:\n  " + "\n  ".join(failures), file=sys.stderr)
        return 1
    print(f"\nreplay: {len(files)} rule files, {len(events)} audit lines, every field emitted")
    return 0


if __name__ == "__main__":
    sys.exit(main())
