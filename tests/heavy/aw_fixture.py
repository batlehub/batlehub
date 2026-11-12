#!/usr/bin/env python3
"""The closed world's own origin for mise auto-wire (RFC 0045 §6.P).

    aw_fixture.py <listen-port> <root-dir> <log-file>

One loopback host plays every upstream the overlay cases need, so none of
them reaches the internet: mise's registry archive (`registry/latest.tar.zst`,
under any prefix the config points `registry_upstream` at), a Forgejo whose
release API serves `acme/deploy`, and the origin a `307` bounce lands on.

A file under <root-dir> answers its own path, the query string ignored, as
`python3 -m http.server` does. What that server cannot do is serve a path
that is both a file and a directory's parent — `releases` and
`releases/latest` — so a directory answers with its `_` file.

Each request is one log line, `<METHOD> <path> auth=<yes|no>`: §6.P case 10
asserts that the bounce reached this origin *without* the instance's
credential, which the `http.server` access log cannot show.
"""

import os
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
ROOT = os.path.realpath(sys.argv[2])
LOG = sys.argv[3]


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 - http.server's naming
        self.answer(body=True)

    def do_HEAD(self):  # noqa: N802
        self.answer(body=False)

    def answer(self, body):
        path = urllib.parse.urlsplit(self.path).path
        with open(LOG, "a", encoding="utf-8") as log:
            auth = "yes" if self.headers.get("Authorization") else "no"
            log.write(f"{self.command} {path} auth={auth}\n")
        file = os.path.realpath(os.path.join(ROOT, urllib.parse.unquote(path).lstrip("/")))
        if os.path.isdir(file):
            file = os.path.join(file, "_")
        if not file.startswith(ROOT + os.sep) or not os.path.isfile(file):
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        with open(file, "rb") as f:
            data = f.read()
        self.send_response(200)
        kind = "application/json" if data[:1] in (b"[", b"{") else "application/octet-stream"
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if body:
            self.wfile.write(data)

    def log_message(self, *_):
        pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
