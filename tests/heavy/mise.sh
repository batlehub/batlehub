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
#   4. RFC 0008's air gap, end to end: a lock is planned, the plan is seeded
#      and exported as a signed bundle, a *second* instance running
#      `[air_gap] enabled = true` imports it, and `mise install` completes
#      against that instance with egress denied to both processes. Before the
#      import the same coordinate is a `503` naming itself, which is the
#      other half of the claim: an air-gapped instance says what it lacks.
#
# What this does *not* prove, and why: the moved-tag refusal and the
# `MUTABLE_REF` verdict are RFC 0019 phase 2, and there is no branch on the
# public repository this suite may move. The phase-1 claim is the identity —
# ref in, commit out, cache keyed by it — and that is what is asserted.
#
# Run via `task test:mise-heavy` or directly. Needs network: the upstream is
# api.github.com, anonymously (60 requests/hour; this run spends about eight).
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8101), HEAVY_TAP_PORT
# (8111), HEAVY_AIRGAP_PORT (8107), COVERAGE, HEAVY_MISE_TOOL (github:cli/cli),
# HEAVY_MISE_VERSION (2.60.0), HEAVY_MISE_BRANCH (trunk — cli/cli's default
# branch).
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

# The literals this suite repeats. `%{http_code}` is the whole of every status
# assertion here, and the four proxy variables must all name the *same* closed
# port — a typo in one of them leaves that scheme reaching the real internet
# and the air-gap phase still passes.
CURL_CODE='%{http_code}'
CLOSED_PROXY="http://127.0.0.1:1"
LOOPBACK_DIRECT="127.0.0.1,localhost"
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

heavy_forge_auth_config tests/heavy/config.mise.toml
heavy_start_server "$HEAVY_CONFIG"
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

# ── 5. Raw content under a policy (RFC 0019 phase 3) ─────────────────────────
#
# The first client-side proof of the `[raw]` policy: a shell script at a
# pinned tag is refused with its reason code, and a plain file at the same
# tag is served, through the same route. Both are what a `curl | sh`
# installer line and a `curl -O README` do; mise itself never fetches raw.

heavy_mark "raw"
RAW_SCRIPT="script/createrepo.sh"
heavy_log "raw $RAW_SCRIPT at v$VERSION — refused as a script; README.md — served"
RAW_CODE="$(curl -sS -o "$HEAVY_WORK/raw-script.body" -w "$CURL_CODE" -D "$HEAVY_WORK/raw-script.h" \
  "$PROXY/$OWNER_REPO/raw/v$VERSION/$RAW_SCRIPT")"
[[ "$RAW_CODE" == "403" ]] || { cat "$HEAVY_WORK/raw-script.body" >&2; heavy_fail "raw/$RAW_SCRIPT answered $RAW_CODE, expected 403 under scripts = \"deny\""; }
grep -q "RAW_SCRIPT" "$HEAVY_WORK/raw-script.body" "$HEAVY_WORK/raw-script.h" \
  || { cat "$HEAVY_WORK/raw-script.body" "$HEAVY_WORK/raw-script.h" >&2; heavy_fail "the refusal does not name RAW_SCRIPT"; }
heavy_wire_after "raw" "GET /proxy/$REG/$OWNER_REPO/raw/v$VERSION/$RAW_SCRIPT -> 403" \
  "the script refusal did not cross the tap as a 403"
[[ ! -s "$HEAVY_WORK/raw-script.body" ]] || ! grep -q '^#!' "$HEAVY_WORK/raw-script.body" \
  || heavy_fail "the refused script's bytes were served with the refusal"

curl -fsS -o "$HEAVY_WORK/raw-readme.md" "$PROXY/$OWNER_REPO/raw/v$VERSION/README.md" \
  || heavy_fail "raw/README.md at v$VERSION was not served under the same policy"
grep -qi "gh\|github" "$HEAVY_WORK/raw-readme.md" \
  || heavy_fail "raw/README.md does not look like cli/cli's README"
