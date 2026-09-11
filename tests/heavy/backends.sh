#!/usr/bin/env bash
# The production backends under real clients: S3 for the artifacts, Redis for
# the metadata cache, an OIDC issuer for the credential.
#
# Every other heavy suite runs on `storage = filesystem`, `auth = token` and
# the in-memory cache — the shape no deployment has. The adapters have their
# own integration tests against MinIO and Redis, but none of those is a
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
# What the suite starts itself when the environment does not name one:
# MinIO (`S3_TEST_ENDPOINT`, a cached download of the server binary), Redis
# (`REDIS_URL`, the `redis-server` binary out of the PyPI `redis-server`
# wheel), and it discovers dex at `OIDC_ISSUER` or the workspace sidecar on
# port 9000. CI provides all three as services (test.yaml, `heavy-backends`).
#
# Ports: 8140 (server), 8141 (tap), 8142 (upstream), 8143 (MinIO), 8144
# (Redis). Environment: DATABASE_URL (required); S3_TEST_ENDPOINT,
# AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY (default minioadmin), REDIS_URL,
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
MINIO_PORT="${HEAVY_MINIO_PORT:-8143}"
REDIS_PORT="${HEAVY_REDIS_PORT:-8144}"
export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-minioadmin}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-minioadmin}"
# The SDK would otherwise spend seconds probing EC2's metadata service on a
# machine that has none, before falling back to the variables above.
export AWS_EC2_METADATA_DISABLED=true
export OIDC_CLIENT_ID="${OIDC_CLIENT_ID:-proxy-auth}"
export OIDC_CLIENT_SECRET="${OIDC_CLIENT_SECRET:-proxy-auth-secret}"
OIDC_USER="${OIDC_USER:-dev@example.com}"
OIDC_PASSWORD="${OIDC_PASSWORD:-password}"

fetch() { curl -fsSL --proto '=https' --proto-redir '=https' "$@"; return $?; }

# ── 0. The backends ─────────────────────────────────────────────────────────

if [[ -z "${S3_TEST_ENDPOINT:-}" ]]; then
  MINIO="$HEAVY_CACHE/minio"
  if [[ ! -x "$MINIO" ]]; then
    heavy_log "Downloading the MinIO server into $MINIO"
    fetch -o "$MINIO.download" "https://dl.min.io/server/minio/release/linux-amd64/minio" \
      && mv "$MINIO.download" "$MINIO" && chmod +x "$MINIO"
  fi
  mkdir -p "$HEAVY_WORK/minio-data"
  MINIO_ROOT_USER="$AWS_ACCESS_KEY_ID" MINIO_ROOT_PASSWORD="$AWS_SECRET_ACCESS_KEY" MINIO_BROWSER=off \
    "$MINIO" server "$HEAVY_WORK/minio-data" --address "127.0.0.1:$MINIO_PORT" >"$HEAVY_WORK/minio.log" 2>&1 &
  HEAVY_EXTRA_PIDS+=($!)
  export S3_TEST_ENDPOINT="http://127.0.0.1:$MINIO_PORT"
  for _ in $(seq 1 60); do curl -sf -o /dev/null "$S3_TEST_ENDPOINT/minio/health/live" && break; sleep 0.5; done
  curl -sf -o /dev/null "$S3_TEST_ENDPOINT/minio/health/live" || { tail -20 "$HEAVY_WORK/minio.log" >&2; heavy_fail "MinIO never came up on $MINIO_PORT"; }
  heavy_log "MinIO at $S3_TEST_ENDPOINT ($("$MINIO" --version 2>/dev/null | head -1))"
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
  curl -fsS "$TOKEN_ENDPOINT" -d grant_type=password -d scope="openid email profile" \
    -d "client_id=$OIDC_CLIENT_ID" -d "client_secret=$OIDC_CLIENT_SECRET" \
    -d "username=$1" -d "password=$2" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["id_token"])'
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
upstream_serve

