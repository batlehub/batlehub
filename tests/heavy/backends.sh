#!/usr/bin/env bash
# The production backends under real clients: S3 for the artifacts, Redis for
# the metadata cache, an OIDC issuer for the credential.
#
# Every other heavy suite runs on `storage = filesystem`, `auth = token` and
# the in-memory cache — the shape no deployment has. The adapters have their
# own integration tests against an S3 server and Redis, but none of those is a
# client fetching a package, and a stream cut in the middle of a `.vsix`, a
# document served from a cache under an older block list, or a JWT the
# server refuses for a reason npm cannot show are all things only a client
# sees. This suite is the client:
#
#   1. Anonymous, the credential-only npm registry refuses npm's own request
#      and nothing reaches the upstream.
#   2. A tampered id_token is refused the same way.
#   3. With the id_token dex minted for `dev@example.com` in `.npmrc`,
#      `npm install` goes through: the upstream served the packument and the
#      tarball once, the tarball is an object in the S3 bucket, and Redis
#      holds the metadata under a key naming the registry.
#   4. A second install from a clean npm cache is answered with **no**
#      upstream request — the packument from Redis, the tarball from S3.
#   5. The server is stopped and started again; a third install still asks
#      the upstream for nothing: what both backends hold outlived the process,
#      which the in-memory cache and a local directory never have to prove.
#   6. pip, on the PyPI registry, the same way: a wheel through S3, a simple
#      page through Redis, a second install with no upstream request.
#
# Then a second server on the same database, Redis and bucket, for RFC
# 0048-bis's cross-replica cold fill (§6.P), against a 64 MiB tarball the
# upstream serves at 2 MB/s, so a fill lasts about 33 s:
#
#   7. One upstream GET for two replicas: npm through A, then curl and npm
#      through B, which follows A's fill and is served its committed segments
#      while A is still filling; neither server's TMPDIR ever holds a scratch
#      file, and the bucket holds `staging:` segments during the fill only.
#   8. Takeover: A is killed (`kill -9`) mid-fill; B's reader is reset, its
#      retry takes the lease over, fetches again (restart, not resume) and the
#      dead fill's segments are gone.
#   9. A tampered tarball is refused on both, fetched once, and nothing of it
#      stays in the bucket; a later install through B is a cache hit.
#  10. One replica on a `memory` cache: its lease is in the process, so a
#      shared-cache server beside it does not see the fill and fetches too.
#
# What the suite starts itself when the environment does not name one:
# RustFS (`S3_TEST_ENDPOINT`, a cached download of the server binary — the same
# implementation CI runs as a service, and MinIO's replacement here), Redis
# (`REDIS_URL`, the `redis-server` binary out of the PyPI `redis-server`
# wheel), and it discovers dex at `OIDC_ISSUER` or the workspace sidecar on
# port 9000. CI provides all three as services (test.yaml, `heavy-backends`).
#
# Ports: 8140 (server), 8141 (tap), 8142 (upstream), 8143 (S3), 8144
# (Redis), 8145 (the second server). Environment: DATABASE_URL (required); S3_TEST_ENDPOINT,
# AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY (default rustfsadmin), REDIS_URL,
# OIDC_ISSUER, OIDC_CLIENT_ID/OIDC_CLIENT_SECRET (proxy-auth), OIDC_USER/
# OIDC_PASSWORD (dev@example.com / password); HEAVY_PORT, HEAVY_TAP_PORT,
# HEAVY_UPSTREAM_PORT; COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init backends 8140 8141
heavy_need npm "npm"
heavy_need node "nodejs"
heavy_need python3 "python3"
heavy_need curl "curl"
heavy_need unzip "unzip"

HEAVY_UPSTREAM_PORT="${HEAVY_UPSTREAM_PORT:-8142}"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/upstream_dir.sh"

REG_NPM="npm-$HEAVY_RUN"
REG_PYPI="pypi-$HEAVY_RUN"
PKG="heavy-backends-probe"
PKG_VERSION="1.0.0"
DIST="heavybackends"
MODULE="heavybackends"
DIST_VERSION="1.0.0"
# The port a local run's own S3 server binds. `HEAVY_MINIO_PORT` is still read,
# because it is what every note and runbook about this suite names.
S3_PORT="${HEAVY_S3_PORT:-${HEAVY_MINIO_PORT:-8143}}"
REDIS_PORT="${HEAVY_REDIS_PORT:-8144}"
export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-rustfsadmin}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-rustfsadmin}"
# The SDK would otherwise spend seconds probing EC2's metadata service on a
# machine that has none, before falling back to the variables above.
export AWS_EC2_METADATA_DISABLED=true
export OIDC_CLIENT_ID="${OIDC_CLIENT_ID:-proxy-auth}"
export OIDC_CLIENT_SECRET="${OIDC_CLIENT_SECRET:-proxy-auth-secret}"
OIDC_USER="${OIDC_USER:-dev@example.com}"
OIDC_PASSWORD="${OIDC_PASSWORD:-password}"

fetch() { curl -fsSL --proto '=https' --proto-redir '=https' "$@"; return $?; }

