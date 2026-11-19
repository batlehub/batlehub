#!/usr/bin/env bash
# Heavy PyPI integration test — twine and pip, against the real clients.
#
# RFC 0009 §12.7 found the worst defect of the whole verification here, and
# found it by accident: pip was installed, so it was cheap to try. BatleHub's
# simple-page rewrite touches `href` and nothing else, so upstream's
# `data-core-metadata` attribute survived onto our page — pip trusted it,
# requested `{file}.metadata`, got a `404`, and **did not fall back to
# downloading the wheel**. §7.6 had called PEP 658 "a silent slowdown rather
# than an error". It was a hard failure, and against real PyPI, where
# essentially every modern wheel carries the attribute, it breaks `pip install`
# outright.
#
# What this proves:
#
#   1. `twine upload` publishes to a **local** registry over the documented
#      flow — `-u __token__ -p <token>`, which is HTTP Basic, not Bearer.
#      docs/registries/pypi.md tells users to do exactly this, and nothing in
#      the repository ran it.
#   2. `pip install` from that registry gets the distribution back through the
#      simple index and the rewritten file link.
#   3. Against a **proxy** registry, pip requests the PEP 658 sibling and gets
#      it — the §12.7 regression. The assertion is on pip's own request, not on
#      a `curl` of the same URL: curl served that document perfectly throughout
#      the period the client was failing (§5.2 is a list of ways that happens).
#
# Run via `task test:pypi-heavy` or directly. Needs network: the proxy
# registry's upstream is pypi.org, and PEP 658 is upstream's advertisement.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8083),
# HEAVY_TAP_PORT (8093), COVERAGE, HEAVY_PYPI_PROBE (default six==1.17.0).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init pypi 8083 8093
heavy_need python3 "python3 with venv"

LOCAL="pypi-local-$HEAVY_RUN"
PROXY="pypi-proxy-$HEAVY_RUN"
# Underscores in the module, hyphens in the distribution: pip normalises the
# name on the way to the index, so this also pins that the index answers the
# normalised form rather than the one that was published.
DIST="heavy-probe-$HEAVY_RUN"
MODULE="heavy_probe_$HEAVY_RUN"
# A dependency-free upstream wheel, recent enough that pypi.org advertises its
# core metadata. Overridable, because that advertisement is upstream's to make.
PROBE="${HEAVY_PYPI_PROBE:-six==1.17.0}"
PROBE_NAME="${PROBE%%==*}"
PROBE_VERSION="${PROBE##*==}"

heavy_start_server tests/heavy/config.pypi.toml
heavy_start_tap

LOCAL_SIMPLE="$HEAVY_TAP_BASE/proxy/$LOCAL/simple/"
PROXY_SIMPLE="$HEAVY_TAP_BASE/proxy/$PROXY/simple/"
UPLOAD_URL="$HEAVY_TAP_BASE/proxy/$LOCAL/legacy/"

# ── 0. A build/publish virtualenv ────────────────────────────────────────────
#
# Installed from pypi.org directly, not through the proxy: these are the tools
# doing the measuring, and routing them through the thing under test would make
# a broken proxy look like a broken toolchain.

heavy_log "Creating the build virtualenv ($(python3 --version))"
python3 -m venv "$HEAVY_WORK/venv" || heavy_fail "python3 -m venv failed (python3-venv missing?)"
VPY="$HEAVY_WORK/venv/bin/python"
"$VPY" -m pip install --quiet --upgrade pip setuptools wheel build twine \
  || heavy_fail "could not install the build toolchain"
heavy_log "pip $("$VPY" -m pip --version | cut -d' ' -f2), twine $("$HEAVY_WORK/venv/bin/twine" --version | head -1)"

# ── 1. Build a wheel and publish it with twine ───────────────────────────────

