# A directory that is an npm registry and a PyPI index at once. Sourced by
# backends.sh and hybrid.sh after lib.sh's `heavy_init`.
#
# Served with `python3 -m http.server`, whose access log is what makes an
# upstream request *countable*: a cache hit, or a hybrid registry answering
# from what it holds, is the absence of a line here — an assertion no test
# against pypi.org or registry.npmjs.org can make (upstream_audit.sh, RFC
# 0014, first made this shape).
#
#   upstream_npm_package <name> <version>       GET /<name>, tarballs/<name>-<v>.tgz
#   upstream_pypi_dist <dist> <module> <version> simple/<dist>/, files/<dist>-<v>-py3-none-any.whl
#   upstream_serve                              start it on $UPSTREAM_PORT (HEAVY_UPSTREAM_URL)
#   upstream_requests [regex]                   count of GETs so far (matching regex)
#
# The npm packument is a file with no extension and the tarball lives under
# `tarballs/`, because a file and a directory cannot share a name. The wheel
# is written by hand — a zip with `.dist-info` — rather than built with
# setuptools in a venv: pip reads METADATA, WHEEL and RECORD, and nothing
# else, and this keeps the fixture at a second rather than a minute.

UPSTREAM_PORT="${HEAVY_UPSTREAM_PORT:?set HEAVY_UPSTREAM_PORT before sourcing upstream_dir.sh}"
export HEAVY_UPSTREAM_URL="http://127.0.0.1:$UPSTREAM_PORT"
UPSTREAM_DIR="$HEAVY_WORK/upstream"
UPSTREAM_LOG="$HEAVY_WORK/upstream.log"
mkdir -p "$UPSTREAM_DIR/tarballs" "$UPSTREAM_DIR/simple" "$UPSTREAM_DIR/files"

upstream_npm_package() {  # <name> <version>
  local name="$1" version="$2"
  local build="$HEAVY_WORK/npm-build-$name-$version"
  mkdir -p "$build/package"
  cat > "$build/package/package.json" <<JSON
{ "name": "$name", "version": "$version", "description": "heavy upstream fixture", "license": "MIT", "main": "index.js" }
JSON
  echo "module.exports = { name: '$name', version: '$version', origin: 'upstream' };" > "$build/package/index.js"
  tar -C "$build" -czf "$UPSTREAM_DIR/tarballs/$name-$version.tgz" package
  local shasum
  shasum="$(sha1sum "$UPSTREAM_DIR/tarballs/$name-$version.tgz" | cut -d' ' -f1)"
  cat > "$UPSTREAM_DIR/$name" <<JSON
{
  "name": "$name",
  "dist-tags": { "latest": "$version" },
  "versions": {
    "$version": {
      "name": "$name", "version": "$version", "license": "MIT",
      "dist": { "tarball": "$HEAVY_UPSTREAM_URL/tarballs/$name-$version.tgz", "shasum": "$shasum" }
    }
  },
  "time": { "created": "2024-01-01T00:00:00.000Z", "modified": "2024-01-01T00:00:00.000Z", "$version": "2024-01-01T00:00:00.000Z" }
}
JSON
  return 0
}

upstream_wheel() {  # <dist> <module> <version> <out.whl> — a minimal, valid wheel
  python3 - "$@" <<'PY'
import base64, hashlib, sys, zipfile
dist, module, version, out = sys.argv[1:]
info = f"{dist}-{version}.dist-info"
files = {
    f"{module}/__init__.py": f"VALUE = '{dist}'\nORIGIN = 'upstream'\n",
    f"{info}/METADATA": f"Metadata-Version: 2.1\nName: {dist}\nVersion: {version}\nSummary: heavy upstream fixture\n",
    f"{info}/WHEEL": "Wheel-Version: 1.0\nGenerator: batlehub-heavy\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    f"{info}/top_level.txt": module + "\n",
}
record = []
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for path, text in files.items():
        data = text.encode()
        z.writestr(path, data)
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        record.append(f"{path},sha256={digest},{len(data)}")
    record.append(f"{info}/RECORD,,")
    z.writestr(f"{info}/RECORD", "\n".join(record) + "\n")
PY
  return $?
}

upstream_pypi_dist() {  # <dist> <module> <version>
  local dist="$1" module="$2" version="$3"
  local wheel="${dist}-${version}-py3-none-any.whl"
  upstream_wheel "$dist" "$module" "$version" "$UPSTREAM_DIR/files/$wheel"
  local sha
  sha="$(sha256sum "$UPSTREAM_DIR/files/$wheel" | cut -d' ' -f1)"
  mkdir -p "$UPSTREAM_DIR/simple/$dist"
  cat > "$UPSTREAM_DIR/simple/$dist/index.html" <<HTML
<!DOCTYPE html><html><head><meta name="pypi:repository-version" content="1.0"><title>Links for $dist</title></head>
<body><h1>Links for $dist</h1>
<a href="$HEAVY_UPSTREAM_URL/files/$wheel#sha256=$sha">$wheel</a><br/>
</body></html>
HTML
  return 0
}

upstream_serve() {
  python3 -m http.server "$UPSTREAM_PORT" --bind 127.0.0.1 --directory "$UPSTREAM_DIR" \
    >"$UPSTREAM_LOG" 2>&1 &
  HEAVY_EXTRA_PIDS+=($!)
  for _ in $(seq 1 30); do
    curl -sf -o /dev/null "$HEAVY_UPSTREAM_URL/" && break
    sleep 0.5
  done
  curl -sf -o /dev/null "$HEAVY_UPSTREAM_URL/" || heavy_fail "the served upstream never came up on $UPSTREAM_PORT"
  heavy_log "Upstream directory served at $HEAVY_UPSTREAM_URL"
  return 0
}

upstream_requests() {  # [regex] → count of GET lines so far
  # Without a regex, every path but `/` itself: the root is what
  # `upstream_serve` probed for readiness, and it is not a client's request.
  local re="${1:-[^ ]}"
  grep -c "\"GET /${re}" "$UPSTREAM_LOG" 2>/dev/null || true
  return 0
}