heavy_wire_after "raw" "GET /proxy/$REG/$OWNER_REPO/raw/v$VERSION/README.md -> 200" \
  "the README was not served through the proxy"
heavy_log "MISE-RAW-OK ($RAW_SCRIPT refused with RAW_SCRIPT, README.md served)"

# ── 6. The typed reads, and a release document that points home ─────────────
#
# `[api_reads] families = ["tags"]` turns one typed family on; a release
# document read through the proxy has every download URL repointed at the
# proxy and the forge's own API links removed (RFC 0019 §4.2, §13.2). Both
# are what a client resolving *from* the proxy's answer needs: a URL left
# pointing at github.com is a request the proxy never sees.

heavy_mark "api-reads"
heavy_log "tags family, then the release document for v$VERSION"
curl -fsS -o "$HEAVY_WORK/tags.json" "$PROXY/$OWNER_REPO/tags" \
  || heavy_fail "the tags family was not served with [api_reads] families = [\"tags\"]"
python3 - "$HEAVY_WORK/tags.json" "v$VERSION" <<'PY' || { head -c 400 "$HEAVY_WORK/tags.json" >&2; heavy_fail "the tags document does not list v$VERSION"; }
import json, sys
tags = json.load(open(sys.argv[1]))
names = [t.get("name") for t in tags] if isinstance(tags, list) else [t.get("name") for t in tags.get("tags", [])]
sys.exit(0 if sys.argv[2] in names else 1)
PY
heavy_wire_after "api-reads" "GET /proxy/$REG/$OWNER_REPO/tags -> 200" "the tags read did not cross the tap"
COMMITS_CODE="$(curl -sS -o /dev/null -w "$CURL_CODE" "$PROXY/$OWNER_REPO/commits/$BRANCH_SHA")"
[[ "$COMMITS_CODE" == "404" || "$COMMITS_CODE" == "403" ]] \
  || heavy_fail "the commits family is not enabled and answered $COMMITS_CODE rather than refusing"

curl -fsS -o "$HEAVY_WORK/release.json" "$PROXY/$OWNER_REPO/releases/tags/v$VERSION" \
  || heavy_fail "the release document for v$VERSION was not served"
python3 - "$HEAVY_WORK/release.json" "$HEAVY_TAP_BASE/proxy/$REG/" <<'PY' || { head -c 600 "$HEAVY_WORK/release.json" >&2; heavy_fail "the release document still points at the forge"; }
import json, sys
doc = json.load(open(sys.argv[1])); base = sys.argv[2]
urls = [a.get("browser_download_url") for a in doc.get("assets", [])]
urls += [doc.get("tarball_url"), doc.get("zipball_url")]
urls = [u for u in urls if u]
bad = [u for u in urls if not u.startswith(base)]
if bad:
    print("not on the proxy:", bad[:3], file=sys.stderr)
# The document's own `url` is the release's identity, left as the forge
# wrote it; what must not survive are the links a client would *follow*.
api = [k for k in ("assets_url", "upload_url") if str(doc.get(k, "")).startswith("https://api.github.com")]
api += ["assets[].url" for a in doc.get("assets", []) if str(a.get("url", "")).startswith("https://api.github.com")]
if api:
    print("forge API links left:", api, file=sys.stderr)
sys.exit(0 if urls and not bad and not api else 1)
PY
heavy_log "MISE-API-READS-OK (tags served, commits refused, release URLs on the proxy)"

# ── 4. The air gap: seed, export, carry, import, install with no egress ──────
#
# RFC 0008 §10's standing proof, and the only test in the tree where "the
# server never dials" is a measurement rather than a claim.
#
# The shape is the RFC's §5.1 diagram, with the gap made real twice over:
#
#   * the second instance runs `[air_gap] enabled = true`, so every registry
#     client it builds is wrapped in one that refuses to dial (§13 decision 1)
#     — there is no route off the host *through the server*;
#   * `mise` itself runs with HTTP(S)_PROXY pointed at a closed port and
#     `no_proxy` narrowed to the loopback, so a URL the rewrite rules failed
#     to catch fails immediately instead of quietly succeeding through the
#     runner's real network — the failure mode that makes an air-gapped claim
#     untestable on a connected developer machine.
#
# What is *not* claimed: the runner's kernel routing is untouched, so this is
# egress denied to the two processes under test rather than to the host. That
# is the strongest form available in a CI container that has to reach GitHub
# in section 1 to have anything to carry across in section 4.

