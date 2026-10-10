#!/usr/bin/env python3
"""Database calls per request on the critical paths, measured on a real server.

Driven by tests/heavy/db_calls.sh, which starts BatleHub with
`RUST_LOG=sqlx::query=debug`: sqlx then logs every statement it executes, with
the request span (`request_id=…`) around it when it ran inside a handler. This
script sends one request at a time, waits for the log to go quiet, and counts
the statements that landed in between — so a query from a task the handler
spawned is counted too, and reported apart as `spawned`.

    db_calls.py mock <port>                           # the npm upstream
    db_calls.py run <base> <server.log> <budget.json> [--update]
    db_calls.py idle <server.log>                     # the scan worker, no traffic

The budget holds, per scenario, the statement count of the first request
(`cold`) and the highest of the repeats (`warm`). A count above budget fails
the run; one below it is reported so the budget can be tightened with
`--update` — the budget only ever records what a run measured.
"""

import base64
import gzip
import hashlib
import io
import json
import os
import re
import sys
import tarfile
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPEAT = 3
ADMIN = "heavy-admin-token"


# ── The upstream ─────────────────────────────────────────────────────────────


def tgz(name, version):
    """Byte-identical on every call: the packument advertises its sha1, and a
    gzip header stamped with the current time would fail the proxy's integrity
    check on every fetch — a purge per request that is the harness's own."""
    buf = io.BytesIO()
    with gzip.GzipFile(fileobj=buf, mode="wb", mtime=0) as gz, tarfile.open(fileobj=gz, mode="w") as tar:
        data = json.dumps({"name": name, "version": version}).encode()
        info = tarfile.TarInfo("package/package.json")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


class Upstream(BaseHTTPRequestHandler):
    def do_GET(self):
        host = self.headers["Host"]
        parts = self.path.strip("/").split("/")  # npm/<name>[/-/<file>]
        if len(parts) == 2:
            name = parts[1]
            body = json.dumps({
                "name": name,
                "dist-tags": {"latest": "1.0.0"},
                "versions": {"1.0.0": {
                    "name": name, "version": "1.0.0",
                    "dist": {
                        "tarball": f"http://{host}/npm/{name}/-/{name}-1.0.0.tgz",
                        "shasum": hashlib.sha1(tgz(name, "1.0.0")).hexdigest(),
                    },
                }},
                "time": {"1.0.0": "2024-01-01T00:00:00.000Z"},
            }).encode()
            ctype = "application/json"
        elif len(parts) == 4:
            body, ctype = tgz(parts[1], "1.0.0"), "application/octet-stream"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


# ── The log ──────────────────────────────────────────────────────────────────

LINE = re.compile(r"sqlx::query: summary=\"((?:[^\"\\]|\\.)*)\" db\.statement=\"((?:[^\"\\]|\\.)*)\"")
ELAPSED = re.compile(r"elapsed_secs=([0-9.e-]+)")
REQUEST = re.compile(r"request_id=([0-9a-fA-F-]+)")


def unescape(s):
    return s.encode().decode("unicode_escape")


def parse(text):
    out = []
    for line in text.splitlines():
        m = LINE.search(line)
        if not m:
            continue
        sql = unescape(m.group(2)).strip() or unescape(m.group(1))
        e = ELAPSED.search(line)
        out.append({
            # Collapsed for grouping and display; `raw` keeps the newlines a
            # `--` comment ends at, for EXPLAIN.
            "sql": " ".join(sql.split()),
            "raw": sql,
            "secs": float(e.group(1)) if e else 0.0,
            "spawned": REQUEST.search(line.split("sqlx::query:")[0]) is None,
        })
    return out


def settle(log, quiet=0.4, cap=5.0):
    """Wait until the log stops growing: spawned work has finished writing."""
    deadline, last, since = time.time() + cap, -1, time.time()
    while time.time() < deadline:
        size = os.path.getsize(log)
        if size != last:
            last, since = size, time.time()
        elif time.time() - since >= quiet:
            break
        time.sleep(0.05)
    return last


