#!/usr/bin/env python3
"""A webhook receiver, for heavy tests that assert on what was delivered.

    webhook_sink.py <log-file> <listen-port>

Writes one line per request — `METHOD PATH <body>` — and answers 200 with an
empty body. The body is written verbatim on one line (a notification payload
is one JSON document, and BatleHub's webhook channel sends it compact), so a
suite greps the log for an event type and parses the line it finds.

The receiver is the oracle for RFC 0014 §4.5: a notification "delivered" is
a request this process wrote down, not a method the server called.
"""

import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG_PATH, LISTEN_PORT = sys.argv[1], int(sys.argv[2])
LOG = open(LOG_PATH, "a", buffering=1)  # noqa: SIM115 — lives for the process


class Sink(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        """Silence BaseHTTPRequestHandler's own stderr logging."""

    def _record(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        LOG.write(
            f"{self.command} {self.path} "
            + body.decode("utf-8", "replace").replace("\n", " ")
            + "\n"
        )
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

    do_GET = do_POST = do_PUT = _record


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", LISTEN_PORT), Sink).serve_forever()