heavy_mark "air-gap"
AG_WORK="$HEAVY_WORK/airgap"
mkdir -p "$AG_WORK"

# The lock is the bill of materials (§4.2). It names the release asset by both
# addresses mise records — the browser download URL and the API's own — so the
# plan carries whichever the client asks for.
# `|| true`: the whole point is that it is often absent, and a failing
# command substitution under `set -e` would end the run rather than take the
# fallback below.
ASSET_URL="$(grep -oE "https://github\.com/$OWNER_REPO/releases/download/v$VERSION/[^ \"]+\.tar\.gz" \
  "$HEAVY_WORK/install.txt" 2>/dev/null | head -1 || true)"
if [[ -z "$ASSET_URL" ]]; then
  # mise 2026.8 downloads by asset id; the URL is not in its output, so it is
  # rebuilt from the release the proxy already served.
  ASSET_URL="https://github.com/$OWNER_REPO/releases/download/v$VERSION/gh_${VERSION}_linux_amd64.tar.gz"
fi
heavy_log "planning from a lock naming $ASSET_URL"
cat > "$AG_WORK/mise.lock" <<EOF
[[tools."$TOOL"]]
version = "$VERSION"
backend = "$TOOL"

[tools."$TOOL"."platforms.linux-x64"]
url = "$ASSET_URL"
EOF

# The binary, not `cargo run`: cargo writes progress and lock notices to
# stderr, and three of the steps below parse this command's output as JSON.
cargo build --quiet -p batlehub-cli >"$AG_WORK/cli-build.txt" 2>&1 \
  || { cat "$AG_WORK/cli-build.txt" >&2; heavy_fail "the CLI did not build"; }
CLI=("$(cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/batlehub-cli")
[[ -x "${CLI[0]}" ]] || heavy_fail "no batlehub-cli binary at ${CLI[0]}"
export BATLEHUB_SERVER="$HEAVY_BASE"
export BATLEHUB_TOKEN="$ADMIN_TOKEN"

"${CLI[@]}" mise plan --lock "$AG_WORK/mise.lock" --platform linux-x64 \
  -o "$AG_WORK/plan.json" >"$AG_WORK/plan.txt" 2>"$AG_WORK/plan.err" \
  || { cat "$AG_WORK/plan.txt" "$AG_WORK/plan.err" >&2; heavy_fail "mise plan failed"; }
grep -q "$REG" "$AG_WORK/plan.json" \
  || { cat "$AG_WORK/plan.json" >&2; heavy_fail "the plan named no registry for the asset host"; }
heavy_log "AIRGAP-PLAN-OK ($(grep -c '"tool"' "$AG_WORK/plan.json" || true) planned entries)"

# Seeding is fetching: one pass warms the connected instance and proves the
# planned path is one the server actually answers.
"${CLI[@]}" mise seed --plan "$AG_WORK/plan.json" >"$AG_WORK/seed.txt" 2>"$AG_WORK/seed.err" \
  || { cat "$AG_WORK/seed.txt" "$AG_WORK/seed.err" >&2; heavy_fail "mise seed failed — a planned path the server does not answer"; }
heavy_log "AIRGAP-SEED-OK ($(head -1 "$AG_WORK/seed.txt"))"

# A signing key, generated per run: the bundle's authenticity is the only
# thing standing between a disconnected instance and any tar somebody hands it.
head -c 32 /dev/urandom | od -An -tx1 | tr -d " \n" > "$AG_WORK/estate.key"
"${CLI[@]}" --json mise export --plan "$AG_WORK/plan.json" \
  --sign-key "$AG_WORK/estate.key" -o "$AG_WORK/estate.bhub" \
  --bundle-id "heavy-$HEAVY_RUN" >"$AG_WORK/export.json" 2>"$AG_WORK/export.err" \
  || { cat "$AG_WORK/export.json" "$AG_WORK/export.err" >&2; heavy_fail "mise export failed"; }