# ── The scenarios ────────────────────────────────────────────────────────────


def call(base, method, path, token=None, body=None):
    req = urllib.request.Request(base + path, method=method, data=body)
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    if body is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            r.read()
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def publish_body(name, version):
    data = tgz(name, version)
    return json.dumps({
        "name": name,
        "dist-tags": {"latest": version},
        "versions": {version: {
            "name": name, "version": version,
            "dist": {"shasum": hashlib.sha1(data).hexdigest()},
        }},
        "_attachments": {f"{name}-{version}.tgz": {
            "content_type": "application/octet-stream",
            "data": base64.b64encode(data).decode(),
            "length": len(data),
        }},
    }).encode()


def mint_pat():
    """A PAT, written straight to the table: minting one takes an OIDC session."""
    import psycopg

    raw = "bh_pat_" + uuid.uuid4().hex * 2
    with psycopg.connect(os.environ["DATABASE_URL"], autocommit=True) as c:
        c.execute(
            "INSERT INTO user_tokens (id, user_id, provider, name, token_hash, role,"
            " expires_at, created_at, groups) VALUES (%s, 'db-calls', 'heavy', %s, %s,"
            " 'user', NOW() + interval '1 day', NOW(), '{}')",
            (uuid.uuid4(), "db-calls-" + raw[-8:], hashlib.sha256(raw.encode()).hexdigest()),
        )
    return raw


def scenarios(run):
    proxy, local = f"npm-proxy-{run}", f"npm-local-{run}"
    pat = mint_pat()
    counter = iter(range(1, 1000))

    def publish():
        v = f"1.0.{next(counter)}"
        return ("PUT", f"/proxy/{local}/pkg-{run}", ADMIN, publish_body(f"pkg-{run}", v))

    # (name, request factory, expected status). Order matters: the local reads
    # need the publish before them, the proxy tarball the packument.
    return [
        ("healthz", lambda: ("GET", "/healthz", None, None), 200),
        ("proxy-packument-anon", lambda: ("GET", f"/proxy/{proxy}/left-pad", None, None), 200),
        ("proxy-tarball-anon", lambda: ("GET", f"/proxy/{proxy}/left-pad/1.0.0/tarball", None, None), 200),
        ("proxy-packument-static-token", lambda: ("GET", f"/proxy/{proxy}/left-pad", ADMIN, None), 200),
        ("proxy-packument-pat", lambda: ("GET", f"/proxy/{proxy}/left-pad", pat, None), 200),
        ("proxy-tarball-pat", lambda: ("GET", f"/proxy/{proxy}/left-pad/1.0.0/tarball", pat, None), 200),
        ("local-publish", publish, 200),
        ("local-packument", lambda: ("GET", f"/proxy/{local}/pkg-{run}", None, None), 200),
        ("local-tarball", lambda: ("GET", f"/proxy/{local}/pkg-{run}/1.0.1/tarball", None, None), 200),
        ("api-explore-list", lambda: ("GET", "/api/v1/explore/packages", ADMIN, None), 200),
        ("api-explore-detail", lambda: ("GET", f"/api/v1/explore/packages/{local}/pkg-{run}", ADMIN, None), 200),
        ("api-packages-admin", lambda: ("GET", "/api/v1/packages", ADMIN, None), 200),
        ("api-me", lambda: ("GET", "/api/v1/me", ADMIN, None), 200),
    ]


def measure(base, log, factory, expect):
    rows = []
    for _ in range(REPEAT):
        start = settle(log)
        method, path, token, body = factory()
        status = call(base, method, path, token, body)
        end = settle(log)
        with open(log, "rb") as f:
            f.seek(start)
            queries = parse(f.read(end - start).decode("utf-8", "replace"))
        rows.append({"status": status, "queries": queries})
    bad = [r["status"] for r in rows if r["status"] not in (expect, 201)]
    return rows, bad