SRC="$HEAVY_WORK/src"
mkdir -p "$SRC/$MODULE"
cat > "$SRC/pyproject.toml" <<EOF
[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"

[project]
name = "$DIST"
version = "1.0.0"
description = "RFC 0009 heavy test probe"
requires-python = ">=3.8"

[tool.setuptools]
packages = ["$MODULE"]
EOF
echo "VALUE = '$DIST'" > "$SRC/$MODULE/__init__.py"

heavy_log "Building the wheel and the sdist"
(cd "$SRC" && "$VPY" -m build --wheel --sdist --no-isolation) >"$HEAVY_WORK/build.log" 2>&1 \
  || { tail -30 "$HEAVY_WORK/build.log" >&2; heavy_fail "python -m build failed"; }
WHEEL="$(ls "$SRC"/dist/*.whl)"
SDIST="$(ls "$SRC"/dist/*.tar.gz)"
heavy_log "Built $(basename "$WHEEL") and $(basename "$SDIST")"

heavy_log "twine upload -> $UPLOAD_URL"
TWINE_USERNAME="__token__" TWINE_PASSWORD="$ADMIN_TOKEN" \
  "$HEAVY_WORK/venv/bin/twine" upload --repository-url "$UPLOAD_URL" \
  --disable-progress-bar "$WHEEL" "$SDIST" \
  || heavy_fail "twine upload failed — the documented publish flow sends HTTP Basic, not Bearer"
heavy_wire "POST /proxy/$LOCAL/legacy/ -> 20" "twine did not reach the legacy upload endpoint"

# ── 2. pip install it back, from a clean environment ─────────────────────────

heavy_mark "local-install"
heavy_log "pip install $DIST from the local registry"
python3 -m venv "$HEAVY_WORK/consumer" || heavy_fail "consumer venv failed"
CPY="$HEAVY_WORK/consumer/bin/python"
"$CPY" -m pip install --quiet --no-cache-dir --index-url "$LOCAL_SIMPLE" "$DIST==1.0.0" \
  || heavy_fail "pip install from the local registry failed"
"$CPY" -c "import $MODULE; assert ${MODULE}.VALUE == '$DIST'" \
  || heavy_fail "the installed distribution is not the one that was published"
heavy_wire_after "local-install" "GET /proxy/$LOCAL/simple/$DIST/ -> 200" \
  "pip did not read the simple page (a normalisation mismatch looks like this)"
# The file link came out of the page BatleHub rendered; a request for it landing
# here is the rewrite pointing at the proxy rather than at files.pythonhosted.org.
grep -q "GET /proxy/$LOCAL/packages/.*\.whl -> 200" "$HEAVY_LOG" \
  || heavy_fail "the wheel was not fetched through the proxy"
# Two files of one release, as twine sends them: two POSTs to one version. The
# second used to be a 409, so a release could carry its wheel or its sdist and
# never both.
heavy_mark "local-sdist"
"$VPY" -m pip download --quiet --no-cache-dir --no-deps --no-build-isolation \
  --no-binary "$DIST" --index-url "$LOCAL_SIMPLE" -d "$HEAVY_WORK/sdist-dl" "$DIST==1.0.0" \
  || heavy_fail "pip could not fetch the sdist published beside the wheel"
cmp -s "$SDIST" "$HEAVY_WORK/sdist-dl/$(basename "$SDIST")" \
  || heavy_fail "the sdist pip fetched is not the one twine uploaded"
heavy_wire_after "local-sdist" "/$(basename "$SDIST") -> 200" \
  "the sdist was not fetched through the proxy"
heavy_log "PYPI-LOCAL-OK (twine upload of wheel + sdist, pip install, links rewritten)"

# ── 3. PEP 658 — the sibling pip is told exists ──────────────────────────────

heavy_mark "pep658"
heavy_log "pip install $PROBE through the proxy registry"
python3 -m venv "$HEAVY_WORK/proxied" || heavy_fail "proxied venv failed"
PPY="$HEAVY_WORK/proxied/bin/python"
"$PPY" -m pip install --no-cache-dir --index-url "$PROXY_SIMPLE" "$PROBE" \
  >"$HEAVY_WORK/pip-proxy.log" 2>&1 \
  || { tail -30 "$HEAVY_WORK/pip-proxy.log" >&2; heavy_fail "pip install through the proxy failed"; }

# The §12.7 signature — asked of the wire, which is the only place the request
# and its status are on the same line.
#
# This was two independent greps over pip's log, `.metadata` **and** `404`,
# matching anywhere in the file and never against each other. `HEAVY_RUN` is
# `date +%H%M%S` and it is in every URL pip prints, so the run that started at
# 22:14:04 matched `404` inside its own run id `221404` and failed a suite whose
# sibling had answered `200` on the transcript directly above the failure. Even
# per-line the pip log cannot carry this assertion: the run id sits in the same
# URL as `.metadata`, so any regex relating the two matches it.
#
# A status assertion has to be anchored to the request it is about. On the
# transcript that anchor is the `pep658` mark, so every check in this phase is
# scoped to it: an earlier phase's `.metadata` request is not this phase's
# result, and a section inserted above must not be able to fail — or satisfy —
# the assertions below.
if heavy_wire_seen_after "pep658" ".metadata -> 404"; then
  cat "$HEAVY_WORK/pip-proxy.log" >&2
  heavy_fail "a .metadata sibling answered 404 — pip does not fall back to the wheel when the \
page advertised one, so this is §12.7's signature: core metadata advertised and not served"
fi

if ! heavy_wire_seen_after "pep658" ".metadata -> 200"; then
  # Either the sibling is broken again, or upstream stopped advertising the
  # attribute for this distribution — which is a real change in the premise and
  # must be looked at, not skipped past. Say which by asking the page.
  heavy_log "no .metadata request observed; checking whether upstream still advertises it"
  curl -fsS "$PROXY_SIMPLE$PROBE_NAME/" > "$HEAVY_WORK/simple.html" || true
  if grep -q "data-core-metadata\|data-dist-info-metadata" "$HEAVY_WORK/simple.html"; then
    heavy_fail "the simple page advertises core metadata and pip never fetched a sibling — PEP 658 is advertised and unserved (RFC 0009 §12.7)"
  fi
  heavy_fail "upstream no longer advertises core metadata for $PROBE — set HEAVY_PYPI_PROBE to a distribution that does, rather than dropping the assertion"
fi
heavy_log "PYPI-PEP658-OK (pip fetched the metadata sibling and got it)"

heavy_done PYPI-HEAVY-OK