BLOBS="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["blobs"])' "$AG_WORK/export.json")"
[[ "$BLOBS" -ge 1 ]] || { cat "$AG_WORK/export.json" >&2; heavy_fail "the bundle carried no blob"; }
export HEAVY_BUNDLE_PUBKEY="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["signer_key"])' "$AG_WORK/export.json")"

# What the manifest carries, read out of the tar. This is the one place the
# `ref → commit` row can be checked honestly: a release *asset* resolves its
# tag before it is fetched (RFC 0019 §4.2 *Identity* — the tag is resolved,
# the cache keeps the tag as its key), and the two instances share a database,
# so the disconnected side would find the connected side's resolution whether
# the bundle carried one or not. The bundle is the thing under test, so the
# bundle is what is asserted.
python3 - "$AG_WORK/estate.bhub" <<'PY' || heavy_fail "the bundle's manifest does not carry the ref it resolved"
import json, re, sys, tarfile
with tarfile.open(sys.argv[1]) as t:
    manifest = json.load(t.extractfile("manifest.json"))
entries = manifest["entries"]
assert entries, "no entries"
refs = [e["git_ref"] for e in entries if e.get("git_ref")]
assert refs, f"no entry carries a git_ref: {entries}"
r = refs[0]
assert re.fullmatch(r"[0-9a-f]{40}", r["sha"]), r
assert r["kind"] == "tag", r
print(f"manifest: {len(entries)} entr(y|ies), ref {r['git_ref']} -> {r['sha'][:12]} ({r['kind']})")
PY

heavy_log "AIRGAP-EXPORT-OK ($BLOBS blob(s), signed by ${HEAVY_BUNDLE_PUBKEY:0:8}…, ref row carried)"

# ── the disconnected side ────────────────────────────────────────────────────

heavy_start_second_server tests/heavy/config.mise-airgap.toml "${HEAVY_AIRGAP_PORT:-8107}"
AG_BASE="$HEAVY_BASE2"

# It knows what it is. The miss log is empty because nothing has been asked
# for yet, which is a different fact from "nothing is missing" — and the
# endpoint says which by reporting the mode.
#
# What is *not* asserted here is `empty_registries`, and the reason is this
# fixture rather than the feature: the two instances share one `DATABASE_URL`,
# and every deployment wraps its storage in a `StorageRouter` whose inventory
# lives in that database (`server/src/setup.rs`). So the disconnected instance
# reads the connected one's rows and does not look empty, however empty its own
# directory is. A real air-gapped pair shares nothing; the report is asserted
# where the store is per-instance, in `crates/web/tests/air_gap.rs`.
MISSING_CODE="$(curl -s -o "$AG_WORK/missing.json" -w "$CURL_CODE" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$AG_BASE/api/v1/admin/air-gap/missing?registry=$REG")"
[[ "$MISSING_CODE" == "200" ]] \
  || heavy_fail "the miss endpoint answered $MISSING_CODE: $(cat "$AG_WORK/missing.json")"
grep -q '"air_gapped":true' "$AG_WORK/missing.json" \
  || heavy_fail "the instance does not think it is air-gapped: $(cat "$AG_WORK/missing.json")"

# Before the import: a coordinate it does not hold is a 503 that names itself,
# never a 404 — 404 asserts the artifact does not exist, and is what a hybrid
# fall-through acts on (§4.4).
BEFORE_CODE="$(curl -s -o "$AG_WORK/before.json" -w "$CURL_CODE" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$AG_BASE/proxy/$REG/$OWNER_REPO/releases/tags/v$VERSION")"
[[ "$BEFORE_CODE" == "503" ]] \
  || heavy_fail "an air-gapped miss answered $BEFORE_CODE, expected 503 (404 would assert non-existence)"
