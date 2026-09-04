#!/usr/bin/env bash
# Heavy mise integration test — the real mise against a GitHub forge registry.
#
# RFC 0019 §10: no heavy suite drove a forge before this one, and the forge
# clients are the ones most deployments use. What this proves, on the wire (the
# tap sits between the client and BatleHub and records the client's requests and
# the two RFC 0019 response headers):
#
#   1. `mise install github:cli/cli@<v>` installs a tool from a GitHub *release*
#      through the proxy — release JSON and the asset bytes both cross the tap,
#      rewritten there by mise's own `[settings.url_replacements]` — and the
#      installed `gh --version` answers. The asset response says
#      `X-BatleHub-Ref-Kind: tag` and names the commit the tag resolved to.
#   2. A source tarball of a *branch* answers `X-BatleHub-Ref-Kind: branch`
#      with the head commit; the same tarball at a *tag* answers `tag`; at a
#      full commit SHA it answers `commit`. Every forge response carries both
#      headers.
#   3. A second pull of the branch, after its TTL, is served from the cache
#      the first one filled — the cache is keyed by the commit, so the
#      counter moves — and reports the same commit.
#
# What this does *not* prove, and why: the moved-tag refusal and the
# `MUTABLE_REF` verdict are RFC 0019 phase 2, and there is no branch on the
# public repository this suite may move. The phase-1 claim is the identity —
# ref in, commit out, cache keyed by it — and that is what is asserted.
#
# Run via `task test:mise-heavy` or directly. Needs network: the upstream is
# api.github.com, anonymously (60 requests/hour; this run spends about eight).
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8101), HEAVY_TAP_PORT
# (8111), COVERAGE, HEAVY_MISE_TOOL (github:cli/cli), HEAVY_MISE_VERSION (2.60.0),
# HEAVY_MISE_BRANCH (trunk — cli/cli's default branch).
#
# The `github:` backend, not the deprecated `ubi:` one: ubi downloads the asset
# with its own HTTP client, which mise's `url_replacements` never see, so the
# bytes went straight to github.com and the run proved nothing. mise says as
# much at install time ("The ubi backend is deprecated. Use the github backend").

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init mise 8101 8111
heavy_need curl "curl"
heavy_need python3 "python3 (the wire tap)"

REG="github-$HEAVY_RUN"
TOOL="${HEAVY_MISE_TOOL:-github:cli/cli}"
VERSION="${HEAVY_MISE_VERSION:-2.60.0}"
BRANCH="${HEAVY_MISE_BRANCH:-trunk}"
OWNER_REPO="${TOOL#github:}"
# The binary inside cli/cli's archive is `gh`, not `cli`: told to mise as a
# tool option, the way its own docs spell it.
TOOL_SPEC="${TOOL}[exe=gh]@$VERSION"

# mise is a binary, so `heavy_need` applies — through the runner probe, because
# this repository pins tools per directory through mise itself.
heavy_runner_for mise "mise@latest"
MISE=("${HEAVY_RUNNER[@]}" mise)

heavy_start_server tests/heavy/config.mise.toml
heavy_start_tap

PROXY="$HEAVY_TAP_BASE/proxy/$REG"

# ── 0. mise, redirected into the run ─────────────────────────────────────────
#
# Its data, cache and config all live under the work dir, so the suite cannot
# install a tool over the runner's own and two matrix jobs cannot collide. The
# url_replacements are the ones the console's Setup Guide and `batlehub-cli
# registry suggest` generate for a github registry, with the proxy base
# substituted.
export MISE_DATA_DIR="$HEAVY_WORK/mise/data"
export MISE_CACHE_DIR="$HEAVY_WORK/mise/cache"
export MISE_CONFIG_DIR="$HEAVY_WORK/mise/config"
export MISE_STATE_DIR="$HEAVY_WORK/mise/state"
export MISE_YES=1
export MISE_TRUSTED_CONFIG_PATHS="$HEAVY_WORK"
# No token: this suite is the anonymous path. A GITHUB_TOKEN in the runner's
# environment would send mise straight past the proxy's identity.
unset GITHUB_TOKEN GH_TOKEN MISE_GITHUB_TOKEN
mkdir -p "$MISE_DATA_DIR" "$MISE_CACHE_DIR" "$MISE_CONFIG_DIR" "$MISE_STATE_DIR"