# Transaction control is reported, not budgeted: the statements a transaction
# wraps are each counted, and the logged COMMIT count on the cache-miss path
# varied by one between identical runs whose every other statement matched.
CONTROL = ("BEGIN", "COMMIT", "ROLLBACK")


def is_control(q):
    return q["sql"].split(" ", 1)[0].upper() in CONTROL


def report(name, rows):
    counts = [sum(not is_control(q) for q in r["queries"]) for r in rows]
    control = [sum(is_control(q) for q in r["queries"]) for r in rows]
    spawned = [sum(q["spawned"] for q in r["queries"]) for r in rows]
    ms = [sum(q["secs"] for q in r["queries"]) * 1000 for r in rows]
    print(f"\n── {name}: statements {counts} (spawned {spawned}, transaction control {control}), db time "
          + " / ".join(f"{m:.1f}ms" for m in ms))
    for i, r in enumerate(rows):
        seen = {}
        for q in r["queries"]:
            seen[q["sql"]] = seen.get(q["sql"], 0) + 1
        if i in (0, len(rows) - 1):
            print(f"   [{'cold' if i == 0 else 'warm'}]")
            for sql, n in seen.items():
                flag = f" ×{n}  <-- repeated" if n > 1 else ""
                print(f"     {sql[:150]}{flag}")
    return {"cold": counts[0], "warm": max(counts[1:])}


# ── Plans ──────────────────────────────────────────────────────────────────


def seq_scans(plan):
    """Every relation a plan reads with a sequential scan."""
    found = []
    if plan.get("Node Type") == "Seq Scan":
        found.append(plan.get("Relation Name", "?"))
    for child in plan.get("Plans", []):
        found += seq_scans(child)
    return found


def explain(statements):
    """Plan each distinct `(collapsed, raw)` statement as logged, `$n` and all.

    `GENERIC_PLAN` (PostgreSQL 16+) plans a statement without its parameters,
    and `enable_seqscan = off` makes the planner take any usable index however
    small the table — so a `Seq Scan` left in the plan is a statement with *no*
    index to use, which is what a production-sized table would pay for, and a
    test database's handful of rows cannot hide.
    """
    import psycopg

    scans, unplanned = {}, []
    with psycopg.connect(os.environ["DATABASE_URL"], autocommit=True) as c:
        c.execute("SET enable_seqscan = off")
        for sql, raw in sorted(statements):
            if sql.split(" ", 1)[0].upper() not in ("SELECT", "WITH", "INSERT", "UPDATE", "DELETE"):
                continue
            try:
                (doc,) = c.execute(f"EXPLAIN (GENERIC_PLAN, FORMAT JSON) {raw}").fetchone()
            except psycopg.Error as e:
                unplanned.append(f"{sql[:110]}  ({str(e).splitlines()[0]})")
                continue
            for rel in seq_scans(doc[0]["Plan"]):
                scans.setdefault(rel, []).append(sql)
    return scans, unplanned


def metrics_seen(base):
    """The per-request histogram `db_metrics` exports, by route."""
    with urllib.request.urlopen(base + "/metrics", timeout=30) as r:
        text = r.read().decode()
    return re.findall(r'batlehub_db_statements_per_request_count\{route="([^"]*)"\}', text)