grep -q 'content_unavailable' "$AG_WORK/before.json" \
  || heavy_fail "the 503 body did not name itself: $(cat "$AG_WORK/before.json")"
heavy_log "AIRGAP-MISS-OK (503 content_unavailable before the import)"

BATLEHUB_SERVER="$AG_BASE" "${CLI[@]}" mise import "$AG_WORK/estate.bhub" \
  >"$AG_WORK/import.txt" 2>"$AG_WORK/import.err" \
  || { cat "$AG_WORK/import.txt" "$AG_WORK/import.err" >&2; heavy_fail "mise import failed"; }
grep -q "signature ok" "$AG_WORK/import.txt" \
  || { cat "$AG_WORK/import.txt" >&2; heavy_fail "the import did not verify the signature"; }
heavy_log "AIRGAP-IMPORT-OK ($(head -1 "$AG_WORK/import.txt"))"

# The history is the provenance: a disconnected instance has no upstream to
# point at, so what it can say about an artifact is which bundle brought it.
curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" "$AG_BASE/api/v1/admin/bundle" \
  | grep -q "heavy-$HEAVY_RUN" \
  || heavy_fail "the imported bundle is not in the history"

# The bytes, back out of the read path, from an instance that cannot dial.
AG_PATH="$(python3 - "$AG_WORK/plan.json" <<'PY'
import json, sys
plan = json.load(open(sys.argv[1]))
print(next(e["proxy_path"] for e in plan["entries"] if e.get("proxy_path")))
PY
)"
AG_CODE="$(curl -s -o "$AG_WORK/served.bin" -w "$CURL_CODE" \
  -H "Authorization: Bearer $ADMIN_TOKEN" "$AG_BASE$AG_PATH")"
[[ "$AG_CODE" == "200" ]] \
  || heavy_fail "the air-gapped instance did not serve what the bundle gave it: HTTP $AG_CODE on $AG_PATH"
[[ -s "$AG_WORK/served.bin" ]] || heavy_fail "the served artifact was empty"
heavy_log "AIRGAP-SERVE-OK ($(wc -c < "$AG_WORK/served.bin") bytes, from an instance with no route out)"

# ── the install, with egress denied ──────────────────────────────────────────
#
# A fresh mise home so nothing installed in section 1 can answer, and a proxy
# pointed at a closed port so anything the rewrite rules missed fails at once.

AG_PROXY="$AG_BASE/proxy/$REG"
export MISE_DATA_DIR="$AG_WORK/mise/data"
export MISE_CACHE_DIR="$AG_WORK/mise/cache"
export MISE_CONFIG_DIR="$AG_WORK/mise/config"
export MISE_STATE_DIR="$AG_WORK/mise/state"
mkdir -p "$MISE_DATA_DIR" "$MISE_CACHE_DIR" "$MISE_CONFIG_DIR" "$MISE_STATE_DIR"
# Four backslashes, as in section 1 and for the same reason: the heredoc eats
# one pair, and TOML needs `\\.` in a basic string to mean a literal `\.` in
# the regex. Two would reach mise as `\.`, which is a TOML escape error — and
# mise reports it, keeps going, and runs with the whole settings block
# dropped. (It is the trap `toml_quote` exists for in the CLI's generator.)
cat > "$MISE_CONFIG_DIR/config.toml" <<EOF
[settings]
# The five verifiers RFC 0008 §2 names, off — exactly as that section says
# operators already set them on a disconnected workstation, and the reason
# §5.2 moves verification to the connected side. Each reaches Sigstore or the
# forge before it will install anything, and a transparency log is not a thing
# a proxy caches: an offline inclusion proof proves nothing (§3, first
# non-goal). What replaces them is the verdict the bundle carried (§13.4).
#
# Measured rather than assumed, and in this order: with attestations on, the
# install gets through download and checksum from the air-gapped instance and
# stops at `api.github.com/…/attestations`. With those off, SLSA stops it at
# the release *document*, which is the §14.8 gap in a second guise. The
# checksum in `mise.lock` is verified locally throughout — it is the one check
# that needs nothing but the bytes.
github_attestations = false

