#!/usr/bin/env bash
# `mode = "hybrid"` under real clients: local-first, upstream for the rest.
#
# Sixteen in-process tests know the rule; no client had met it. The rule, as
# `serve_local_or_proxy_document` and `serve_local_or_proxy_artifact` state
# it: a name published here is served from here and **shadows the upstream
# entirely**; a name absent here falls through to the upstream; and a local
# package whose every version is blocked is refused, never replaced by the
# upstream's copy of the same name (blocked_versions_hidden.rs). The upstream
# is a directory the suite serves, so "never asked" is a count of zero.
#
#   npm   three names in one `npm install`: `private` exists only here,
#         `upstream-only` exists only upstream, `shadowed` exists in both at
#         different versions. The install resolves all three; the upstream
#         served the second and was never asked for the other two; `npm view
#         shadowed versions` is the local version alone. Then the upstream
#         package's one version is blocked: a fresh install fails as npm's
#         "no matching version" with the upstream never asked for its
#         tarball; and the local package's one version is blocked: refused
#         `403`, with the upstream — which has a `shadowed@1.0.0` it would
#         happily serve — never asked.
#   pip   the same three shapes on the PyPI registry, published with the
#         legacy upload (what twine sends), installed with pip.
#
# Ports: 8150 (server), 8151 (tap), 8152 (upstream). Environment:
# DATABASE_URL (required); HEAVY_PORT, HEAVY_TAP_PORT, HEAVY_UPSTREAM_PORT;
# COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
heavy_init hybrid 8150 8151
heavy_need npm "npm"
heavy_need node "nodejs"
heavy_need python3 "python3"
heavy_need curl "curl"

HEAVY_UPSTREAM_PORT="${HEAVY_UPSTREAM_PORT:-8152}"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/upstream_dir.sh"

REG_NPM="npm-$HEAVY_RUN"
REG_PYPI="pypi-$HEAVY_RUN"
PRIVATE="heavy-hybrid-private"
UPSTREAM_ONLY="heavy-hybrid-upstream-only"
SHADOWED="heavy-hybrid-shadowed"
DIST_PRIVATE="hybridprivate"
DIST_UPSTREAM="hybridupstream"
DIST_SHADOWED="hybridshadowed"

# ── 0. The upstream: two npm packages and two distributions ─────────────────

upstream_npm_package "$UPSTREAM_ONLY" "1.0.0"
upstream_npm_package "$SHADOWED" "1.0.0"
upstream_pypi_dist "$DIST_UPSTREAM" "$DIST_UPSTREAM" "1.0.0"
upstream_pypi_dist "$DIST_SHADOWED" "$DIST_SHADOWED" "1.0.0"
upstream_serve

heavy_start_server tests/heavy/config.hybrid.toml
heavy_start_tap
NPM_URL="$HEAVY_TAP_BASE/proxy/$REG_NPM/"
PYPI_SIMPLE="$HEAVY_TAP_BASE/proxy/$REG_PYPI/simple/"
PYPI_UPLOAD="$HEAVY_TAP_BASE/proxy/$REG_PYPI/legacy/"

export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false
NPMRC="$HEAVY_WORK/npmrc"
cat > "$NPMRC" <<RC
registry=$NPM_URL
//127.0.0.1:$HEAVY_TAP_PORT/proxy/$REG_NPM/:_authToken=$ADMIN_TOKEN
RC
export NPM_CONFIG_USERCONFIG="$NPMRC"

# ── 1. Publish what is ours ─────────────────────────────────────────────────

