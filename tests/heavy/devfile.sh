#!/usr/bin/env bash
# Heavy devfile-registry test — the real `registry-library` CLI (the library
# `odo` and the IDE plugins embed) against a `devfile` registry in front of
# registry.devfile.io, plus Che's three reads transcribed (RFC 0035 §6.9).
#
# The client's exit code is never read: `registry-library` prints every failure
# with `fmt.Printf` and exits 0 (RFC 0035 §2.3). Every case asserts on the wire
# transcript and on the files the client left behind.
#
#   1  a pull of go:2.6.0 --all makes the five requests of §5.1, all here,
#      and leaves a devfile.yaml with the manifest's digest plus the unpacked
#      archive.tar;
#   2  a blocked version is refused at the listing: no request under /v2/;
#   3  the blocked tag and a digest recorded before the block are refused when
#      replayed straight at the OCI routes;
#   4  the served indexes and tag list no longer name the blocked version, and
#      a blocked default moves (v2) or takes the stack away (legacy);
#   5  a second pull of the same stack is served from storage;
#   6  (an altered upstream byte is refused before a byte of it is served —
#      covered in-process by `crates/web/tests/devfile.rs::
#      an_altered_layer_is_never_served`: this harness has no upstream-side
#      tap to alter one);
#   7  a starter project is downloaded at the host root;
#   8  the prefix trap: through /proxy/{reg}/ the index answers and the pull
#      then 404s at the main host's /v2/ — what the config warning is about;
#   9  Che's reads, transcribed with curl: no credential, no redirect, the
#      prefix kept.
#
# Run via `task test:devfile-heavy` or directly. Needs network (the server
# reaches registry.devfile.io; the client is built from GitHub's source
# tarball once and cached) and Go. Environment knobs: DATABASE_URL (required),
# HEAVY_PORT (8176), HEAVY_TAP_PORT (8186), COVERAGE,
# HEAVY_DEVFILE_CLIENT_COMMIT (the registry-support commit to build).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init devfile 8176 8186
heavy_need python3 "python3 (the wire tap)"
heavy_need sha256sum "coreutils"

REG="devfile-$HEAVY_RUN"

heavy_devfile_client
CLIENT="$HEAVY_DEVFILE_CLIENT"

heavy_start_server tests/heavy/config.devfile.toml
heavy_start_tap

HOST_ROOT="http://devfile.localhost:$HEAVY_TAP_PORT/"
PREFIX_ROOT="$HEAVY_TAP_BASE/proxy/$REG/"
OUT=""

# rl <context-dir> <args…> — run the client with its home and output in the
# run's directory. Always "succeeds"; the output is kept in $OUT.
rl() {
  local ctx="$1"
  shift
  mkdir -p "$ctx"
  OUT="$ctx.out"
  HOME="$HEAVY_WORK/home" "$CLIENT" "$@" --context "$ctx" >"$OUT" 2>&1 || true
}

hits() {
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk '/^batlehub_artifact_cache_hits_total/ { total += $2 } END { print total + 0 }'
}

# ── 1. A pull, end to end ────────────────────────────────────────────────────

heavy_mark pull
rl "$HEAVY_WORK/pull" pull "$HOST_ROOT" go:2.6.0 --all --new-index-schema
[[ -s "$HEAVY_WORK/pull/devfile.yaml" ]] \
  || { cat "$OUT" >&2; heavy_fail "1: the pull left no devfile.yaml"; }
grep -q "Failed" "$OUT" && { cat "$OUT" >&2; heavy_fail "1: the client printed a failure"; }
[[ -d "$HEAVY_WORK/pull/docker" && ! -e "$HEAVY_WORK/pull/archive.tar" ]] \
  || heavy_fail "1: archive.tar was not fetched and unpacked"
heavy_wire_re_after pull '^GET /v2index -> 200' "1: the index was not read at the host root"
heavy_wire_re_after pull '^HEAD /v2/devfile-catalog/go/manifests/2[.]6[.]0 -> 200' \
  "1: the manifest HEAD by tag did not answer 200"
heavy_wire_re_after pull '^GET /v2/devfile-catalog/go/manifests/sha256:[0-9a-f]{64} -> 200' \
  "1: the manifest by digest did not answer 200"
[[ "$(heavy_wire_count_after pull '^GET /v2/devfile-catalog/go/blobs/sha256:[0-9a-f]{64} -> 200')" -ge 2 ]] \
  || heavy_fail "1: fewer than two layers were fetched"