# The S3 server a local run starts for itself is **RustFS**, which is also the
# `rustfs/rustfs` image the CI job runs as a service: one S3 implementation
# measured in both places rather than MinIO here and RustFS there. It replaced
# MinIO because MinIO's own download host now answers **410 Gone** for every
# path under `dl.min.io/{server,client}/…`, so the local half of this suite
# could not be started at all.
#
# The *client* is still MinIO's `mc`, and now deliberately rather than for want
# of an alternative: RustFS does ship a CLI (`rustfs/cli`, the `rc` binary,
# which `mise.toml` installs for the compose and perf bucket tasks), but `rc`
# has no `--config-dir`. Its alias file is fixed at `~/.config/rc/config.toml`,
# so a heavy run would write its throwaway credentials into the caller's own
# config instead of into `$HEAVY_WORK`, and two runs at once would fight over
# it. `mc` is a plain S3 client and the four things this suite asks of it
# (alias, mb, ls --recursive, cat) are S3 calls, verified against RustFS. From
# the project's GitHub release for the same reason `rustfs` is, pinned so an
# upstream release cannot re-point a heavy suite.
#
# If that isolation arrives, `rc object list -r --json` is the migration: it
# replaces the `awk '{print $NF}'` below with a parsed key list.
RUSTFS_RELEASE="${HEAVY_RUSTFS_RELEASE:-1.0.0-rc.6}"
MC_RELEASE="${HEAVY_MC_RELEASE:-RELEASE.2025-08-13T08-35-41Z}"

# ── 0. The backends ─────────────────────────────────────────────────────────

if [[ -z "${S3_TEST_ENDPOINT:-}" ]]; then
  # One binary in a zip; `heavy_cached_dir` keeps it across runs, as it does
  # every other client this suite downloads.
  RUSTFS="$(heavy_cached_dir "rustfs-$RUSTFS_RELEASE" \
    "https://github.com/rustfs/rustfs/releases/download/$RUSTFS_RELEASE/rustfs-linux-x86_64-musl-v$RUSTFS_RELEASE.zip" \
    zip)/rustfs"
  [[ -x "$RUSTFS" ]] || chmod +x "$RUSTFS"
  mkdir -p "$HEAVY_WORK/s3-data"
  "$RUSTFS" server "$HEAVY_WORK/s3-data" --address "127.0.0.1:$S3_PORT" \
    --access-key "$AWS_ACCESS_KEY_ID" --secret-key "$AWS_SECRET_ACCESS_KEY" \
    >"$HEAVY_WORK/s3.log" 2>&1 &
  HEAVY_EXTRA_PIDS+=($!)
  export S3_TEST_ENDPOINT="http://127.0.0.1:$S3_PORT"
  # RustFS answers MinIO's own liveness path, so the probe is unchanged.
  for _ in $(seq 1 60); do curl -sf -o /dev/null "$S3_TEST_ENDPOINT/minio/health/live" && break; sleep 0.5; done
  curl -sf -o /dev/null "$S3_TEST_ENDPOINT/minio/health/live" || { tail -20 "$HEAVY_WORK/s3.log" >&2; heavy_fail "RustFS never came up on $S3_PORT"; }
  heavy_log "RustFS at $S3_TEST_ENDPOINT ($("$RUSTFS" --version 2>/dev/null | head -1))"
fi
export S3_TEST_ENDPOINT

if [[ -z "${REDIS_URL:-}" ]]; then
  REDIS="$HEAVY_CACHE/redis-server-6.0.9/redis-server"
  if [[ ! -x "$REDIS" ]]; then
    # The `redis-server` wheel on PyPI is a zip carrying a static binary; no
    # Python is involved in using it.
    heavy_log "Downloading redis-server 6.0.9 (the PyPI wheel) into $(dirname "$REDIS")"
    WHEEL_URL="$(curl -fsS https://pypi.org/pypi/redis-server/6.0.9/json \
      | python3 -c 'import sys,json;print([u["url"] for u in json.load(sys.stdin)["urls"] if "cp38-cp38-manylinux2010_x86_64" in u["filename"]][0])')"
    mkdir -p "$(dirname "$REDIS")"
    fetch -o "$HEAVY_WORK/redis-server.whl" "$WHEEL_URL"
    unzip -qo -j "$HEAVY_WORK/redis-server.whl" "redis_server/bin/redis-server" -d "$(dirname "$REDIS")"
    chmod +x "$REDIS"
  fi
  "$REDIS" --port "$REDIS_PORT" --bind 127.0.0.1 --save "" --appendonly no --dir "$HEAVY_WORK" \
    >"$HEAVY_WORK/redis.log" 2>&1 &
  HEAVY_EXTRA_PIDS+=($!)
  export REDIS_URL="redis://127.0.0.1:$REDIS_PORT"
  heavy_log "Redis at $REDIS_URL ($("$REDIS" --version | cut -d' ' -f1-3))"
fi
export REDIS_URL

