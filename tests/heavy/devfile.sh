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
# The local half (RFC 0039 §6.8), on a hybrid registry, publishing through the
# real `batlehub publish`:
#
#  L1  with no token the publish is refused; with role:user's a stack
#      directory publishes (201); the same directory again is a 409, and the
#      digest of the 201 is the one HEAD answers;
#  L2  `list` shows the published stack beside upstream's;
#  L3  `pull acme-go:1.0.0 --all` reads only this instance, the devfile on disk
#      hashes to the manifest's layer and archive.tar is unpacked;
#  L4  `download acme-go hello` returns the published zip byte-identical;
#  L5  publishing `go` (an upstream name) as role:user is refused naming
#      devfile:stack:claim, and upstream's go is still listed; on a registry
#      whose upstream never answers the same publish is 503, while a second
#      version of a name already owned there publishes;
#  L6  publishing `go` with the claim takes the name whole: one go, 99.0.0
#      only, `pull go:2.6.0` reports it absent with no OCI request, and the
#      tag replayed at the OCI route is MANIFEST_UNKNOWN;
#  L7  a non-public published stack is absent anonymously and its name stays
#      owned — upstream's go does not reappear for the anonymous caller;
#  L8  a blocked published version leaves the index, its manifest is 404 and a
#      kept layer digest is BLOB_UNKNOWN;
#  L9  deleting the last version releases the name: upstream's go is back, and
#      a new first publish needs the claim again.
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
  return $?
}