MC="$(command -v mc || true)"
if [[ -z "$MC" ]]; then
  MC="$HEAVY_CACHE/mc"
  [[ -x "$MC" ]] || { heavy_log "Downloading the MinIO client into $MC"; fetch -o "$MC" https://dl.min.io/client/mc/release/linux-amd64/mc && chmod +x "$MC"; }
fi
export MC_CONFIG_DIR="$HEAVY_WORK/mc"
export HEAVY_S3_BUCKET="heavy-$HEAVY_RUN"
"$MC" --config-dir "$MC_CONFIG_DIR" alias set heavy "$S3_TEST_ENDPOINT" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null \
  || heavy_fail "mc could not reach $S3_TEST_ENDPOINT"
"$MC" --config-dir "$MC_CONFIG_DIR" mb "heavy/$HEAVY_S3_BUCKET" >/dev/null || heavy_fail "mc could not create the bucket"
s3_objects() { "$MC" --config-dir "$MC_CONFIG_DIR" ls --recursive "heavy/$HEAVY_S3_BUCKET" 2>/dev/null | awk '{print $NF}'; }
# The store is content-addressed (`blob/<sha256>`), so the proof that a
# client's bytes landed in S3 is the object named by their own hash — and
# `mc cat` reads it back to say it holds those bytes, not merely that name.
s3_holds_file() {  # <file> — the bucket has blob/<sha256 of file> with the same bytes
  local sha
  sha="$(sha256sum "$1" | cut -d' ' -f1)"
  s3_objects | grep -qx "blob/$sha" || return 1
  [[ "$("$MC" --config-dir "$MC_CONFIG_DIR" cat "heavy/$HEAVY_S3_BUCKET/blob/$sha" | sha256sum | cut -d' ' -f1)" == "$sha" ]]
}
heavy_log "Bucket heavy/$HEAVY_S3_BUCKET created, empty"

export HEAVY_SERVER_FEATURES="storage-s3"
heavy_start_server tests/heavy/config.backends.toml
heavy_start_tap
NPM_URL="$HEAVY_TAP_BASE/proxy/$REG_NPM/"
PYPI_SIMPLE="$HEAVY_TAP_BASE/proxy/$REG_PYPI/simple/"

# npm_install <label> <npmrc-auth-line or ""> — a fresh project and a fresh
# npm cache every time, so a second install proves the *server's* cache.
export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false
npm_install() {
  local label="$1" auth="$2"
  local proj="$HEAVY_WORK/proj-$label"
  mkdir -p "$proj"
  printf '{ "name": "consumer-%s", "version": "1.0.0", "private": true }\n' "$label" >"$proj/package.json"
  { echo "registry=$NPM_URL"; [[ -n "$auth" ]] && echo "//127.0.0.1:$HEAVY_TAP_PORT/proxy/$REG_NPM/:_authToken=$auth"; } >"$proj/.npmrc"
  (cd "$proj" && NPM_CONFIG_USERCONFIG="$proj/.npmrc" NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-$label" \
    npm install "$PKG@$PKG_VERSION" >"$HEAVY_WORK/npm-$label.txt" 2>&1)
  return $?
}
installed_ok() {  # <label>
  node -e "const p=require('$HEAVY_WORK/proj-$1/node_modules/$PKG/package.json'); if (p.version !== '$PKG_VERSION') process.exit(1)"
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
  local venv="$HEAVY_WORK/venv-$1"
  python3 -m venv "$venv" >/dev/null || heavy_fail "python3 -m venv failed"
  "$venv/bin/python" -m pip install --quiet --no-cache-dir --index-url "$PYPI_SIMPLE" "$DIST==$DIST_VERSION" \
    >"$HEAVY_WORK/pip-$1.txt" 2>&1 || { cat "$HEAVY_WORK/pip-$1.txt" >&2; return 1; }
  "$venv/bin/python" -c "import $MODULE; assert $MODULE.VALUE == '$DIST'" || heavy_fail "the installed distribution is not the upstream's"
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

heavy_done BACKENDS-HEAVY-OK