cat > "$MISE_CONFIG_DIR/config.toml" <<EOF
[settings.url_replacements]
# API (release listings, tag metadata, asset lists)
"regex:^https://api\\\\.github\\\\.com/repos/(.+)" = "$PROXY/\$1"
# Release asset binaries (browser_download_url from API responses)
"regex:^https://github\\\\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)" = "$PROXY/\$1/\$2/releases/download/\$3/\$4"
# Source tarballs
"regex:^https://github\\\\.com/([^/]+)/([^/]+)/archive/(?:refs/tags/)?(.+?)\\\\.tar\\\\.gz" = "$PROXY/\$1/\$2/tarball/\$3"
"regex:^https://codeload\\\\.github\\\\.com/([^/]+)/([^/]+)/tar\\\\.gz/(?:refs/tags/)?(.+)" = "$PROXY/\$1/\$2/tarball/\$3"
EOF
heavy_log "mise $("${MISE[@]}" --version 2>/dev/null | head -1), config at $MISE_CONFIG_DIR/config.toml"

# ── 1. Install a tool from a GitHub release through the proxy ────────────────

heavy_mark "install"
heavy_log "mise install $TOOL_SPEC"
"${MISE[@]}" install "$TOOL_SPEC" >"$HEAVY_WORK/install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/install.txt" >&2; heavy_fail "mise install $TOOL_SPEC failed"; }
# The release by tag is the first thing the github backend asks for, and the
# route that answered 500 to every real client until this suite existed.
heavy_wire_after "install" "GET /proxy/$REG/$OWNER_REPO/releases/tags/v$VERSION -> 200" \
  "mise did not read the release through the proxy — url_replacements did not apply to the API call, or the route failed"
# The release is a tag coordinate: resolved to the tag's commit, and both
# headers say so on the JSON (RFC 0019 §4.2 *Response headers*).
REL_LINE="$(grep -E "GET /proxy/$REG/$OWNER_REPO/releases/tags/v$VERSION -> 200" "$HEAVY_LOG" | head -1)"
[[ "$REL_LINE" == *"X-BatleHub-Ref-Kind: tag"* ]] \
  || heavy_fail "the release response did not say X-BatleHub-Ref-Kind: tag: $REL_LINE"
[[ "$REL_LINE" =~ X-BatleHub-Resolved-Commit:\ [0-9a-f]{40} ]] \
  || heavy_fail "the release response did not name the commit the tag resolved to: $REL_LINE"
# The asset bytes: mise 2026.8 asks for the asset by name first (a HEAD, which
# the artifact routes do not answer) and then by id (`releases/assets/{id}`),
# which carries no tag and so no ref headers — the client derives the release
# from the asset itself. Either shape must have crossed the tap with a 200.
grep -E "GET /proxy/$REG/$OWNER_REPO/releases/(download/v$VERSION/[^ ]*\.tar\.gz|assets/[0-9]+) -> 200" "$HEAVY_LOG" >/dev/null \
  || heavy_fail "the asset was not downloaded through the proxy"

INSTALLED="$("${MISE[@]}" exec "$TOOL_SPEC" -- gh --version 2>/dev/null | head -1)"
[[ "$INSTALLED" == *"$VERSION"* ]] || heavy_fail "gh --version answered '$INSTALLED', expected $VERSION"
heavy_log "MISE-INSTALL-OK ($INSTALLED, through the proxy)"

# ── 2. Ref kinds on source archives ──────────────────────────────────────────
#
# curl, not mise: mise has no reason to fetch a branch of this repository, and
# what is asserted here is the proxy's identity model, which every client sees
# the same way. `-o /dev/null`: a cli/cli source tarball is a few megabytes.

heavy_mark "refs"
heavy_log "tarball of branch '$BRANCH', tag v$VERSION, then the resolved commit"
curl -fsS -o /dev/null -D "$HEAVY_WORK/branch.h" "$PROXY/$OWNER_REPO/tarball/$BRANCH" \
  || heavy_fail "tarball/$BRANCH was not served"
