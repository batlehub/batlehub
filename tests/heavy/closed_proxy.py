#!/usr/bin/env python3
"""The closed world, as a proxy a client cannot opt out of.

    closed_proxy.py <listen-port>

Every heavy suite closes the world the same way: `HTTP(S)_PROXY` at a port
nothing listens on, `NO_PROXY` exempting the loopback so the tap stays
reachable. That rests on the client honouring `NO_PROXY`, and one does not.
VS Code's server CLI resolves a proxy with this, and nothing else
(`out/server-main.js`, 1.136.2)::

    r.protocol === "http:" ? t.HTTP_PROXY || t.http_proxy || null : …

No `no_proxy` in that path at any version — so `code-server --install-extension`
sent its gallery request for `http://127.0.0.1:<tap>` to the closed port and
reported `connect ECONNREFUSED 127.0.0.1:1`. The world was closed to the client
*and to the instance*, which is not the world the suite is measuring.

This is the same denial, enforced in the proxy instead of in the client: the
loopback is relayed, every other host is refused with `403` before a socket is
opened or a name is resolved. A client that honours `NO_PROXY` never arrives
here for a loopback request and is unaffected; a client that ignores it is
carried to the tap and still cannot reach the internet. The control probe in
§0 asserts that against two real registries on every run, so a mistake here is
a failed run rather than a phase that passes for the wrong reason.

Refusing rather than hanging is deliberate: a closed port gives
`ECONNREFUSED` immediately and a blackhole makes every phase wait out its own
timeout. `403` with a one-line body is the same speed and says which host.
"""

import ipaddress
import select
import socket
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LISTEN_PORT = int(sys.argv[1])

# Names that are the loopback without being an address. Everything else has to
# *be* a loopback address to be relayed — no DNS, so a name cannot resolve its
# way in.
LOOPBACK_NAMES = {"localhost", "localhost.localdomain", "ip6-localhost"}

HOP = {"connection", "keep-alive", "proxy-connection", "te", "trailer", "upgrade"}

RELAY_CHUNK = 64 * 1024


def is_loopback(host):
    host = (host or "").strip().strip("[]").lower()
    if host in LOOPBACK_NAMES:
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


class ClosedProxy(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        """Silence BaseHTTPRequestHandler's own stderr logging."""

    def _refuse(self, host):
        body = f"closed world: {host} is not reachable from here\n".encode()
        self.send_response(403)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_CONNECT(self):
        """The TLS form. Only the Terraform phase's TLS tap is ever tunnelled."""
        host, _, port = self.path.rpartition(":")
        if not is_loopback(host):
            self._refuse(host)
            return
        try:
            upstream = socket.create_connection((host.strip("[]"), int(port or 443)), 30)
        except OSError as e:
            self._refuse(f"{host} ({e})")
            return
        self.send_response(200, "Connection Established")
        self.end_headers()
        self._tunnel(upstream)

    def _tunnel(self, upstream):
        client = self.connection
        try:
            while True:
                ready, _, _ = select.select([client, upstream], [], [], 60)
                if not ready:
                    break
                for src in ready:
                    dst = upstream if src is client else client
                    data = src.recv(RELAY_CHUNK)
                    if not data:
                        return
                    dst.sendall(data)
        except OSError:
            return
        finally:
            upstream.close()
            self.close_connection = True

    def _proxy(self):
        """The plain-HTTP form: an absolute URI in the request line."""
        url = urllib.parse.urlsplit(self.path)
        if not is_loopback(url.hostname):
            self._refuse(url.hostname or self.path)
            return

        body = None
        length = self.headers.get("Content-Length")
        if length:
            body = self.rfile.read(int(length))

        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        target = urllib.parse.urlunsplit(("", "", url.path or "/", url.query, ""))
        import http.client

        conn = http.client.HTTPConnection(
            url.hostname, url.port or 80, timeout=300
        )
        try:
            conn.request(self.command, target, body=body, headers=headers)
            resp = conn.getresponse()
            upstream_length = resp.getheader("Content-Length")
            has_body = self.command != "HEAD" and resp.status not in (204, 304)

            self.send_response(resp.status)
            for k, v in resp.getheaders():
                if k.lower() in HOP or k.lower() in ("content-length", "transfer-encoding"):
                    continue
                self.send_header(k, v)
            if not has_body:
                self.send_header("Content-Length", upstream_length or "0")
            elif upstream_length is not None:
                self.send_header("Content-Length", upstream_length)
            else:
                self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()

            if has_body:
                chunked = upstream_length is None
                while True:
                    chunk = resp.read(RELAY_CHUNK)
                    if not chunk:
                        break
                    if chunked:
                        self.wfile.write(f"{len(chunk):x}\r\n".encode())
                        self.wfile.write(chunk)
                        self.wfile.write(b"\r\n")
                    else:
                        self.wfile.write(chunk)
                if chunked:
                    self.wfile.write(b"0\r\n\r\n")
        finally:
            conn.close()

    do_GET = do_POST = do_PUT = do_HEAD = do_DELETE = do_PATCH = _proxy


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", LISTEN_PORT), ClosedProxy).serve_forever()