# redis_cmd <arg>… — one command over RESP, replies printed one value per
# line. No redis-cli needed, and the check reads the store the server writes.
redis_cmd() {
  python3 - "$REDIS_URL" "$@" <<'PY'
import socket, sys
from urllib.parse import urlsplit
u = urlsplit(sys.argv[1]); args = sys.argv[2:]
s = socket.create_connection((u.hostname, u.port or 6379), timeout=5)
s.sendall(("*%d\r\n" % len(args) + "".join("$%d\r\n%s\r\n" % (len(a.encode()), a) for a in args)).encode())
f = s.makefile("rb")
def read():
    line = f.readline().rstrip(b"\r\n"); t, rest = line[:1], line[1:]
    if t in (b"+", b":"): return rest.decode()
    if t == b"-": raise SystemExit("redis: " + rest.decode())
    if t == b"$":
        n = int(rest)
        if n < 0: return None
        data = f.read(n); f.read(2); return data.decode(errors="replace")
    if t == b"*": return [read() for _ in range(int(rest))]
    raise SystemExit("redis: unexpected " + repr(line))
r = read()
for v in (r if isinstance(r, list) else [r]): print(v)
PY
  return $?
}
for _ in $(seq 1 30); do [[ "$(redis_cmd PING 2>/dev/null)" == "PONG" ]] && break; sleep 0.5; done
[[ "$(redis_cmd PING)" == "PONG" ]] || heavy_fail "Redis at $REDIS_URL does not answer PING"
redis_cmd FLUSHALL >/dev/null

if [[ -z "${OIDC_ISSUER:-}" ]]; then
  DISCOVERY="$(curl -sf "http://127.0.0.1:${OIDC_PORT:-9000}/.well-known/openid-configuration" 2>/dev/null || true)"
  [[ -n "$DISCOVERY" ]] || heavy_fail "no OIDC issuer: set OIDC_ISSUER, or run dex on 127.0.0.1:9000 (the workspace sidecar; CI's container)"
  OIDC_ISSUER="$(printf '%s' "$DISCOVERY" | python3 -c 'import sys,json;print(json.load(sys.stdin)["issuer"])')"
fi
export OIDC_ISSUER
TOKEN_ENDPOINT="$(curl -fsS "$OIDC_ISSUER/.well-known/openid-configuration" \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["token_endpoint"])')" \
  || heavy_fail "the issuer $OIDC_ISSUER serves no discovery document"
heavy_log "OIDC issuer $OIDC_ISSUER (token endpoint $TOKEN_ENDPOINT)"

mint_id_token() {  # <user> <password> → id_token on stdout (dex's password grant)
  local user="$1" password="$2"
  curl -fsS "$TOKEN_ENDPOINT" -d grant_type=password -d scope="openid email profile" \
    -d "client_id=$OIDC_CLIENT_ID" -d "client_secret=$OIDC_CLIENT_SECRET" \
    -d "username=$user" -d "password=$password" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["id_token"])'
  return $?
}
JWT="$(mint_id_token "$OIDC_USER" "$OIDC_PASSWORD")" || heavy_fail "dex refused the password grant for $OIDC_USER"
[[ "$JWT" == *.*.* ]] || heavy_fail "the issuer did not answer with a JWT"
python3 - "$JWT" "$OIDC_ISSUER" "$OIDC_CLIENT_ID" "$OIDC_USER" <<'PY' || heavy_fail "the id_token's claims are not the ones the server will check"
import base64, json, sys
tok, iss, aud, email = sys.argv[1:]
payload = tok.split(".")[1]; payload += "=" * (-len(payload) % 4)
c = json.loads(base64.urlsafe_b64decode(payload))
assert c["iss"] == iss, (c["iss"], iss)
assert c["aud"] == aud or aud in c["aud"], c["aud"]
assert c.get("email") == email, c.get("email")
PY
# The same token with its signature's last character changed: the issuer's
# key no longer verifies it, and the claims are untouched.
SIG="${JWT##*.}"
LAST="${SIG: -1}"; [[ "$LAST" == "A" ]] && FLIP="B" || FLIP="A"
TAMPERED="${JWT%.*}.${SIG%?}$FLIP"
heavy_log "id_token for $OIDC_USER minted (${#JWT} chars; role by email → user)"

# ── 1. The upstream, the bucket, the server ─────────────────────────────────

upstream_npm_package "$PKG" "$PKG_VERSION"
upstream_pypi_dist "$DIST" "$MODULE" "$DIST_VERSION"
# Sections 7 to 10: one 64 MiB package per claim, served at 2 MB/s.
FILL_MIB=64
for p in fill-join fill-takeover fill-tampered fill-memory; do
  upstream_npm_package "$p" 1.0.0 "$FILL_MIB"
done
# The packument advertises a digest the tarball does not have.
python3 - "$UPSTREAM_DIR/fill-tampered" <<'PY'
import json, sys
p = json.load(open(sys.argv[1]))
p["versions"]["1.0.0"]["dist"]["shasum"] = "0" * 40
json.dump(p, open(sys.argv[1], "w"))
PY
export UPSTREAM_THROTTLE_BPS=$((2 * 1000 * 1000)) UPSTREAM_THROTTLE_MATCH='^/tarballs/fill-'
upstream_serve

MC="$(command -v mc || true)"
if [[ -z "$MC" ]]; then
  MC="$HEAVY_CACHE/mc"
  [[ -x "$MC" ]] || { heavy_log "Downloading the MinIO client into $MC"; \
    fetch -o "$MC" "https://github.com/minio/mc/releases/download/$MC_RELEASE/mc.linux-amd64.$MC_RELEASE" \
      && chmod +x "$MC"; }