DEVFILE_DIGEST="sha256:$(sha256sum "$HEAVY_WORK/pull/devfile.yaml" | cut -d' ' -f1)"
curl -fsS "$HEAVY_TAP_BASE/proxy/$REG/v2/devfile-catalog/go/manifests/2.6.0" \
  | grep -qF "$DEVFILE_DIGEST" \
  || heavy_fail "1: the devfile.yaml on disk is not the layer the manifest names"
heavy_log "1: pull ok — the devfile on disk is $DEVFILE_DIGEST"

# ── 5. The second pull comes from storage ────────────────────────────────────

BEFORE="$(hits)"
rl "$HEAVY_WORK/pull2" pull "$HOST_ROOT" go:2.6.0 --all --new-index-schema
[[ -s "$HEAVY_WORK/pull2/devfile.yaml" ]] || { cat "$OUT" >&2; heavy_fail "5: the second pull failed"; }
AFTER="$(hits)"
[[ "$AFTER" -gt "$BEFORE" ]] \
  || heavy_fail "5: batlehub_artifact_cache_hits_total did not move ($BEFORE → $AFTER)"
heavy_log "5: cache hits $BEFORE → $AFTER"

# ── 2 & 3. A block, at the listing and at the OCI routes ─────────────────────

# The digest of nodejs@2.1.1's devfile, learned before the block, so the
# replay in case 3 names a digest a client could genuinely have kept.
OLD_BLOB="$(curl -fsS "$HEAVY_TAP_BASE/proxy/$REG/v2/devfile-catalog/nodejs/manifests/2.1.1" \
  | python3 -c 'import json,sys; m=json.load(sys.stdin); print([l["digest"] for l in m["layers"] if l.get("annotations",{}).get("org.opencontainers.image.title")=="devfile.yaml"][0])')"

heavy_block "$REG" nodejs 2.1.1
# The index is filtered against a 30-second blocked-set snapshot (RFC 0035
# §6.1), and case 1 has already taken one.
sleep 31

heavy_mark blocked
rl "$HEAVY_WORK/blocked" pull "$HOST_ROOT" nodejs:2.1.1 --new-index-schema
heavy_client_must_say "$OUT" "the requested version 2.1.1 for stack nodejs does not exist" \
  "2: the client did not report the blocked version as absent"
heavy_wire_after blocked "GET /v2index -> 200"
[[ "$(heavy_wire_count_after blocked '^(GET|HEAD) /v2/')" -eq 0 ]] \
  || heavy_fail "2: the client reached the OCI routes for a version the index no longer lists"
[[ ! -e "$HEAVY_WORK/blocked/devfile.yaml" ]] || heavy_fail "2: a devfile was written"
heavy_log "2: refused at the listing"

heavy_mark replay
# `--head` for a HEAD, never `-X HEAD`: with `-X` curl still expects the body
# the Content-Length announces and waits for it until the server gives up.
code_of() {
  local method=(-X "$1")
  [[ "$1" == HEAD ]] && method=(--head)
  curl -s -o "$HEAVY_WORK/replay.json" -w '%{http_code}' --max-time 30 "${method[@]}" "$2"
}
[[ "$(code_of HEAD "$HOST_ROOT"v2/devfile-catalog/nodejs/manifests/2.1.1)" == 404 ]] \
  || heavy_fail "3: the blocked tag's manifest HEAD was not 404"
[[ "$(code_of GET "$HOST_ROOT"v2/devfile-catalog/nodejs/manifests/2.1.1)" == 404 ]] \
  && grep -q MANIFEST_UNKNOWN "$HEAVY_WORK/replay.json" \
  || heavy_fail "3: the blocked tag was not MANIFEST_UNKNOWN"
[[ "$(code_of GET "$HOST_ROOT"v2/devfile-catalog/nodejs/blobs/$OLD_BLOB)" == 404 ]] \
  && grep -q BLOB_UNKNOWN "$HEAVY_WORK/replay.json" \
  || heavy_fail "3: a blob digest of the blocked version was served"
heavy_log "3: the blocked tag and its kept digest are unreachable"

# ── 4. The documents say the same thing ─────────────────────────────────────

curl -fsS "$HOST_ROOT"v2index | python3 -c '
import json, sys
node = [e for e in json.load(sys.stdin) if e["name"] == "nodejs"][0]
vs = [v["version"] for v in node["versions"]]
assert "2.1.1" not in vs, vs
' || heavy_fail "4: the v2 index still lists nodejs@2.1.1"
curl -fsS "$HOST_ROOT"v2/devfile-catalog/nodejs/tags/list | grep -q '"2.1.1"' \
  && heavy_fail "4: the tag list still names 2.1.1"
