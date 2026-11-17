#!/usr/bin/env python3
"""Seed a running demo (scripts/demo.sh) with traffic every console page reads.

    demo_seed.py <base-url> <work-dir>

Real requests through the real server, nothing written to the database by
hand, so what the console shows is what a deployment would:

  1. First downloads on every registry — each version is held SCAN_PENDING
     and queued; the worker scans it against the replayed OSV.
  2. Wait for the queue to drain.
  3. Second downloads — the held ones are refused (the refusals of Trends and
     Exposure), the clean ones served.
  4. A signed SOC push: hard_block on left-pad@1.3.0, already pulled (so
     Exposure has pulls before the flag, and asking again is refused), and a
     warn flag on minimist@1.2.8, pulled before and after it. A warn flag still
     serves, and only a served download counts, so minimist is the version
     "Needs a decision" lists as downloaded after its flag.
  5. Rejected credentials from bogus tokens (Instance security's sign-ins).
  6. A manual ban of a documentation address (203.0.113.7).
  7. An exemption on minimist@1.2.5 that ends in three days.

Idempotent enough to re-run: a second run adds traffic, never fails on what
the first one created.
"""

import datetime as dt
import hashlib
import hmac
import html.parser
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

BASE, WORK = sys.argv[1].rstrip("/"), sys.argv[2]
ADMIN = "demo-admin-token"
USER = "demo-user-token"
SOC_SECRET = "demo-soc-secret"

NPM = {
    "npm-public": [
        ("event-stream", "3.3.6"),
        ("ua-parser-js", "0.7.29"),
        ("node-ipc", "10.1.1"),
        ("ynodejs_mod_log", "10.0.1"),
        ("minimist", "1.2.5"),
        ("minimist", "1.2.8"),
        ("lodash", "4.17.20"),
        ("left-pad", "1.3.0"),
    ],
    "npm-young": [("lodash", "4.17.21"), ("left-pad", "1.3.0")],
}
PYPI = [("requests", "2.25.0"), ("jinja2", "2.11.2"), ("idna", "3.6"), ("etherreeum", "1.0.0")]
CARGO = [("time", "0.1.43"), ("rsa", "0.9.6"), ("memmap2", "0.5.10")]
GO = [("golang.org/x/net", "v0.22.0"), ("golang.org/x/crypto", "v0.16.0")]


def req(method, path, token=None, body=None, headers=None, timeout=120):
    """(status, bytes); never raises on an HTTP status."""
    url = path if path.startswith("http") else BASE + path
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    r = urllib.request.Request(url, data=data, method=method, headers=dict(headers or {}))
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    if data is not None and "Content-Type" not in r.headers:
        r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except (urllib.error.URLError, TimeoutError) as e:
        return 0, str(e).encode()


def say(msg):
    print(f"  · {msg}", flush=True)