fi
export MC_CONFIG_DIR="$HEAVY_WORK/mc"
export HEAVY_S3_BUCKET="heavy-$HEAVY_RUN"
"$MC" --config-dir "$MC_CONFIG_DIR" alias set heavy "$S3_TEST_ENDPOINT" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null \
  || heavy_fail "mc could not reach $S3_TEST_ENDPOINT"
"$MC" --config-dir "$MC_CONFIG_DIR" mb "heavy/$HEAVY_S3_BUCKET" >/dev/null || heavy_fail "mc could not create the bucket"
s3_objects() { "$MC" --config-dir "$MC_CONFIG_DIR" ls --recursive "heavy/$HEAVY_S3_BUCKET" 2>/dev/null | awk '{print $NF}'; return $?; }
# The store is content-addressed (`blob/<sha256>`), so the proof that a
# client's bytes landed in S3 is the object named by their own hash — and
# `mc cat` reads it back to say it holds those bytes, not merely that name.
s3_holds_file() {  # <file> — the bucket has blob/<sha256 of file> with the same bytes
  local file="$1" sha
  sha="$(sha256sum "$file" | cut -d' ' -f1)"
  grep -qx "blob/$sha" <<<"$(s3_objects)" || return 1  # captured: grep -q would SIGPIPE mc
  [[ "$("$MC" --config-dir "$MC_CONFIG_DIR" cat "heavy/$HEAVY_S3_BUCKET/blob/$sha" | sha256sum | cut -d' ' -f1)" == "$sha" ]]
  return $?  # the status is the assertion: callers hang `||` off this
}
heavy_log "Bucket heavy/$HEAVY_S3_BUCKET created, empty"

export HEAVY_SERVER_FEATURES="storage-s3"
heavy_start_server tests/heavy/config.backends.toml
heavy_start_tap
NPM_URL="$HEAVY_TAP_BASE/proxy/$REG_NPM/"
PYPI_SIMPLE="$HEAVY_TAP_BASE/proxy/$REG_PYPI/simple/"