hits() {
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk '/^batlehub_artifact_cache_hits_total/ { total += $2 } END { print total + 0 }'
  return $?
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
  local verb="$1" url="$2"
  local method=(-X "$verb")
  [[ "$verb" == HEAD ]] && method=(--head)
  curl -s -o "$HEAVY_WORK/replay.json" -w '%{http_code}' --max-time 30 "${method[@]}" "$url"
  return $?
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


# ── RFC 0039: the local half ─────────────────────────────────────────────────

LREG="devfile-local-$HEAVY_RUN"
DARK="devfile-dark-$HEAVY_RUN"
LHOST="http://devfile-local.localhost:$HEAVY_TAP_PORT/"
USER_TOKEN="heavy-user-token"

heavy_log "Building batlehub-cli"
cargo build -p batlehub-cli >"$HEAVY_WORK/cli-build.log" 2>&1 \
  || { cat "$HEAVY_WORK/cli-build.log" >&2; heavy_fail "batlehub-cli did not build"; }
BH="$HEAVY_ROOT/target/debug/batlehub-cli"

# stack_dir <dir> <name> <version> — a stack laid out as registry-support lays
# one out: the devfile, a kubernetes/ manifest, one starter zip.
stack_dir() {
  local dir="$1" name="$2" version="$3"
  mkdir -p "$dir/kubernetes" "$dir/starter-projects"
  cat >"$dir/devfile.yaml" <<YAML
schemaVersion: 2.2.0
metadata:
  name: $name
  version: $version
  displayName: Acme $name
  icon: https://icons.acme.example/$name.svg
  tags:
    - Go
starterProjects:
  - name: hello
    zip:
      location: hello.zip
components:
  - name: runtime
    container:
      image: golang:1.23
YAML
  printf 'kind: Deployment\n' >"$dir/kubernetes/deploy.yaml"
  python3 -c 'import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z: z.writestr("main.go", "package main\n")' \
    "$dir/starter-projects/hello.zip"
}

# publish <registry> <token> <dir> — `batlehub publish`, its output in $OUT.
publish() {
  OUT="$HEAVY_WORK/publish-$RANDOM.out"
  HOME="$HEAVY_WORK/home" BATLEHUB_SERVER="$HEAVY_TAP_BASE" BATLEHUB_TOKEN="$2" \
    "$BH" publish "$3" --registry "$1" >"$OUT" 2>&1
  return $?
}

# ── L1. Publish, twice ───────────────────────────────────────────────────────

stack_dir "$HEAVY_WORK/acme-go" acme-go 1.0.0
# The credential boundary of the publish, through the real CLI: no token is a
# refusal that writes nothing, and the role:user token below is the allowed arm.
heavy_mark l1-anon
publish "$LREG" "" "$HEAVY_WORK/acme-go" && heavy_fail "L1: an anonymous publish succeeded"
heavy_wire_re_after l1-anon "^POST /proxy/$LREG/api/stacks -> 40[13]" "L1: the anonymous publish was not refused"
heavy_mark l1
publish "$LREG" "$USER_TOKEN" "$HEAVY_WORK/acme-go" \
  || { cat "$OUT" >&2; heavy_fail "L1: the first publish failed"; }
heavy_wire_re_after l1 "^POST /proxy/$LREG/api/stacks -> 201" "L1: the publish did not answer 201"
PUBLISHED_DIGEST="$(grep -oE 'sha256:[0-9a-f]{64}' "$OUT" | head -1)"
[[ -n "$PUBLISHED_DIGEST" ]] || { cat "$OUT" >&2; heavy_fail "L1: the publish printed no digest"; }
heavy_mark l1-again
publish "$LREG" "$USER_TOKEN" "$HEAVY_WORK/acme-go" && heavy_fail "L1: a republish succeeded"
heavy_wire_re_after l1-again "^POST /proxy/$LREG/api/stacks -> 409" "L1: the republish was not 409"
HEAD_DIGEST="$(curl -fsS --head "${LHOST}v2/devfile-catalog/acme-go/manifests/1.0.0" \
  | tr -d '\r' | awk 'tolower($1) == "docker-content-digest:" { print $2 }')"
[[ "$HEAD_DIGEST" == "$PUBLISHED_DIGEST" ]] \
  || heavy_fail "L1: HEAD answers $HEAD_DIGEST, the publish said $PUBLISHED_DIGEST"
heavy_log "L1: published as $PUBLISHED_DIGEST; the republish is 409"

# ── L2. Listed beside upstream ───────────────────────────────────────────────

# `list` takes no registry argument and neither `--context` nor
# `--new-index-schema`: it reads the registry from $REGISTRY_LIST
# (`cmd/root.go`) and the legacy `index/all`. So not through rl.
OUT="$HEAVY_WORK/l2.out"
HOME="$HEAVY_WORK/home" REGISTRY_LIST="${LHOST%/}" "$CLIENT" list --type all >"$OUT" 2>&1 || true
grep -q 'acme-go' "$OUT" || { cat "$OUT" >&2; heavy_fail "L2: list does not show acme-go"; }
grep -qw 'nodejs' "$OUT" || { cat "$OUT" >&2; heavy_fail "L2: list lost upstream's stacks"; }
heavy_log "L2: acme-go is listed beside upstream's stacks"

# ── L3. Pull it ──────────────────────────────────────────────────────────────

heavy_mark l3
rl "$HEAVY_WORK/l3" pull "$LHOST" acme-go:1.0.0 --all --new-index-schema
[[ -s "$HEAVY_WORK/l3/devfile.yaml" ]] || { cat "$OUT" >&2; heavy_fail "L3: the pull left no devfile.yaml"; }
grep -q "Failed" "$OUT" && { cat "$OUT" >&2; heavy_fail "L3: the client printed a failure"; }
cmp -s "$HEAVY_WORK/l3/devfile.yaml" "$HEAVY_WORK/acme-go/devfile.yaml" \
  || heavy_fail "L3: the pulled devfile is not the published one"
[[ -s "$HEAVY_WORK/l3/kubernetes/deploy.yaml" && ! -e "$HEAVY_WORK/l3/archive.tar" ]] \
  || heavy_fail "L3: archive.tar was not fetched and unpacked"
heavy_wire_re_after l3 '^HEAD /v2/devfile-catalog/acme-go/manifests/1[.]0[.]0 -> 200' \
  "L3: the manifest HEAD did not answer 200"
[[ "$(heavy_wire_count_after l3 '^GET /v2/devfile-catalog/acme-go/blobs/sha256:[0-9a-f]{64} -> 200')" -ge 2 ]] \
  || heavy_fail "L3: fewer than two layers were fetched"
L3_LAYER="sha256:$(sha256sum "$HEAVY_WORK/l3/devfile.yaml" | cut -d' ' -f1)"
curl -fsS "${LHOST}v2/devfile-catalog/acme-go/manifests/1.0.0" | grep -qF "$L3_LAYER" \
  || heavy_fail "L3: the devfile on disk is not the layer the manifest names"
heavy_log "L3: pulled; the devfile on disk is $L3_LAYER"

# ── L4. The starter project ──────────────────────────────────────────────────

heavy_mark l4
OUT="$HEAVY_WORK/l4.zip"
curl -fsS -o "$OUT" "${LHOST}devfiles/acme-go/starter-projects/hello" \
  || heavy_fail "L4: the starter project did not answer"
cmp -s "$OUT" "$HEAVY_WORK/acme-go/starter-projects/hello.zip" \
  || heavy_fail "L4: the starter zip is not the published one"
rl "$HEAVY_WORK/l4" download "$LHOST" acme-go hello --new-index-schema
[[ -s "$HEAVY_WORK/l4/main.go" ]] || { cat "$OUT" >&2; heavy_fail "L4: registry-library unzipped nothing"; }
heavy_log "L4: the starter zip is byte-identical and registry-library unzips it"

# ── L5. An upstream name without the claim ───────────────────────────────────

stack_dir "$HEAVY_WORK/go-99" go 99.0.0
heavy_mark l5
publish "$LREG" "$USER_TOKEN" "$HEAVY_WORK/go-99" && heavy_fail "L5: role:user took upstream's go"
heavy_wire_re_after l5 "^POST /proxy/$LREG/api/stacks -> 403" "L5: the publish was not 403"
heavy_client_must_say "$OUT" "devfile:stack:claim" "L5: the refusal does not name the action"
curl -fsS "${LHOST}v2index" | python3 -c '
import json, sys
go = [e for e in json.load(sys.stdin) if e["name"] == "go"]
assert go and "2.6.0" in [v["version"] for v in go[0]["versions"]], go
' || heavy_fail "L5: upstream's go is no longer listed"

heavy_mark l5-dark
publish "$DARK" "$USER_TOKEN" "$HEAVY_WORK/acme-go" && heavy_fail "L5: an undecidable claim let the publish through"
heavy_wire_re_after l5-dark "^POST /proxy/$DARK/api/stacks -> 503" "L5: the undecidable claim was not 503"
publish "$DARK" "$ADMIN_TOKEN" "$HEAVY_WORK/acme-go" \
  || { cat "$OUT" >&2; heavy_fail "L5: the claim holder could not publish with upstream down"; }
stack_dir "$HEAVY_WORK/acme-go-11" acme-go 1.1.0
publish "$DARK" "$ADMIN_TOKEN" "$HEAVY_WORK/acme-go-11" \
  || { cat "$OUT" >&2; heavy_fail "L5: a second version of an owned name needed upstream"; }
heavy_log "L5: 403 naming the claim; 503 with upstream down; an owned name publishes regardless"

# ── L6. The claim takes the name whole ───────────────────────────────────────

heavy_mark l6
publish "$LREG" "$ADMIN_TOKEN" "$HEAVY_WORK/go-99" \
  || { cat "$OUT" >&2; heavy_fail "L6: the claim holder could not publish go"; }
curl -fsS "${LHOST}v2index" | python3 -c '
import json, sys
go = [e for e in json.load(sys.stdin) if e["name"] == "go"]
assert len(go) == 1 and [v["version"] for v in go[0]["versions"]] == ["99.0.0"], go
' || heavy_fail "L6: the index does not show one go at 99.0.0 only"
heavy_mark l6-pull
rl "$HEAVY_WORK/l6" pull "$LHOST" go:2.6.0 --new-index-schema
heavy_client_must_say "$OUT" "does not exist" "L6: pulling upstream's go:2.6.0 was not reported absent"
[[ "$(heavy_wire_count_after l6-pull '^(GET|HEAD) /v2/devfile-catalog/go/')" -eq 0 ]] \
  || heavy_fail "L6: the client reached the OCI routes for a version this instance does not hold"
# Replayed straight at the OCI route, upstream's tag is unknown here: an owned
# name resolves locally only. That the server never asked upstream is the
# in-process `taking_a_name_upstream_lists_needs_the_claim_and_then_owns_it_whole`
# (a mock upstream expecting zero manifest requests) — this tap sees only the
# client's side.
curl -s -o "$HEAVY_WORK/l6.json" "${LHOST}v2/devfile-catalog/go/manifests/2.6.0"
grep -q MANIFEST_UNKNOWN "$HEAVY_WORK/l6.json" \
  || heavy_fail "L6: upstream's go:2.6.0 manifest was served under an owned name"
heavy_log "L6: go is 99.0.0 only; upstream's 2.6.0 is absent, at the index and at the OCI route"

# ── L7. A hidden published stack stays owned ─────────────────────────────────

curl -fsS -X PUT "$HEAVY_BASE/api/v1/admin/registries/$LREG/packages/go/visibility" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
  -d '{"visibility":"internal"}' >/dev/null \
  || heavy_fail "L7: setting go internal failed"
curl -fsS "${LHOST}v2index" | python3 -c '
import json, sys
assert "go" not in [e["name"] for e in json.load(sys.stdin)]
' || heavy_fail "L7: an anonymous caller sees a go — the hidden local one, or upstream's in its place"
[[ "$(curl -s -o /dev/null -w '%{http_code}' "${LHOST}v2/devfile-catalog/go/manifests/99.0.0")" == 404 ]] \
  || heavy_fail "L7: the hidden version's manifest was not 404 anonymously"
curl -fsS -X PUT "$HEAVY_BASE/api/v1/admin/registries/$LREG/packages/go/visibility" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
  -d '{"visibility":"public"}' >/dev/null
heavy_log "L7: hidden from anonymous, and upstream's go did not take its place"

# ── L8. A block ──────────────────────────────────────────────────────────────

KEPT_LAYER="$L3_LAYER"
heavy_block "$LREG" acme-go 1.0.0
sleep 31
curl -fsS "${LHOST}v2index" | python3 -c '
import json, sys
assert "acme-go" not in [e["name"] for e in json.load(sys.stdin)]
' || heavy_fail "L8: the blocked version is still listed"
[[ "$(curl -s -o /dev/null -w '%{http_code}' "${LHOST}v2/devfile-catalog/acme-go/manifests/1.0.0")" == 404 ]] \
  || heavy_fail "L8: the blocked manifest was not 404"
curl -s -o "$HEAVY_WORK/l8.json" "${LHOST}v2/devfile-catalog/acme-go/blobs/$KEPT_LAYER"
grep -qE 'BLOB_UNKNOWN|NAME_UNKNOWN' "$HEAVY_WORK/l8.json" \
  || heavy_fail "L8: a kept layer digest of the blocked version was served"
heavy_unblock "$LREG" acme-go 1.0.0
heavy_log "L8: blocked — unlisted, manifest 404, kept digest refused"

# ── L9. Delete releases the name ─────────────────────────────────────────────

[[ "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$HEAVY_TAP_BASE/proxy/$LREG/api/stacks/go/99.0.0")" == 200 ]] \
  || heavy_fail "L9: the delete did not answer 200"
curl -fsS "${LHOST}v2index" | python3 -c '
import json, sys
go = [e for e in json.load(sys.stdin) if e["name"] == "go"]
assert go and "2.6.0" in [v["version"] for v in go[0]["versions"]], go
' || heavy_fail "L9: upstream's go did not come back"
stack_dir "$HEAVY_WORK/go-991" go 99.1.0
publish "$LREG" "$USER_TOKEN" "$HEAVY_WORK/go-991" && heavy_fail "L9: a released name was taken without the claim"
heavy_client_must_say "$OUT" "devfile:stack:claim" "L9: the second first publish did not need the claim"
heavy_log "L9: the name is released and the claim applies again"

heavy_done "devfile heavy test passed: pull, cache, listing refusal, OCI replay refused, documents agree, starter, prefix trap, Che reads; published: publish, list, pull, starter, claim, ownership, visibility, block, release"