npm_publish() {  # <name> <version>
  local name="$1" version="$2"
  local dir="$HEAVY_WORK/publish-$name-$version"
  mkdir -p "$dir"
  printf '{ "name": "%s", "version": "%s", "description": "heavy hybrid local", "license": "MIT", "main": "index.js" }\n' "$name" "$version" >"$dir/package.json"
  echo "module.exports = { name: '$name', version: '$version', origin: 'local' };" >"$dir/index.js"
  (cd "$dir" && NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-publish" npm publish --registry "$NPM_URL" >"$HEAVY_WORK/publish-$name.txt" 2>&1) \
    || { cat "$HEAVY_WORK/publish-$name.txt" >&2; heavy_fail "npm publish $name@$version failed"; }
  return 0
}
npm_publish "$PRIVATE" "1.0.0"
npm_publish "$SHADOWED" "2.0.0"
heavy_wire "PUT /proxy/$REG_NPM/$PRIVATE -> 200" "the private package did not reach the publish route"
heavy_log "PUBLISH-OK ($PRIVATE@1.0.0 and $SHADOWED@2.0.0 published here; the upstream has $UPSTREAM_ONLY@1.0.0 and $SHADOWED@1.0.0)"

# ── 2. One install, three origins ───────────────────────────────────────────

npm_project() {  # <label> <dep>… → installs deps into a fresh project with a fresh cache
  local label="$1"; shift
  local proj="$HEAVY_WORK/proj-$label"
  mkdir -p "$proj"
  printf '{ "name": "consumer-%s", "version": "1.0.0", "private": true }\n' "$label" >"$proj/package.json"
  (cd "$proj" && NPM_CONFIG_CACHE="$HEAVY_WORK/npm-cache-$label" npm install --ignore-scripts "$@" >"$HEAVY_WORK/npm-$label.txt" 2>&1)
  return $?
}
origin_of() {  # <label> <name> → the installed module's origin and version
  local label="$1" name="$2"
  node -e "const m=require('$HEAVY_WORK/proj-$label/node_modules/$name'); console.log(m.origin + ' ' + m.version)"
  return $?
}

# Counts are taken per phase, as deltas: `npm publish` reads the packument
# before it writes, and at that moment nothing is local, so the upstream is
# rightly asked for the names about to be published — a total would carry
# that into the install's verdict. The packument count is not pinned to one:
# a fall-through resolves it more than once per install (three, measured —
# the document route and the artifact route each ask), which is an
# inefficiency for another day; what is pinned is *whether* the upstream was
# asked, and that a name held here is never asked for.
snap() { S_UP="$(upstream_requests "$UPSTREAM_ONLY ")"; S_UP_TGZ="$(upstream_requests "tarballs/$UPSTREAM_ONLY-")"; S_PRIV="$(upstream_requests "$PRIVATE")"; S_SHA="$(upstream_requests "$SHADOWED")"; return 0; }
delta() {  # <regex> <count at snap> → requests since
  local re="$1" before="$2"
  echo $(( $(upstream_requests "$re") - before ))
  return 0
}

snap
heavy_mark "install-three"
npm_project three "$PRIVATE" "$UPSTREAM_ONLY" "$SHADOWED" \
  || { cat "$HEAVY_WORK/npm-three.txt" >&2; tail -30 "$HEAVY_WORK/server.log" >&2; heavy_fail "npm install of the three packages failed"; }
[[ "$(origin_of three "$PRIVATE")" == "local 1.0.0" ]] || heavy_fail "$PRIVATE did not come from here: $(origin_of three "$PRIVATE")"
[[ "$(origin_of three "$UPSTREAM_ONLY")" == "upstream 1.0.0" ]] || heavy_fail "$UPSTREAM_ONLY did not come from the upstream: $(origin_of three "$UPSTREAM_ONLY")"
[[ "$(origin_of three "$SHADOWED")" == "local 2.0.0" ]] || heavy_fail "$SHADOWED must be the local 2.0.0, got: $(origin_of three "$SHADOWED")"
UP_PACKUMENTS="$(delta "$UPSTREAM_ONLY " "$S_UP")"
[[ "$UP_PACKUMENTS" -ge 1 && "$(delta "tarballs/$UPSTREAM_ONLY-" "$S_UP_TGZ")" == "1" ]] \
  || { cat "$UPSTREAM_LOG" >&2; heavy_fail "the upstream was not asked for $UPSTREAM_ONLY's packument and its tarball exactly once"; }
[[ "$(delta "$PRIVATE" "$S_PRIV")" == "0" ]] || heavy_fail "the upstream was asked for $PRIVATE, which is ours"
[[ "$(delta "$SHADOWED" "$S_SHA")" == "0" ]] || heavy_fail "the upstream was asked for $SHADOWED, which is ours — local-first did not shadow it"
heavy_wire_after "install-three" "GET /proxy/$REG_NPM/$UPSTREAM_ONLY -> 200" "the fall-through packument was not served"
heavy_log "THREE-ORIGINS-OK (one install: ours from here, the upstream's from the upstream ($UP_PACKUMENTS packument read(s), 1 tarball), the shadowed name ours at 2.0.0 with the upstream never asked)"

snap
VIEWED="$(npm view "$SHADOWED" versions --json --registry "$NPM_URL" 2>/dev/null | tr -d '[:space:]')"
[[ "$VIEWED" == '"2.0.0"' || "$VIEWED" == '["2.0.0"]' ]] || heavy_fail "npm view $SHADOWED versions is $VIEWED, expected the local 2.0.0 alone"
[[ "$(delta "$SHADOWED" "$S_SHA")" == "0" ]] || heavy_fail "npm view reached the upstream for the shadowed name"
heavy_log "SHADOW-OK (the packument of a name we hold lists our versions only; the upstream's 1.0.0 is invisible)"

# ── 3. Blocks: on the upstream's package, and on ours ───────────────────────

heavy_block "$REG_NPM" "$UPSTREAM_ONLY" "1.0.0"
snap
heavy_mark "blocked-upstream"
if npm_project blocked-upstream "$UPSTREAM_ONLY"; then heavy_fail "npm installed the blocked $UPSTREAM_ONLY@1.0.0"; fi
# npm 11 says ENOVERSIONS for a packument with no version left; older npm
# said ETARGET / "No matching version".
grep -qi "ENOVERSIONS\|No versions available\|No matching version\|ETARGET\|notarget" "$HEAVY_WORK/npm-blocked-upstream.txt" \
  || { cat "$HEAVY_WORK/npm-blocked-upstream.txt" >&2; heavy_fail "npm failed for a reason other than the version being gone from the listing"; }
[[ "$(delta "tarballs/$UPSTREAM_ONLY-" "$S_UP_TGZ")" == "0" ]] || heavy_fail "the blocked tarball was fetched from the upstream again"
heavy_log "BLOCK-UPSTREAM-OK (a proxied version blocked: hidden from the packument, npm reports no matching version, the tarball never asked for)"

heavy_block "$REG_NPM" "$SHADOWED" "2.0.0"
snap
heavy_mark "blocked-local"
if npm_project blocked-local "$SHADOWED"; then heavy_fail "npm installed $SHADOWED after its only local version was blocked — it fell through to the upstream's 1.0.0"; fi
heavy_wire_re_after "blocked-local" "GET /proxy/$REG_NPM/$SHADOWED -> 403" "an all-blocked local package must answer 403, not fall through"
[[ "$(delta "$SHADOWED" "$S_SHA")" == "0" ]] || heavy_fail "the upstream was asked for $SHADOWED once ours was blocked — the block became a fall-through"
heavy_log "BLOCK-LOCAL-OK (our only version blocked: 403, and the upstream's same-named 1.0.0 never asked for)"

# ── 4. pip, the same three shapes ───────────────────────────────────────────

pypi_publish() {  # <dist> <version> — the legacy upload, as twine sends it
  local dist="$1" version="$2"
  local wheel="$HEAVY_WORK/$dist-$version-py3-none-any.whl"
  python3 - "$dist" "$dist" "$version" "$wheel" <<'PY'
import base64, hashlib, sys, zipfile
dist, module, version, out = sys.argv[1:]
info = f"{dist}-{version}.dist-info"
files = {
    f"{module}/__init__.py": f"VALUE = '{dist}'\nORIGIN = 'local'\n",
    f"{info}/METADATA": f"Metadata-Version: 2.1\nName: {dist}\nVersion: {version}\nSummary: heavy hybrid local\n",
    f"{info}/WHEEL": "Wheel-Version: 1.0\nGenerator: batlehub-heavy\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    f"{info}/top_level.txt": module + "\n",
}
record = []
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for path, text in files.items():
        data = text.encode(); z.writestr(path, data)
        record.append(f"{path},sha256={base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b'=').decode()},{len(data)}")
    record.append(f"{info}/RECORD,,")
    z.writestr(f"{info}/RECORD", "\n".join(record) + "\n")
PY
  curl -fsS -u "__token__:$ADMIN_TOKEN" -F ":action=file_upload" -F "protocol_version=1" \
    -F "name=$dist" -F "version=$version" -F "filetype=bdist_wheel" -F "pyversion=py3" -F "metadata_version=2.1" \
    -F "sha256_digest=$(sha256sum "$wheel" | cut -d' ' -f1)" -F "content=@$wheel" \
    "$PYPI_UPLOAD" >"$HEAVY_WORK/pypi-publish-$dist.txt" 2>&1 \
    || { cat "$HEAVY_WORK/pypi-publish-$dist.txt" >&2; heavy_fail "the legacy upload of $dist $version failed"; }
  return 0
}
pypi_publish "$DIST_PRIVATE" "1.0.0"
pypi_publish "$DIST_SHADOWED" "2.0.0"
heavy_wire "POST /proxy/$REG_PYPI/legacy/ -> 20" "the legacy upload did not reach the publish route"

pip_project() {  # <label> <requirement>… → a fresh venv
  local label="$1"; shift
  local venv="$HEAVY_WORK/venv-$label"
  python3 -m venv "$venv" >/dev/null || heavy_fail "python3 -m venv failed"
  "$venv/bin/python" -m pip install --quiet --no-cache-dir --index-url "$PYPI_SIMPLE" "$@" >"$HEAVY_WORK/pip-$label.txt" 2>&1
  return $?
}
pip_origin_of() {  # <label> <module>
  local label="$1" module="$2"
  "$HEAVY_WORK/venv-$label/bin/python" -c "import $module, importlib.metadata as m; print($module.ORIGIN, m.version('$module'))"
  return $?
}
P_UP="$(upstream_requests "simple/$DIST_UPSTREAM/")"; P_PRIV="$(upstream_requests "simple/$DIST_PRIVATE/")"; P_SHA="$(upstream_requests "simple/$DIST_SHADOWED/")"
heavy_mark "pip-three"
pip_project three "$DIST_PRIVATE" "$DIST_UPSTREAM" "$DIST_SHADOWED" \
  || { cat "$HEAVY_WORK/pip-three.txt" >&2; heavy_fail "pip install of the three distributions failed"; }
[[ "$(pip_origin_of three "$DIST_PRIVATE")" == "local 1.0.0" ]] || heavy_fail "$DIST_PRIVATE did not come from here: $(pip_origin_of three "$DIST_PRIVATE")"
[[ "$(pip_origin_of three "$DIST_UPSTREAM")" == "upstream 1.0.0" ]] || heavy_fail "$DIST_UPSTREAM did not come from the upstream: $(pip_origin_of three "$DIST_UPSTREAM")"
[[ "$(pip_origin_of three "$DIST_SHADOWED")" == "local 2.0.0" ]] || heavy_fail "$DIST_SHADOWED must be the local 2.0.0, got: $(pip_origin_of three "$DIST_SHADOWED")"
[[ "$(delta "simple/$DIST_UPSTREAM/" "$P_UP")" -ge 1 ]] || { cat "$UPSTREAM_LOG" >&2; heavy_fail "the upstream was not asked for $DIST_UPSTREAM's page"; }
[[ "$(delta "simple/$DIST_PRIVATE/" "$P_PRIV")" == "0" && "$(delta "simple/$DIST_SHADOWED/" "$P_SHA")" == "0" ]] \
  || heavy_fail "the upstream was asked for a distribution we hold"
heavy_log "PIP-THREE-ORIGINS-OK (ours from here, the upstream's from the upstream, the shadowed name ours at 2.0.0)"

heavy_block "$REG_PYPI" "$DIST_SHADOWED" "2.0.0"
P_SHA="$(upstream_requests "simple/$DIST_SHADOWED/")"
heavy_mark "pip-blocked-local"
if pip_project blocked-local "$DIST_SHADOWED"; then heavy_fail "pip installed $DIST_SHADOWED after its only local version was blocked — it fell through to the upstream's 1.0.0"; fi
[[ "$(delta "simple/$DIST_SHADOWED/" "$P_SHA")" == "0" ]] || heavy_fail "the upstream was asked for $DIST_SHADOWED once ours was blocked"
heavy_log "PIP-BLOCK-LOCAL-OK (our only version blocked: refused, the upstream's same-named 1.0.0 never asked for)"

heavy_log "Measurement: hybrid npm and PyPI registries, upstream requests in the run: $(upstream_requests) — publishes read the packument before writing, installs asked for $UPSTREAM_ONLY and $DIST_UPSTREAM, and nothing held here was asked for after it was published"
heavy_done HYBRID-HEAVY-OK