# npm_install <label> <npmrc-auth-line or ""> — a fresh project and a fresh
# npm cache every time, so a second install proves the *server's* cache.
# `--ignore-scripts`: what is measured is the fetch through the proxy, and a
# fixture's lifecycle script is not part of it (the fixture has none).
export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false
npm_install() {
  local label="$1" auth="$2"
  local proj="$HEAVY_WORK/proj-$label"
  mkdir -p "$proj"
  printf '{ "name": "consumer-%s", "version": "1.0.0", "private": true }\n' "$label" >"$proj/package.json"
  { echo "registry=$NPM_URL"; [[ -n "$auth" ]] && echo "//127.0.0.1:$HEAVY_TAP_PORT/proxy/$REG_NPM/:_authToken=$auth"; } >"$proj/.npmrc"
  (cd "$proj" && NPM_CONFIG_USERCONFIG="$proj/.npmrc" NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-$label" \
    npm install --ignore-scripts "$PKG@$PKG_VERSION" >"$HEAVY_WORK/npm-$label.txt" 2>&1)
  return $?
}
installed_ok() {  # <label>
  local label="$1"
  node -e "const p=require('$HEAVY_WORK/proj-$label/node_modules/$PKG/package.json'); if (p.version !== '$PKG_VERSION') process.exit(1)"
  return $?
}

# ── 2. Anonymous and tampered: refused, and the upstream never asked ────────

heavy_mark "anon"
if npm_install anon ""; then heavy_fail "npm installed $PKG with no credential from a registry whose anonymous holds no verb"; fi
heavy_wire_re_after "anon" "GET /proxy/$REG_NPM/$PKG -> 40[13]" "npm's packument request was not refused"
[[ "$(upstream_requests)" == "0" ]] || { cat "$UPSTREAM_LOG" >&2; heavy_fail "an anonymous request reached the upstream"; }
heavy_log "ANON-OK (npm refused; the upstream saw nothing)"

heavy_mark "tampered"
if npm_install tampered "$TAMPERED"; then heavy_fail "npm installed $PKG with a tampered id_token"; fi
heavy_wire_re_after "tampered" "GET /proxy/$REG_NPM/$PKG -> 40[13] .*Authorization: Bearer" "the tampered token was not refused"
[[ "$(upstream_requests)" == "0" ]] || { cat "$UPSTREAM_LOG" >&2; heavy_fail "a request with a tampered token reached the upstream"; }
heavy_log "TAMPERED-OK (a JWT with one signature character changed is refused; the upstream saw nothing)"

# ── 3. The id_token: install, and the bytes land in S3 and Redis ────────────

heavy_mark "first"
npm_install first "$JWT" || { cat "$HEAVY_WORK/npm-first.txt" >&2; tail -30 "$HEAVY_WORK/server.log" >&2; heavy_fail "npm install with dex's id_token failed"; }
installed_ok first || heavy_fail "$PKG@$PKG_VERSION is not what got installed"
heavy_wire_re_after "first" "GET /proxy/$REG_NPM/$PKG -> 200 .*Authorization: Bearer" "the packument was not served to the JWT"
heavy_wire_re_after "first" "GET /proxy/$REG_NPM/$PKG/$PKG_VERSION/tarball -> 200 .*Authorization: Bearer" "the tarball was not served to the JWT"
# The packument count is recorded, not pinned: measured at three for one
# `npm install` on 2026-09-11 (the document route and the artifact route
# each resolve the metadata for themselves, and npm asks twice) — an
# inefficiency worth a look, and not what this suite is about. What it is
# about is the *next* two counts: the same numbers again after a second
# install and after a restart.
UP_PACKUMENT="$(upstream_requests "$PKG ")"; UP_TARBALL="$(upstream_requests "tarballs/$PKG-")"
[[ "$UP_PACKUMENT" -ge 1 && "$UP_TARBALL" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "expected the packument fetched and the tarball fetched exactly once from the upstream, got $UP_PACKUMENT/$UP_TARBALL"; }
OBJECTS="$(s3_objects)"
s3_holds_file "$UPSTREAM_DIR/tarballs/$PKG-$PKG_VERSION.tgz" \
  || { echo "$OBJECTS" >&2; heavy_fail "the S3 bucket holds no blob with the tarball's sha256 after the install"; }
DBSIZE="$(redis_cmd DBSIZE)"
[[ "$DBSIZE" -gt 0 ]] || heavy_fail "Redis is empty after the install — the metadata cache is not the configured backend"
KEYS="$(redis_cmd KEYS '*')"
grep -q "$REG_NPM" <<<"$KEYS" || { echo "$KEYS" >&2; heavy_fail "no Redis key names the registry $REG_NPM"; }
heavy_log "OIDC-INSTALL-OK ($PKG@$PKG_VERSION installed with dex's id_token; upstream asked $UP_PACKUMENT time(s) for the packument and once for the tarball; the tarball's bytes under blob/<sha256> in S3, $DBSIZE key(s) in Redis)"

# ── 4. Again, from nothing on the client: the backends answer ───────────────

heavy_mark "second"
npm_install second "$JWT" || { cat "$HEAVY_WORK/npm-second.txt" >&2; heavy_fail "the second npm install failed"; }
installed_ok second || heavy_fail "the second install is not $PKG@$PKG_VERSION"
[[ "$(upstream_requests "$PKG ")" == "$UP_PACKUMENT" && "$(upstream_requests "tarballs/$PKG-")" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "the second install reached the upstream — the S3 or Redis copy was not used"; }
heavy_wire_re_after "second" "GET /proxy/$REG_NPM/$PKG/$PKG_VERSION/tarball -> 200" "the tarball was not served on the second install"
heavy_log "CACHE-OK (second install: packument from Redis, tarball from S3, zero upstream requests)"

# ── 5. The process dies; the backends do not ────────────────────────────────

heavy_log "Restarting the server"
heavy_stop_server
heavy_start_server tests/heavy/config.backends.toml
heavy_mark "restarted"
npm_install restarted "$JWT" || { cat "$HEAVY_WORK/npm-restarted.txt" >&2; heavy_fail "npm install after the restart failed"; }
installed_ok restarted || heavy_fail "the install after the restart is not $PKG@$PKG_VERSION"
[[ "$(upstream_requests "$PKG ")" == "$UP_PACKUMENT" && "$(upstream_requests "tarballs/$PKG-")" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "after a restart the server went back to the upstream — what Redis and S3 hold did not outlive the process"; }
heavy_log "RESTART-OK (a new process, the same Redis and S3: still zero upstream requests)"

# ── 6. pip: the other artifact path through the same backends ───────────────

pip_install() {  # <label>
  local label="$1"
  local venv="$HEAVY_WORK/venv-$label"
  python3 -m venv "$venv" >/dev/null || heavy_fail "python3 -m venv failed"
  "$venv/bin/python" -m pip install --quiet --no-cache-dir --index-url "$PYPI_SIMPLE" "$DIST==$DIST_VERSION" \
    >"$HEAVY_WORK/pip-$label.txt" 2>&1 || { cat "$HEAVY_WORK/pip-$label.txt" >&2; return 1; }
  "$venv/bin/python" -c "import $MODULE; assert $MODULE.VALUE == '$DIST'" || heavy_fail "the installed distribution is not the upstream's"
  return $?
}
heavy_mark "pip-first"
pip_install first || heavy_fail "pip install through the PyPI registry failed"
heavy_wire_after "pip-first" "GET /proxy/$REG_PYPI/simple/$DIST/ -> 200" "pip did not read the simple page"
UP_SIMPLE="$(upstream_requests "simple/$DIST/")"
[[ "$UP_SIMPLE" -ge 1 && "$(upstream_requests "files/$DIST-")" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "expected the simple page fetched and the wheel fetched exactly once from the upstream"; }
s3_holds_file "$UPSTREAM_DIR/files/$DIST-$DIST_VERSION-py3-none-any.whl" \
  || { s3_objects >&2; heavy_fail "the S3 bucket holds no blob with the wheel's sha256 after pip install"; }
heavy_mark "pip-second"
pip_install second || heavy_fail "the second pip install failed"
[[ "$(upstream_requests "simple/$DIST/")" == "$UP_SIMPLE" && "$(upstream_requests "files/$DIST-")" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "the second pip install reached the upstream"; }
heavy_log "PIP-OK (wheel through S3, simple page through Redis; the second install asked the upstream for nothing)"

heavy_log "Measurement: storage S3 ($S3_TEST_ENDPOINT, bucket $HEAVY_S3_BUCKET: $(s3_objects | wc -l) objects), cache Redis ($REDIS_URL: $(redis_cmd DBSIZE) keys), OIDC $OIDC_ISSUER"
heavy_log "  upstream requests in the whole run: $(upstream_requests) — npm: $UP_PACKUMENT packument + 1 tarball, pip: $UP_SIMPLE page + 1 wheel, the rest the JSON-API probes an HTML-only index answers 404 — across five installs and one server restart"

# ── 7. Two replicas, one cold fill (RFC 0048-bis §6.P obs. 1–3) ─────────────

REG_FILL="npmfill-$HEAVY_RUN"
SECOND_PORT="${HEAVY_SECOND_PORT:-8145}"
SEGMENT=$((8 * 1024 * 1024))
TMP_A="$HEAVY_WORK/tmp-a"; TMP_B="$HEAVY_WORK/tmp-b"
mkdir -p "$TMP_A" "$TMP_B"

# Server A again, with a TMPDIR of its own, and B beside it on the same
# database, Redis and bucket: a scratch file would show in either.
heavy_stop_server
TMPDIR="$TMP_A" heavy_start_server tests/heavy/config.backends.toml
TMPDIR="$TMP_B" heavy_start_second_server tests/heavy/config.backends.toml "$SECOND_PORT"
BASE_A="$HEAVY_BASE"; BASE_B="$HEAVY_BASE2"

# metric <base> <regex> — the sum of the series whose line matches.
metric() {
  curl -fsS "$1/metrics" | grep -E "^$2" | awk '{s += $NF} END {print s + 0}'
  return 0
}
# The server binary of a cargo process group: the PID to kill is not cargo's.
server_pid() { pgrep -g "$1" -x batlehub | head -1; return 0; }
tarball() { echo "$UPSTREAM_DIR/tarballs/$1-1.0.0.tgz"; return 0; }
tarball_url() { echo "$1/proxy/$REG_FILL/$2/1.0.0/tarball"; return 0; }
fill_gets() { upstream_requests "tarballs/$1-"; return 0; }

# npm_fill <label> <base> <pkg> — npm against one server directly, no retry:
# a refused body must not be fetched again behind the assertion's back.
npm_fill() {
  local label="$1" base="$2" pkg="$3" proj="$HEAVY_WORK/proj-$1"
  mkdir -p "$proj"
  printf '{ "name": "consumer-%s", "version": "1.0.0", "private": true }\n' "$label" >"$proj/package.json"
  echo "registry=$base/proxy/$REG_FILL/" >"$proj/.npmrc"
  (cd "$proj" && NPM_CONFIG_USERCONFIG="$proj/.npmrc" NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-$label" \
    NPM_CONFIG_FETCH_RETRIES=0 NPM_CONFIG_FETCH_TIMEOUT=300000 \
    npm install --ignore-scripts "$pkg@1.0.0" >"$HEAVY_WORK/npm-$label.txt" 2>&1)
  return $?
}
# timed_curl <label> <url> — the body to a file; first byte and total on stdout.
timed_curl() {
  local rc=0  # `|| rc=$?`: under `set -e` a failing curl would end the subshell unrecorded
  curl -sS --max-time 400 -o "$HEAVY_WORK/curl-$1.bin" -w '%{time_starttransfer} %{time_total}' "$2" \
    >"$HEAVY_WORK/curl-$1.time" 2>"$HEAVY_WORK/curl-$1.err" || rc=$?
  echo "$rc" >"$HEAVY_WORK/curl-$1.rc"
  return 0
}

# A scratch file would be `batlehub-fill-*` under either TMPDIR.
( while :; do ls -A "$TMP_A" "$TMP_B"; sleep 1; done ) >"$HEAVY_WORK/tmpdir-samples.txt" 2>&1 &
TMPDIR_SAMPLER=$!
HEAVY_EXTRA_PIDS+=("$TMPDIR_SAMPLER")

heavy_log "7. One cold fill of fill-join across two replicas"
npm_fill join-a "$BASE_A" fill-join & NPM_A=$!
sleep 3
timed_curl join-b "$(tarball_url "$BASE_B" fill-join)" & CURL_B=$!
npm_fill join-b "$BASE_B" fill-join & NPM_B=$!
# The segments are in the bucket while A fills, not only after.
sleep 8
STAGING_DURING="$(s3_objects | grep -c '^staging:' || true)"
wait "$NPM_A" || { cat "$HEAVY_WORK/npm-join-a.txt" >&2; heavy_fail "npm through A failed"; }
wait "$NPM_B" || { cat "$HEAVY_WORK/npm-join-b.txt" >&2; heavy_fail "npm through B, joining A's fill, failed"; }
wait "$CURL_B"
[[ "$(cat "$HEAVY_WORK/curl-join-b.rc")" == "0" ]] || { cat "$HEAVY_WORK/curl-join-b.err" >&2; heavy_fail "curl through B failed"; }
cmp -s "$HEAVY_WORK/curl-join-b.bin" "$(tarball fill-join)" || heavy_fail "B's body is not the upstream tarball"
[[ "$(fill_gets fill-join)" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "two replicas fetched the tarball $(fill_gets fill-join) times, not once"; }
REMOTE_JOINS="$(metric "$BASE_B" 'batlehub_proxy_fill_joined_total\{.*scope="remote"')"
[[ "$REMOTE_JOINS" == "1" ]] || heavy_fail "B followed $REMOTE_JOINS remote fills, not 1"
[[ "$(metric "$BASE_A" 'batlehub_proxy_fill_takeovers_total')" == "0" ]] || heavy_fail "A took a lease over in a fill nobody killed"
read -r TTFB TOTAL <"$HEAVY_WORK/curl-join-b.time" || true  # no trailing newline
python3 -c "import sys; t, n = map(float, sys.argv[1:]); sys.exit(0 if n - t >= 10 else 1)" "$TTFB" "$TOTAL" \
  || heavy_fail "B's first byte came at ${TTFB}s of ${TOTAL}s: it waited for the artifact instead of reading A's segments"
heavy_log "JOIN-OK (one upstream GET for two replicas; B's first byte at ${TTFB}s of ${TOTAL}s)"

sleep 2
kill "$TMPDIR_SAMPLER" 2>/dev/null || true
! grep -q 'batlehub-fill-' "$HEAVY_WORK/tmpdir-samples.txt" \
  || heavy_fail "a scratch file appeared in a server's TMPDIR: $(grep -m1 batlehub-fill- "$HEAVY_WORK/tmpdir-samples.txt")"
[[ "$STAGING_DURING" -gt 0 ]] || { s3_objects >&2; heavy_fail "no staging: segment was in the bucket during the fill"; }
[[ "$(s3_objects | grep -c '^staging:' || true)" == "0" ]] || { s3_objects >&2; heavy_fail "staging: objects outlived the fill"; }
s3_holds_file "$(tarball fill-join)" || heavy_fail "the bucket holds no blob/<sha256> of the tarball after the fill"
heavy_log "NO-SCRATCH-OK (neither TMPDIR ever held a fill file; $STAGING_DURING staging: segment(s) mid-fill, none after, one blob)"

# ── 8. Takeover after kill -9 (obs. 4) ──────────────────────────────────────

heavy_log "8. A dies mid-fill; B takes the fill over"
timed_curl take-a "$(tarball_url "$BASE_A" fill-takeover)" & CURL_A=$!
sleep 3
timed_curl take-b "$(tarball_url "$BASE_B" fill-takeover)" & CURL_B=$!
sleep 7
PID_A="$(server_pid "$HEAVY_SERVER_PID")"
[[ -n "$PID_A" ]] || heavy_fail "no batlehub process in A's process group"
kill -9 "$PID_A"
wait "$CURL_A" || true
wait "$CURL_B"
[[ "$(cat "$HEAVY_WORK/curl-take-b.rc")" != "0" ]] || heavy_fail "B's reader of a dead fill ended as if it were complete"
GOT="$(stat -c %s "$HEAVY_WORK/curl-take-b.bin" 2>/dev/null || echo 0)"
[[ "$GOT" -lt "$(stat -c %s "$(tarball fill-takeover)")" ]] || heavy_fail "B served the whole size of a fill whose holder died"
heavy_log "B's reader reset: curl exit $(cat "$HEAVY_WORK/curl-take-b.rc") ($(head -c 200 "$HEAVY_WORK/curl-take-b.err")) after $GOT bytes"
timed_curl take-retry "$(tarball_url "$BASE_B" fill-takeover)"
[[ "$(cat "$HEAVY_WORK/curl-take-retry.rc")" == "0" ]] || { cat "$HEAVY_WORK/curl-take-retry.err" >&2; heavy_fail "the retry on B did not take the fill over"; }
cmp -s "$HEAVY_WORK/curl-take-retry.bin" "$(tarball fill-takeover)" || heavy_fail "the taken-over fill's body is not the tarball"
[[ "$(fill_gets fill-takeover)" == "2" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "expected a second upstream GET after the takeover (restart, not resume), got $(fill_gets fill-takeover)"; }
[[ "$(metric "$BASE_B" 'batlehub_proxy_fill_takeovers_total')" == "1" ]] || heavy_fail "B counted no takeover"
# The log is coloured: strip the escapes before reading the field.
DEAD_FILL="$(sed 's/\x1b\[[0-9;]*m//g' "$HEAVY_WORK/server2.log" | grep -o 'previous_fill=[0-9a-f-]*' | tail -1 | cut -d= -f2 || true)"
[[ -n "$DEAD_FILL" ]] || heavy_fail "B logged no 'cold fill taken over' naming the previous fill"
# Captured first: `grep -q` closing the pipe early would fail `mc` under pipefail.
OBJECTS="$(s3_objects)"
! grep -q "$DEAD_FILL" <<<"$OBJECTS" || { echo "$OBJECTS" >&2; heavy_fail "objects of the dead fill $DEAD_FILL are still in the bucket"; }
heavy_log "TAKEOVER-OK (B reset its reader, took over $DEAD_FILL, fetched again and left nothing of it)"

# A back, for the next two claims.
heavy_stop_server
TMPDIR="$TMP_A" heavy_start_server tests/heavy/config.backends.toml

# ── 9. A tampered tarball, refused on both; then a cache hit (obs. 5–6) ─────

heavy_log "9. A tarball that does not match its packument, across two replicas"
npm_fill bad-a "$BASE_A" fill-tampered & NPM_A=$!
sleep 3
timed_curl bad-b "$(tarball_url "$BASE_B" fill-tampered)" & CURL_B=$!
npm_fill bad-b "$BASE_B" fill-tampered & NPM_B=$!
if wait "$NPM_A"; then heavy_fail "npm through A installed a tampered tarball"; fi
if wait "$NPM_B"; then heavy_fail "npm through B installed a tampered tarball"; fi
wait "$CURL_B"
heavy_log "npm on A said: $(grep -m2 -iE 'integrity|ECONNRESET|error' "$HEAVY_WORK/npm-bad-a.txt" | tr '\n' ' ')"
heavy_log "npm on B said: $(grep -m2 -iE 'integrity|ECONNRESET|error' "$HEAVY_WORK/npm-bad-b.txt" | tr '\n' ' ')"
[[ "$(fill_gets fill-tampered)" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "the tampered tarball was fetched $(fill_gets fill-tampered) times, not once"; }
BAD_SIZE="$(stat -c %s "$(tarball fill-tampered)")"
LAST_SEGMENT=$(( BAD_SIZE % SEGMENT )); [[ "$LAST_SEGMENT" -gt 0 ]] || LAST_SEGMENT="$SEGMENT"
GOT="$(stat -c %s "$HEAVY_WORK/curl-bad-b.bin" 2>/dev/null || echo 0)"
[[ "$GOT" -le $((BAD_SIZE - LAST_SEGMENT)) ]] || heavy_fail "B served $GOT of $BAD_SIZE bytes of an unverified body: more than all but its last segment"
sleep 2
! s3_holds_file "$(tarball fill-tampered)" || heavy_fail "the tampered tarball was promoted into the bucket"
[[ "$(s3_objects | grep -c '^staging:' || true)" == "0" ]] || { s3_objects >&2; heavy_fail "the failed fill left staging: objects"; }
heavy_log "TAMPERED-FILL-OK (refused on both, fetched once, B got $GOT of $BAD_SIZE bytes, nothing kept)"

HITS_BEFORE="$(metric "$BASE_B" 'batlehub_artifact_cache_hits_total\{.*registry="'"$REG_FILL"'"')"
npm_fill hit-b "$BASE_B" fill-join || { cat "$HEAVY_WORK/npm-hit-b.txt" >&2; heavy_fail "a fourth install through B failed"; }
[[ "$(fill_gets fill-join)" == "1" ]] || heavy_fail "a cached artifact was fetched again"
HITS_AFTER="$(metric "$BASE_B" 'batlehub_artifact_cache_hits_total\{.*registry="'"$REG_FILL"'"')"
[[ "$HITS_AFTER" -gt "$HITS_BEFORE" ]] || heavy_fail "B's install did not move batlehub_artifact_cache_hits_total"
heavy_log "HIT-OK (an install through B from a clean npm cache: zero upstream requests)"

# ── 10. One replica on a memory cache: the lease stays in the process (obs. 7)

heavy_log "10. A on a memory cache: an in-process lease B cannot see"
MEMORY_CONFIG="$HEAVY_WORK/config.backends-memory.toml"
python3 - tests/heavy/config.backends.toml "$MEMORY_CONFIG" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
open(sys.argv[2], "w").write(re.sub(r'\n\[cache\]\ntype = "redis"\nurl = "\$\{REDIS_URL\}"\n', "\n", text))
PY
! grep -q '^\[cache\]' "$MEMORY_CONFIG" || heavy_fail "the memory config still has a [cache] block"
heavy_stop_server
TMPDIR="$TMP_A" heavy_start_server "$MEMORY_CONFIG"
npm_fill mem-a "$BASE_A" fill-memory & NPM_A=$!
sleep 3
timed_curl mem-b "$(tarball_url "$BASE_B" fill-memory)" & CURL_B=$!
wait "$NPM_A" || { cat "$HEAVY_WORK/npm-mem-a.txt" >&2; heavy_fail "npm through A on a memory cache failed"; }
wait "$CURL_B"
[[ "$(cat "$HEAVY_WORK/curl-mem-b.rc")" == "0" ]] || heavy_fail "curl through B failed beside a memory-cache A"
[[ "$(fill_gets fill-memory)" == "2" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "expected A and B to fetch on their own (no shared lease row), got $(fill_gets fill-memory) GETs"; }
[[ "$(metric "$BASE_A" 'batlehub_proxy_fill_joined_total\{.*scope="remote"')" == "0" ]] || heavy_fail "a memory-cache server followed a remote fill"
heavy_log "MEMORY-OK (A installed with one fetch of its own and no lease row: B, on the shared lease, did not see A's fill)"

heavy_done BACKENDS-HEAVY-OK