[settings.github]
slsa = false

[settings.aqua]
cosign = false
slsa = false
minisign = false

[settings.url_replacements]
"regex:^https://api\\\\.github\\\\.com/repos/(.+)" = "$AG_PROXY/\$1"
"regex:^https://github\\\\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)" = "$AG_PROXY/\$1/\$2/releases/download/\$3/\$4"
"regex:^https://codeload\\\\.github\\\\.com/([^/]+)/([^/]+)/tar\\\\.gz/(?:refs/tags/)?(.+)" = "$AG_PROXY/\$1/\$2/tarball/\$3"
EOF

# **From the lock**, in a project directory, which is the whole premise of
# §4.2: `mise.lock` records the exact URL and checksum of every tool, so an
# install that reads it resolves nothing. Without a lock mise asks the forge
# for the release *list* to resolve `2.60.0` to a concrete version — a
# document, and a bundle carries artifacts — and gets the `503` this mode
# promises. That refusal is correct and it is also the measurement: it is why
# the lock, not the version string, is the bill of materials.
AG_PROJECT="$AG_WORK/project"
mkdir -p "$AG_PROJECT"
AG_SHA="$(sha256sum "$AG_WORK/served.bin" | cut -d" " -f1)"
cat > "$AG_PROJECT/mise.toml" <<EOF
[settings]
lockfile = true

[tools]
"$TOOL" = { version = "$VERSION", exe = "gh" }
EOF
cat > "$AG_PROJECT/mise.lock" <<EOF
[[tools."$TOOL"]]
version = "$VERSION"
backend = "$TOOL"

[tools."$TOOL"."platforms.linux-x64"]
url = "$ASSET_URL"
checksum = "sha256:$AG_SHA"
EOF

heavy_log "mise install from the lock, with egress denied (proxy -> 127.0.0.1:1)"
set +e
(
  cd "$AG_PROJECT"
  env HTTP_PROXY="$CLOSED_PROXY" HTTPS_PROXY="$CLOSED_PROXY" \
      http_proxy="$CLOSED_PROXY" https_proxy="$CLOSED_PROXY" \
      NO_PROXY="$LOOPBACK_DIRECT" no_proxy="127.0.0.1,localhost" \
      MISE_TRUSTED_CONFIG_PATHS="$AG_PROJECT" \
      "${MISE[@]}" install
) >"$AG_WORK/install.txt" 2>&1
AG_INSTALL=$?
set -e
if [[ "$AG_INSTALL" -ne 0 ]]; then
  cat "$AG_WORK/install.txt" >&2
  heavy_fail "mise install from the lock failed against the air-gapped instance with no egress — read the log above: a URL the plan did not carry is a gap in the bundle, and that is the finding"
fi
AG_INSTALLED="$(cd "$AG_PROJECT" && env HTTP_PROXY="$CLOSED_PROXY" \
  HTTPS_PROXY="$CLOSED_PROXY" NO_PROXY="$LOOPBACK_DIRECT" \
  MISE_TRUSTED_CONFIG_PATHS="$AG_PROJECT" \
  "${MISE[@]}" exec -- gh --version 2>/dev/null | head -1 || true)"
[[ "$AG_INSTALLED" == *"$VERSION"* ]] \
  || heavy_fail "gh --version answered '$AG_INSTALLED' after the air-gapped install, expected $VERSION"
heavy_log "AIRGAP-INSTALL-OK ($AG_INSTALLED, from the lock, with no route off the host)"

# The other half of the promise: what it could not answer is *named*, so the
# next bundle is a list the estate produced.
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$AG_BASE/api/v1/admin/air-gap/missing?registry=$REG" > "$AG_WORK/missing-after.json"
heavy_log "AIRGAP-MISSLOG-OK ($(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(d["total"], "recorded miss(es):", ", ".join(sorted({i["kind"]+" "+i["storage_key"] for i in d["items"]})[:4]))' "$AG_WORK/missing-after.json"))"

heavy_stop_second_server

heavy_done MISE-HEAVY-OK