BRANCH_SHA="$(awk 'tolower($1)=="x-batlehub-resolved-commit:" {print $2}' "$HEAVY_WORK/branch.h" | tr -d '\r')"
heavy_wire_after "refs" "GET /proxy/$REG/$OWNER_REPO/tarball/$BRANCH -> 200" \
  "the branch tarball did not cross the tap"
grep -E "GET /proxy/$REG/$OWNER_REPO/tarball/$BRANCH -> 200 .*X-BatleHub-Ref-Kind: branch" "$HEAVY_LOG" >/dev/null \
  || heavy_fail "tarball/$BRANCH did not say X-BatleHub-Ref-Kind: branch"
[[ "$BRANCH_SHA" =~ ^[0-9a-f]{40}$ ]] || heavy_fail "no resolved commit on the branch tarball (got '$BRANCH_SHA')"

curl -fsS -o /dev/null "$PROXY/$OWNER_REPO/tarball/v$VERSION" \
  || heavy_fail "tarball/v$VERSION was not served"
grep -E "GET /proxy/$REG/$OWNER_REPO/tarball/v$VERSION -> 200 .*X-BatleHub-Ref-Kind: tag" "$HEAVY_LOG" >/dev/null \
  || heavy_fail "tarball/v$VERSION did not say X-BatleHub-Ref-Kind: tag"

curl -fsS -o /dev/null "$PROXY/$OWNER_REPO/tarball/$BRANCH_SHA" \
  || heavy_fail "tarball/$BRANCH_SHA was not served"
grep -E "GET /proxy/$REG/$OWNER_REPO/tarball/$BRANCH_SHA -> 200 .*X-BatleHub-Ref-Kind: commit" "$HEAVY_LOG" >/dev/null \
  || heavy_fail "tarball/<sha> did not say X-BatleHub-Ref-Kind: commit"
grep -E "GET /proxy/$REG/$OWNER_REPO/tarball/$BRANCH_SHA -> 200 .*X-BatleHub-Resolved-Commit: $BRANCH_SHA" "$HEAVY_LOG" >/dev/null \
  || heavy_fail "tarball/<sha> did not resolve to itself"
heavy_log "MISE-REFS-OK (branch → $BRANCH_SHA, tag and commit each named)"

# ── 3. The branch is cached by its commit ────────────────────────────────────
#
# After the branch TTL the proxy re-resolves; the commit has not moved, so the
# second pull lands on the entry the first one wrote. The tap cannot see the
# upstream side, so the cache-hit counter is the witness — as in nvm.sh.

hits_for() {
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk -v reg="$REG" '$1 ~ /^batlehub_artifact_cache_hits_total\{/ && index($1, "registry=\"" reg "\"") { print $2 }'
}
HITS_BEFORE="$(hits_for)"; HITS_BEFORE="${HITS_BEFORE:-0}"
sleep 11   # > refs.branch_ttl_secs in config.mise.toml
heavy_mark "cache"
curl -fsS -o /dev/null -D "$HEAVY_WORK/branch2.h" "$PROXY/$OWNER_REPO/tarball/$BRANCH" \
  || heavy_fail "the second tarball/$BRANCH was not served"
BRANCH_SHA2="$(awk 'tolower($1)=="x-batlehub-resolved-commit:" {print $2}' "$HEAVY_WORK/branch2.h" | tr -d '\r')"
HITS_AFTER="$(hits_for)"; HITS_AFTER="${HITS_AFTER:-0}"
if [[ "$BRANCH_SHA2" == "$BRANCH_SHA" ]]; then
  [[ "${HITS_AFTER%.*}" -gt "${HITS_BEFORE%.*}" ]] \
    || heavy_fail "the branch resolved to the same commit but the second pull was not a cache hit ($HITS_BEFORE -> $HITS_AFTER)"
  heavy_log "MISE-CACHE-OK (same commit, cache hits $HITS_BEFORE -> $HITS_AFTER)"
else
  # The default branch moved between the two pulls — a real push on cli/cli.
  # That is the other half of the design working: a new commit is a new entry.
  heavy_log "MISE-CACHE-OK (branch moved $BRANCH_SHA -> $BRANCH_SHA2 during the run; a new commit is a new entry)"
fi

heavy_done MISE-HEAVY-OK