def run(base, log, budget_path, update):
    run_id = os.environ["HEAVY_RUN"]
    budget = json.load(open(budget_path)) if os.path.exists(budget_path) else {}
    measured, failures, slack, statements = {}, [], [], set()
    for name, factory, expect in scenarios(run_id):
        rows, bad = measure(base, log, factory, expect)
        statements.update((q["sql"], q["raw"]) for r in rows for q in r["queries"])
        if bad:
            failures.append(f"{name}: answered {bad}, expected {expect} — the count measured an error path")
        got = measured[name] = report(name, rows)
        want = budget.get(name)
        for phase in ("cold", "warm"):
            if want is None:
                continue
            if got[phase] > want[phase]:
                failures.append(f"{name} [{phase}]: {got[phase]} statements, budget {want[phase]}")
            elif got[phase] < want[phase]:
                slack.append(f"{name} [{phase}]: {got[phase]} < budget {want[phase]}")

    print("\n── Summary (statements per request)")
    print(f"   {'scenario':32} {'cold':>5} {'warm':>5}   budget")
    for name, got in measured.items():
        want = budget.get(name, {})
        print(f"   {name:32} {got['cold']:>5} {got['warm']:>5}   {want.get('cold', '-')}/{want.get('warm', '-')}")

    scans, unplanned = explain(statements)
    print(f"\n── Plans: {len(statements)} distinct statements, {len(unplanned)} not plannable generically")
    for u in unplanned:
        print(f"   unplanned: {u}")
    allowed = budget.get("seq_scans", {})
    for rel, sqls in sorted(scans.items()):
        why = allowed.get(rel)
        print(f"   Seq Scan on {rel}{' — allowed: ' + why if why else ''}")
        for sql in sqls:
            print(f"     {sql[:150]}")
        if why is None and not update:
            failures.append(f"Seq Scan on {rel} with no usable index — index it, or allow it in seq_scans with the reason")

    routes = metrics_seen(base)
    print(f"\n── /metrics: batlehub_db_statements_per_request for {len(routes)} routes")
    if not any(r.startswith("/proxy/") for r in routes):
        failures.append("/metrics carries no batlehub_db_statements_per_request for a /proxy/ route")

    if update:
        measured["seq_scans"] = {rel: allowed.get(rel, "TODO: why this scan is acceptable") for rel in scans}
        with open(budget_path, "w") as f:
            json.dump(measured, f, indent=2)
            f.write("\n")
        print(f"\nbudget written to {budget_path}")
        return 0
    missing = [n for n in measured if n not in budget and n != "seq_scans"]
    if missing:
        failures.append(f"no budget for {missing} — run with --update")
    for s in slack:
        print(f"   below budget, tighten with --update: {s}")
    for f in failures:
        print(f"FAIL: {f}", file=sys.stderr)
    return 1 if failures else 0


# An idle scan worker, per minute: one lease per 16 s poll once the backoff
# has ramped (4), and a heartbeat, a queue count and an exhausted sweep every
# 30 s (6). The worker it replaced polled every 2 s with five statements — 150
# a minute. A fixed ceiling rather than a budget entry: the count depends on
# where the window falls against the two clocks, by a statement or two.
IDLE_CEILING = 14
IDLE_WARMUP, IDLE_WINDOW = 40, 60


def idle(log):
    time.sleep(IDLE_WARMUP)  # 2 + 4 + 8 + 16 s: the backoff reaches its ceiling
    start = os.path.getsize(log)
    time.sleep(IDLE_WINDOW)
    with open(log, "rb") as f:
        f.seek(start)
        queries = parse(f.read().decode("utf-8", "replace"))
    seen = {}
    for q in queries:
        seen[q["sql"][:110]] = seen.get(q["sql"][:110], 0) + 1
    print(f"\n── idle scan worker: {len(queries)} statements in {IDLE_WINDOW}s (ceiling {IDLE_CEILING})")
    for sql, n in sorted(seen.items(), key=lambda kv: -kv[1]):
        print(f"   ×{n:<3} {sql}")
    if len(queries) > IDLE_CEILING:
        print(f"FAIL: an idle worker ran {len(queries)} statements a minute, ceiling {IDLE_CEILING}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    if sys.argv[1] == "mock":
        ThreadingHTTPServer(("127.0.0.1", int(sys.argv[2])), Upstream).serve_forever()
    elif sys.argv[1] == "idle":
        sys.exit(idle(sys.argv[2]))
    else:
        sys.exit(run(sys.argv[2], sys.argv[3], sys.argv[4], "--update" in sys.argv))
