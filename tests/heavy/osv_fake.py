#!/usr/bin/env python3
"""The quarantine suite's OSV: one that changes its mind, and one that replays.

    osv_fake.py <flag-file> <listen-port> <purl-substring> [<recordings-dir>]

`POST /v1/query` — the flip. Answers the way api.osv.dev does —
`{"vulns": [...]}` — with one critical advisory for any PURL containing
<purl-substring> **while <flag-file> exists**, and `{}` otherwise. That is the
instrument RFC 0018 phase 4 needs: a version scanned clean, then a database
that has learned something, then a rescan — a flip staged without waiting for
a real advisory to be published against a real package.

`POST /replay/v1/query` — api.osv.dev's own answer for the PURL, recorded
verbatim in <recordings-dir> (`tests/fixtures/supply-chain-jdd/osv/`, the
`answer` of each file), and `{}` for a PURL nothing was recorded for. RFC 0041
made OSV fail closed: a 429 from the live database in CI now holds a version
instead of reading as clean, and an advisory published later against a
package the suite expects served would refuse it. The suite's other
registries ask this path, so what they are judged on is fixed.

Everything else is `{}`: a package the suite did not name is clean here,
whatever the real OSV would say.
"""

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

FLAG, PORT, NEEDLE = sys.argv[1], int(sys.argv[2]), sys.argv[3]
RECORDINGS = {}
if len(sys.argv) > 4:
    for name in os.listdir(sys.argv[4]):
        if name.endswith(".json"):
            with open(os.path.join(sys.argv[4], name), encoding="utf-8") as f:
                rec = json.load(f)
            RECORDINGS[rec["purl"]] = rec["answer"]

ADVISORY = {
    "id": "BATLEHUB-HEAVY-FLIP",
    "summary": "heavy: an advisory the database learned after the first scan",
    "details": "Staged by tests/heavy/osv_fake.py to measure the rescan flip (RFC 0018 phase 4).",
    "aliases": ["CVE-0000-0000"],
    "database_specific": {"severity": "CRITICAL"},
}


class Osv(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        """Silence BaseHTTPRequestHandler's own stderr logging."""

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b"{}"
        try:
            purl = json.loads(body).get("package", {}).get("purl", "")
        except ValueError:
            purl = ""
        if self.path == "/replay/v1/query":
            payload = json.dumps(RECORDINGS.get(purl, {})).encode()
        else:
            vulns = [ADVISORY] if (NEEDLE in purl and os.path.exists(FLAG)) else []
            payload = json.dumps({"vulns": vulns} if vulns else {}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Osv).serve_forever()
