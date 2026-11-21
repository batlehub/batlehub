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
  8. The admin side: a banner, a webhook subscription (before any publish, so
     deliveries reach the sink), a team namespace, a beta-channel member.
  9. Publishes by the demo user to one local registry per ecosystem (npm,
     cargo, pypi, nuget, maven, composer, rubygems), real archives with a
     README and a licence, so README and SBOM have something to show; a
     pre-release on npm, and a publish into a namespace whose grants run in
     shadow.
 10. Downloads of what was published, a block, a bulk block, a refused request
     on a closed registry, a signed inbound event, an upstream nobody answers,
     scans that fail, a blocked user, a warming run, a release import and a
     config change.

Idempotent enough to re-run: a second run adds traffic, never fails on what
the first one created.
"""

import base64
import datetime as dt
import hashlib
import hmac
import html.parser
import gzip
import io
import tarfile
import zipfile
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
INBOUND_SECRET = "demo-inbound-secret"
DESCRIPTION = "Demo greeter"
OCTET_STREAM = "application/octet-stream"
README_MD = "README.md"

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
    # `has_header`, not `in r.headers`: urllib stores the name as "Content-type",
    # so the membership test missed a caller's multipart type and replaced it.
    if data is not None and not r.has_header("Content-type"):
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


def sign(body: bytes, secret: str = SOC_SECRET) -> dict:
    """The two headers of a flag push or an inbound event: HMAC of "{timestamp}.{body}"."""
    ts = str(int(time.time()))
    mac = hmac.new(secret.encode(), ts.encode() + b"." + body, hashlib.sha256).hexdigest()
    return {"X-BatleHub-Timestamp": ts, "X-Hub-Signature-256": "sha256=" + mac}


# ── Archives a real client would upload ───────────────────────────────────────
README = """# {name}

A package published by the BatleHub demo seed, so the console has a README,
an SBOM and a licence to show.

```
greet("world")  # -> "Hello, world!"
```
"""


def _tar(files: dict, gz=True) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz" if gz else "w", format=tarfile.USTAR_FORMAT) as t:
        for path, data in files.items():
            data = data.encode() if isinstance(data, str) else data
            info = tarfile.TarInfo(path)
            info.size, info.mode, info.mtime = len(data), 0o644, 1_700_000_000
            t.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def _zip(files: dict) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for path, data in files.items():
            z.writestr(path, data)
    return buf.getvalue()


def _multipart(fields: dict, file_field: str, filename: str, payload: bytes):
    boundary = "demoseedboundary"
    out = b""
    for k, v in fields.items():
        out += f'--{boundary}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode()
    out += (
        f'--{boundary}\r\nContent-Disposition: form-data; name="{file_field}"; filename="{filename}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode()
    out += payload + f"\r\n--{boundary}--\r\n".encode()
    return out, f"multipart/form-data; boundary={boundary}"


def publish_npm(registry, name, version, tag="latest", token=None):
    pkg = {"name": name, "version": version, "description": DESCRIPTION, "license": "MIT", "main": "index.js"}
    tgz = _tar({
        "package/package.json": json.dumps(pkg),
        "package/README.md": README.format(name=name),
        "package/index.js": "exports.greet = (w) => `Hello, ${w}!`;\n",
        "package/LICENSE": "MIT License\n",
    })
    file = f"{name.split('/')[-1]}-{version}.tgz"
    doc = {
        "name": name,
        "readme": README.format(name=name),
        "dist-tags": {tag: version},
        "versions": {version: {**pkg, "readme": README.format(name=name), "dist": {
            "integrity": "sha512-" + base64.b64encode(hashlib.sha512(tgz).digest()).decode(),
            "tarball": f"{BASE}/proxy/{registry}/{name}/-/{file}",
        }}},
        "_attachments": {file: {"content_type": OCTET_STREAM,
                                "data": base64.b64encode(tgz).decode(), "length": len(tgz)}},
    }
    path = f"/proxy/{registry}/{urllib.parse.quote(name, safe='@')}"
    return req("PUT", path, token or USER, doc)[0]


def publish_cargo(name, version):
    toml = f'[package]\nname = "{name}"\nversion = "{version}"\nedition = "2021"\nlicense = "MIT"\ndescription = "Demo greeter"\nreadme = "README.md"\n'
    crate = _tar({
        f"{name}-{version}/Cargo.toml": toml,
        f"{name}-{version}/README.md": README.format(name=name),
        f"{name}-{version}/src/lib.rs": "pub fn greet(w: &str) -> String { format!(\"Hello, {w}!\") }\n",
    })
    meta = json.dumps({
        "name": name, "vers": version, "deps": [], "features": {}, "authors": ["demo"],
        "description": DESCRIPTION, "documentation": None, "homepage": None,
        "readme": README.format(name=name), "readme_file": README_MD, "keywords": ["demo"],
        "categories": [], "license": "MIT", "license_file": None, "repository": None,
        "badges": {}, "links": None,
    }).encode()
    body = len(meta).to_bytes(4, "little") + meta + len(crate).to_bytes(4, "little") + crate
    return req("PUT", "/proxy/cargo-local/api/v1/crates/new", USER, body,
               {"Content-Type": OCTET_STREAM})[0]


def publish_pypi(name, version):
    stem = f"{name.replace('-', '_')}-{version}"
    pkg_info = (f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\nSummary: Demo greeter\n"
                f"License: MIT\nDescription-Content-Type: text/markdown\n\n{README.format(name=name)}")
    sdist = _tar({f"{stem}/PKG-INFO": pkg_info, f"{stem}/README.md": README.format(name=name),
                  f"{stem}/{name.replace('-', '_')}/__init__.py": "def greet(w):\n    return f'Hello, {w}!'\n"})
    body, ct = _multipart({":action": "file_upload", "name": name, "version": version,
                           "summary": DESCRIPTION, "license": "MIT",
                           "description": README.format(name=name),
                           "description_content_type": "text/markdown",
                           "sha256_digest": hashlib.sha256(sdist).hexdigest()},
                          "content", f"{stem}.tar.gz", sdist)
    return req("POST", "/proxy/pypi-local/legacy/", USER, body, {"Content-Type": ct})[0]


def publish_nuget(pid, version):
    nuspec = f"""<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{pid}</id><version>{version}</version><authors>demo</authors>
    <description>Demo greeter</description><license type="expression">MIT</license>
    <readme>README.md</readme><tags>demo</tags>
  </metadata>