DEFAULT="$(curl -fsS "$HOST_ROOT"v2index | python3 -c '
import json, sys
node = [e for e in json.load(sys.stdin) if e["name"] == "nodejs"][0]
print([v["version"] for v in node["versions"] if v.get("default")][0])')"
heavy_block "$REG" nodejs "$DEFAULT"
sleep 31
curl -fsS "$HOST_ROOT"index/all | python3 -c '
import json, sys
assert "nodejs" not in [e["name"] for e in json.load(sys.stdin)]
' || heavy_fail "4: blocking the default $DEFAULT left nodejs in the legacy index"
heavy_mark moved
rl "$HEAVY_WORK/moved" pull "$HOST_ROOT" nodejs --new-index-schema
[[ -s "$HEAVY_WORK/moved/devfile.yaml" ]] \
  || { cat "$OUT" >&2; heavy_fail "4: with the default blocked, an unpinned pull found no default"; }
grep -q "version: $DEFAULT" "$HEAVY_WORK/moved/devfile.yaml" \
  && heavy_fail "4: the unpinned pull fetched the blocked default $DEFAULT"
heavy_log "4: default $DEFAULT blocked — gone from index/all, and the v2 default moved"
heavy_unblock "$REG" nodejs "$DEFAULT"

# ── 7. A starter project, at the host root ───────────────────────────────────

heavy_mark starter
rl "$HEAVY_WORK/starter" download "$HOST_ROOT" go go-starter --new-index-schema
heavy_wire_re_after starter '^GET /devfiles/go/starter-projects/go-starter -> 200' \
  "7: the starter project was not fetched at the host root"
[[ -n "$(ls -A "$HEAVY_WORK/starter")" ]] || { cat "$OUT" >&2; heavy_fail "7: nothing was unzipped"; }
heavy_log "7: starter project downloaded"

# ── 8. The prefix trap ───────────────────────────────────────────────────────

heavy_mark prefix
rl "$HEAVY_WORK/prefix" pull "$PREFIX_ROOT" go --new-index-schema
heavy_wire_after prefix "GET /proxy/$REG/v2index -> 200"
heavy_wire_re_after prefix '^HEAD /v2/devfile-catalog/go/manifests/[^ ]* -> 404' \
  "8: the client did not fall off the prefix to the main host's /v2/"
[[ ! -e "$HEAVY_WORK/prefix/devfile.yaml" ]] || heavy_fail "8: a pull through the prefix succeeded"
heavy_log "8: through the prefix, the index answers and the pull 404s at the host root — as warned"

# ── 9. Che's reads, transcribed ──────────────────────────────────────────────
#
# Che's dashboard resolver sends no credential and follows no redirect
# (`dataResolver.ts`, maxRedirects: 0); its tile link is `resolveLinks`'
# `new URL('devfiles', root)` + `/{stack}/{version}`, which keeps the prefix.
heavy_mark che
CHE_LINK="$(node -e '
const root = process.argv[1];
const self = "devfile-catalog/go:2.6.0";
console.log(self.replace(":", "/").replace("devfile-catalog", new URL("devfiles", root).toString()));
' "$PREFIX_ROOT")"
[[ "$(curl -s -o "$HEAVY_WORK/che-index.json" -w '%{http_code}' --max-redirs 0 "${PREFIX_ROOT}index/all")" == 200 ]] \
  || heavy_fail "9: Che's index read did not answer 200 without redirects"
python3 -c 'import json,sys; assert any(e["name"]=="go" for e in json.load(open(sys.argv[1])))' \
  "$HEAVY_WORK/che-index.json" || heavy_fail "9: the index Che reads has no go stack"
[[ "$(curl -s -o "$HEAVY_WORK/che-devfile.yaml" -w '%{http_code}' --max-redirs 0 "$CHE_LINK")" == 200 ]] \
  || heavy_fail "9: Che's tile link $CHE_LINK did not answer 200"
cmp -s "$HEAVY_WORK/che-devfile.yaml" "$HEAVY_WORK/pull/devfile.yaml" \
  || heavy_fail "9: the devfile Che reads differs from the one the pull verified"
heavy_log "9: Che's index and tile link answer through the prefix, credential-free, with no redirect"

heavy_done "devfile heavy test passed: pull, cache, listing refusal, OCI replay refused, documents agree, starter, prefix trap, Che reads"