class Links(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.hrefs = []

    def handle_starttag(self, tag, attrs):
        if tag == "a":
            self.hrefs += [v for k, v in attrs if k == "href" and v]


def pypi_file(registry, name, version):
    """The proxied URL of one file of name==version, from the simple index."""
    path = f"/proxy/{registry}/simple/{name}/"
    status, page = req("GET", path, USER, headers={"Accept": "text/html"})
    if status != 200:
        return None
    parser = Links()
    parser.feed(page.decode("utf-8", "replace"))
    stem = f"-{version}"
    files = [h for h in parser.hrefs if stem in urllib.parse.urlparse(h).path.rsplit("/", 1)[-1]]
    files.sort(key=lambda h: (not h.split("#")[0].endswith(".whl"), h))
    return urllib.parse.urljoin(BASE + path, files[0].split("#")[0]) if files else None


def download_round(label):
    """One download of every seeded version; returns {status: count}."""
    seen = {}

    def note(what, status):
        seen[status] = seen.get(status, 0) + 1
        say(f"{label}: {what} → {status or 'no answer'}")

    for registry, versions in NPM.items():
        for name, version in versions:
            status, _ = req("GET", f"/proxy/{registry}/{name}/{version}/tarball", USER)
            note(f"{registry} {name}@{version}", status)
    for name, version in PYPI:
        url = pypi_file("pypi-public", name, version)
        status = req("GET", url, USER)[0] if url else 404
        note(f"pypi-public {name}=={version}", status)
    for name, version in CARGO:
        status, _ = req("GET", f"/proxy/crates-io/{name}/{version}/download", USER)
        note(f"crates-io {name}@{version}", status)
    for module, version in GO:
        status, _ = req("GET", f"/proxy/go-public/{module}/@v/{version}.info", USER)
        note(f"go-public {module}@{version}", status)
    return seen


def wait_for_scans(limit_secs=240):
    """Until no scan job is open, or the limit: a held version stays held."""
    deadline = time.time() + limit_secs
    while time.time() < deadline:
        status, body = req("GET", "/api/v1/admin/scans?per_page=1", ADMIN)
        if status == 200:
            total = json.loads(body).get("total", 0)
            if total == 0:
                say("the scan queue is empty")
                return
            say(f"{total} scan job(s) open, waiting…")
        else:
            say(f"GET /admin/scans answered {status}, waiting…")
        time.sleep(5)
    say("the queue did not drain in time; the overview will show what is left")


def sign(body: bytes) -> str:
    return "sha256=" + hmac.new(SOC_SECRET.encode(), body, hashlib.sha256).hexdigest()


def push_flags():
    flags = [
        {
            "external_id": "DEMO-SOC-1",
            "registry": "npm-public",
            "package_name": "left-pad",
            "version": "1.3.0",
            "kind": "malware",
            "effect": "hard_block",
            "severity": "critical",
            "summary": "Demo: a SOC reports a compromised release",
            "url": "https://example.invalid/soc/demo-1",
        },
        {
            "external_id": "DEMO-SOC-2",
            "registry": "npm-public",
            "package_name": "minimist",
            "version": "1.2.8",
            "kind": "vulnerability",
            "effect": "warn",
            "severity": "medium",
            "summary": "Demo: a SOC asks consumers to plan an upgrade",
            "url": "https://example.invalid/soc/demo-2",
        },
    ]
    body = json.dumps({"flags": flags}, separators=(",", ":")).encode()
    status, answer = req(
        "POST", "/api/v1/flags/soc", body=body, headers={"X-Hub-Signature-256": sign(body)}
    )
    say(f"SOC push (hard_block left-pad@1.3.0, warn minimist@1.2.8) → {status}")
    if status != 200:
        print(answer.decode("utf-8", "replace")[:400], file=sys.stderr)


def main():
    status, _ = req("GET", "/healthz")
    if status != 200:
        sys.exit(f"{BASE}/healthz answered {status}")

    print("1. first downloads (each version is held until it is scanned)", flush=True)
    download_round("first")

    print("2. the worker scans", flush=True)
    wait_for_scans()

    print("3. second downloads (held versions are refused now)", flush=True)
    seen = download_round("second")
    say(f"answers: {seen}")

    print("4. the SOC pushes two flags; both versions are asked for again", flush=True)
    push_flags()
    for _ in range(3):
        status, _ = req("GET", "/proxy/npm-public/minimist/1.2.8/tarball", USER)
    say(f"minimist@1.2.8 (warn flag) asked three times after the flag → {status}")
    status, _ = req("GET", "/proxy/npm-public/left-pad/1.3.0/tarball", USER)
    say(f"left-pad@1.3.0 (hard_block) asked after the flag → {status}")

    print("5. rejected credentials", flush=True)
    for i in range(24):
        req("GET", "/api/v1/me", f"stolen-or-mistyped-{i}")
    say("24 requests with tokens nobody issued")

    print("6. a manual ban", flush=True)
    status, _ = req(
        "POST",
        "/api/v1/admin/ip-blocks",
        ADMIN,
        {"ip": "203.0.113.7", "reason": "Demo: credential stuffing", "duration_secs": 86400},
    )
    say(f"ban 203.0.113.7 → {status}")

    print("7. an exemption ending in three days", flush=True)
    until = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(days=3)).isoformat()
    status, answer = req(
        "PUT",
        "/api/v1/admin/registries/npm-public/policy/version/minimist/1.2.5/rules/security_verdict",
        ADMIN,
        {"exempt_until": until, "reason": "Demo: patched in our fork, upgrade planned"},
    )
    say(f"exempt minimist@1.2.5 → {status}")
    if status >= 400:
        print(answer.decode("utf-8", "replace")[:400], file=sys.stderr)

    print("seeded.", flush=True)


if __name__ == "__main__":
    main()