</package>"""
    nupkg = _zip({f"{pid}.nuspec": nuspec, README_MD: README.format(name=pid), "lib/netstandard2.0/_._": ""})
    body, ct = _multipart({}, "package", f"{pid}.{version}.nupkg", nupkg)
    return req("PUT", "/proxy/nuget-local/nuget/api/v2/package", USER, body, {"Content-Type": ct})[0]


def publish_maven(group, artifact, version):
    base = f"/proxy/maven-local/maven2/{group.replace('.', '/')}/{artifact}/{version}/{artifact}-{version}"
    pom = f"""<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version>
  <description>Demo greeter</description>
  <licenses><license><name>MIT</name></license></licenses>
</project>"""
    jar = _zip({"META-INF/MANIFEST.MF": "Manifest-Version: 1.0\n", README_MD: README.format(name=artifact)})
    a = req("PUT", base + ".pom", USER, pom.encode(), {"Content-Type": "application/xml"})[0]
    b = req("PUT", base + ".jar", USER, jar, {"Content-Type": "application/java-archive"})[0]
    return max(a, b)


def publish_composer(name, version):
    zp = _zip({"composer.json": json.dumps({"name": name, "version": version, "description": DESCRIPTION,
                                            "type": "library", "license": "MIT"}),
               README_MD: README.format(name=name)})
    return req("POST", "/proxy/composer-local/api/upload", USER, zp, {"Content-Type": "application/zip"})[0]


def publish_gem(name, version):
    yaml = (f"--- !ruby/object:Gem::Specification\nname: {name}\nversion: !ruby/object:Gem::Version\n"
            f"  version: '{version}'\nplatform: ruby\nsummary: Demo greeter\nlicenses:\n- MIT\n")
    gem = _tar({"metadata.gz": gzip.compress(yaml.encode(), mtime=0),
                "data.tar.gz": _tar({README_MD: README.format(name=name), "lib/greeter.rb": "# demo\n"})},
               gz=False)
    return req("POST", "/proxy/gems-local/api/v1/gems", USER, gem, {"Content-Type": OCTET_STREAM})[0]


def check(what, status, ok=(200, 201, 202, 204)):
    say(f"{what} → {status}" + ("" if status in ok else "  (unexpected)"))


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
        "POST", "/api/v1/flags/soc", body=body, headers=sign(body)
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


    print("8. the admin side: banner, webhook subscription, team namespace, beta member", flush=True)
    check("banner", req("PUT", "/api/v1/admin/banner", ADMIN,
                        {"message": "Demo instance: everything here is seeded, nothing is real.",
                         "level": "info"})[0])
    status, body = req("POST", "/api/v1/admin/notifications/subscriptions", ADMIN,
                       {"channel_name": "sink",
                        "event_types": ["package_published", "package_yanked", "package_deleted"]})
    check("subscription: every publish, yank and delete → sink", status)
    sub_id = json.loads(body).get("id") if status in (200, 201) else None
    # @platform, not @acme: a claim refuses everyone outside the group, and the
    # demo user publishes under @acme below.
    check("team namespace @platform on npm-team for group platform",
          req("POST", "/api/v1/admin/registries/npm-team/namespaces", ADMIN,
              {"prefix": "@platform", "group_id": "platform"})[0], ok=(200, 201, 204, 409))
    check("beta-channel member demo-user on npm-team",
          req("POST", "/api/v1/admin/registries/npm-team/beta-channel", ADMIN,
              {"principal_type": "user", "principal_id": "demo-user"})[0], ok=(200, 201, 204, 409))

    print("9. publishes by the demo user, one local registry per ecosystem", flush=True)
    done = (200, 201, 202, 204, 409)  # 409: already published by an earlier run
    for v in ("1.0.0", "1.1.0", "1.2.0"):
        check(f"npm-local demo-greeter@{v}", publish_npm("npm-local", "demo-greeter", v), done)
    check("npm-local demo-greeter@2.0.0-beta.1 (tag beta)",
          publish_npm("npm-local", "demo-greeter", "2.0.0-beta.1", tag="beta"), done)
    check("npm-team @acme/web@1.0.0", publish_npm("npm-team", "@acme/web", "1.0.0"), done)
    check("npm-team @acme/internal-tools@1.0.0 (sealed grants, in shadow)",
          publish_npm("npm-team", "@acme/internal-tools", "1.0.0"), done)
    for v in ("0.1.0", "0.2.0"):
        check(f"cargo-local demo-greeter@{v}", publish_cargo("demo-greeter", v), done)
    check("pypi-local demo-greeter==1.0.0", publish_pypi("demo-greeter", "1.0.0"), done)
    check("nuget-local Demo.Greeter 1.0.0", publish_nuget("Demo.Greeter", "1.0.0"), done)
    check("maven-local com.example:greeter:1.0.0", publish_maven("com.example", "greeter", "1.0.0"), done)
    check("composer-local acme/greeter 1.0.0", publish_composer("acme/greeter", "1.0.0"), done)
    check("gems-local demo_greeter 1.0.0", publish_gem("demo_greeter", "1.0.0"), done)

    print("10. traffic and admin actions on what was published", flush=True)
    for v in ("1.0.0", "1.2.0", "1.2.0", "1.2.0"):
        req("GET", f"/proxy/npm-local/demo-greeter/{v}/tarball", USER)
    req("GET", "/proxy/cargo-local/demo-greeter/0.2.0/download", USER)
    say("downloads of demo-greeter (npm ×4, cargo ×1)")
    check("block npm-local demo-greeter@1.1.0",
          req("POST", "/api/v1/admin/packages/block", ADMIN,
              {"registry": "npm-local", "name": "demo-greeter", "version": "1.1.0",
               "reason": "Demo: a regression in greet(), use 1.2.0"})[0], ok=(200, 201, 204, 409))
    check("bulk block cargo 0.1.0 + pypi 1.0.0",
          req("POST", "/api/v1/admin/packages/bulk-block", ADMIN, {"items": [
              {"registry": "cargo-local", "name": "demo-greeter", "version": "0.1.0", "reason": "Demo: bulk"},
              {"registry": "pypi-local", "name": "demo-greeter", "version": "1.0.0", "reason": "Demo: bulk"},
          ]})[0])
    if sub_id:
        check("test delivery to the sink",
              req("POST", f"/api/v1/admin/notifications/subscriptions/{sub_id}/test", ADMIN)[0])
    for _ in range(3):
        status, _ = req("GET", "/proxy/npm-closed/anything", USER)
    say(f"demo-user asks the closed registry three times → {status}")
    event = json.dumps({"event": "build", "status": "success", "pipeline": "demo/main#42"},
                       separators=(",", ":")).encode()
    check("signed inbound event from ci",
          req("POST", "/api/v1/webhooks/inbound/ci", body=event, headers=sign(event, INBOUND_SECRET))[0])
    for name in ("left-pad", "lodash", "react"):
        req("GET", f"/proxy/npm-down/{name}", USER, timeout=30)
    say("three reads on npm-down, whose upstream nobody answers")
    for name, version in (("is-number", "7.0.0"), ("is-odd", "3.0.1")):
        status, _ = req("GET", f"/proxy/npm-broken/{name}/{version}/tarball", USER)
        say(f"npm-broken {name}@{version} (its scanner is down) → {status}")
    check("block user former-contractor",
          req("POST", "/api/v1/admin/users/former-contractor/block", ADMIN,
              {"reason": "Demo: left the company, token not yet revoked"})[0], ok=(200, 201, 204, 409))
    check("warm npm-public: is-number",
          req("POST", "/api/v1/admin/registries/npm-public/warm", ADMIN, {"packages": ["is-number"]},
              timeout=180)[0], ok=(200, 202))
    status, _ = req("POST", "/api/v1/admin/registries/npm-local/import", ADMIN, {}, timeout=180)
    say(f"release import into npm-local (needs GitHub; a failed run is shown too) → {status}")

    print("11. a config change, recorded on the Configuration page", flush=True)
    cfg = f"{WORK}/config.toml"
    text = open(cfg, encoding="utf-8").read()
    if "max_packages_per_user = 12" in text:
        open(cfg, "w", encoding="utf-8").write(text.replace("max_packages_per_user = 12", "max_packages_per_user = 15", 1))
        time.sleep(3)  # the watcher applies the edit on its own
    check("reload", req("POST", "/api/v1/admin/config/reload", ADMIN)[0], ok=(200, 202, 204, 409))

    print("seeded.", flush=True)


if __name__ == "__main__":
    main()
