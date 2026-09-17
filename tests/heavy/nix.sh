#!/usr/bin/env bash
# Heavy Nix binary-cache test — the real `nix` against a `nix` registry
# (RFC 0028 §6.10, items 1–3; items 4–5 arrive with the publish phase).
#
# Nothing here is written from Nix's documentation. What this suite proves, and
# what nothing below a real client can:
#
#   Substitute  `nix copy --from <proxy>` fetches `{hash}.narinfo` and then the
#               NAR at the *rewritten* URL, and `nix path-info --sigs` shows the
#               upstream's `Sig:` unchanged — which is the claim the whole
#               design rests on and which no test written from our own
#               implementation can make.
#   Refuse      With the coordinate blocked, the narinfo is a `404`, **nothing
#               is requested under `nar/{hash}/`**, and Nix says so in its own
#               words. The absent request is the assertion: a block that only
#               changed the status of the NAR would leave a client with a
#               cached narinfo fetching bytes.
#   Cache       A second copy into a *fresh* store is served from storage.
#   Publish     `nix copy --to` a local registry, then copy it back with a
#               client that trusts **only** this registry's key and
#               `require-sigs = true`. The negative control matters as much:
#               the same client with the key *absent* must refuse it, or the
#               positive arm would pass against a client that accepts anything.
#   Refuse a    A narinfo whose `NarHash` disagrees with the uploaded bytes is
#   bad claim   a `400` naming the field, and the previously published document
#               is unchanged — this server must never sign a hash it did not
#               compute.
#
# ## Why the client runs in a container, and why it still needs a chroot store
#
# Two separate problems, and each needs its own answer.
#
# **The binary cannot run without a real `/nix/store`.** RFC 0028 §6.10 proposed
# unpacking "the static single-user `nix` binary from the release tarball" into
# the run's temp directory. There is no such binary:
# `nix-{ver}-x86_64-linux.tar.xz` is a *store closure*, and its `bin/nix` has
# ELF interpreter `/nix/store/…-glibc-2.40-66/lib/ld-linux-x86-64.so.2` plus 158
# further `/nix/store` references. `releases.nixos.org` publishes no
# `nix-static*` asset and `NixOS/nix` cuts no GitHub release to take one from.
# So the client comes from `nixos/nix`, its own image — the same answer
# `closed_world.sh` already gives for `dnf` and `pacman`, through the shared
# `heavy_container_engine`. That is better than installing Nix on the runner:
# it needs no root on the host, and it runs on any developer machine with
# podman or docker rather than only on CI.
#
# **The store directory is still not a knob.** A `nix-cache-info` says
# `StoreDir: /nix/store`, and a client whose own store dir differs refuses the
# cache outright — *"binary cache '…' is for Nix stores with prefix '…', not
# '…'"*, which §4.4 and §5.1 both quote as the reason. So `NIX_STORE_DIR` is
# left alone and the isolation comes from a **chroot store**
# (`--store 'local?root=…'`), which keeps the logical store dir `/nix/store`
# while the bytes land under the run's own directory. That also makes the store
# survive between container invocations, which the Recover axis needs: it has to
# be the *same* client that saw the refusal.
#
# Run via `task test:nix-heavy` or directly. Needs network: cache.nixos.org and
# the image registry. Environment knobs: DATABASE_URL (required), HEAVY_PORT
# (8161), HEAVY_TAP_PORT (8171), COVERAGE, NIX_IMAGE, HEAVY_NIX_PATH (the store
# path to substitute; see below).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init nix 8161 8171
heavy_need python3 "python3 (the wire tap)"
heavy_need curl "curl"
heavy_container_engine

# Pinned, for the reason every other client in this tree is: a suite that
# floats on `latest` reports a different thing each week and cannot be bisected.
NIX_IMAGE="${NIX_IMAGE:-docker.io/nixos/nix:2.35.2}"
heavy_log "Pulling $NIX_IMAGE (the client)"
"${CW_ENGINE[@]}" pull "$NIX_IMAGE" >"$HEAVY_WORK/pull.log" 2>&1 \
  || { cat "$HEAVY_WORK/pull.log" >&2; heavy_fail "could not pull $NIX_IMAGE"; }

mkdir -p "$HEAVY_WORK/home"

# NIX_RUN_USER — the identity the client runs as inside the image.
#
# Not a constant, because the two engines map identities in opposite
# directions. **Rootless podman maps *container root* to the invoking host
# user**, so `-u $(id -u)` there names a UID that maps to a *subordinate* one on
# the host — which cannot so much as stat the bind mount. Nothing in the failure
# says so: nix reports its own "couldn't stat $HOME ('/work/home')" and then
# "creating directory '/.cache/nix': Permission denied", naming neither the
# mount nor the mapping. **Rootful docker is the other way round**: container
# root *is* host root, and every file the run leaves under $HEAVY_WORK is then
# root-owned in a mktemp directory this script's cleanup cannot remove.
#
# So probe instead of inferring from the engine's name — rootless docker and
# rootful podman both exist, and `podman info` would have to be parsed for each.
# Whichever identity can actually write the mount is the one the client runs as.
nix_probe_write() {  # <run flags…> → 0 when /work is writable under them
  "${CW_ENGINE[@]}" run --rm "$@" -v "$HEAVY_WORK:/work:z" "$NIX_IMAGE" \
    sh -c 'touch /work/.probe && rm -f /work/.probe' >/dev/null 2>&1
}
NIX_RUN_USER=()
if nix_probe_write -u "$(id -u):$(id -g)"; then
  NIX_RUN_USER=(-u "$(id -u):$(id -g)")
  heavy_log "client identity: $(id -u):$(id -g) — the run directory is writable as this user"
elif nix_probe_write; then
  heavy_log "client identity: container root — a rootless engine, so it is $(id -un) on the host"
else
  heavy_fail "neither $(id -u):$(id -g) nor container root can write $HEAVY_WORK through ${CW_ENGINE[0]} — the client has nowhere to keep a store"
fi

# nix_run <args…> — the client, in its own image, against this run's directory.
#
# `--network host` so `127.0.0.1:$HEAVY_TAP_PORT` is the tap, exactly as it is
# for a host process. `$NIX_RUN_USER` so the chroot stores it writes under
# $HEAVY_WORK are removable by the cleanup that owns them — a root-owned
# leftover in a mktemp directory is a mess the next run inherits. `HOME` inside
# the mount for the same reason.
nix_run() {
  "${CW_ENGINE[@]}" run --rm --network host \
    "${NIX_RUN_USER[@]}" \
    -e HOME=/work/home \
    -v "$HEAVY_WORK:/work:z" \
    "$NIX_IMAGE" \
    nix --extra-experimental-features "nix-command flakes" \
        --option narinfo-cache-positive-ttl 0 \
        --option narinfo-cache-negative-ttl 0 \
        --option substituters "" \
        "$@"
}

# A Nix store directory is mode `r-xr-xr-x` and its files are `r--r--r--`, so
# `rm -rf` on the work directory cannot unlink anything inside one — the shared
# cleanup fails with a "Permission denied" per file (some 2 000 lines in CI,
# which is what the real failure above them is read under) and leaves the whole
# store on disk. `heavy_init` has already installed `heavy_cleanup` on EXIT;
# this replaces it with one that makes the stores writable first.
heavy_cleanup_nix() {
  [[ -n "${HEAVY_WORK:-}" ]] && chmod -R u+w "$HEAVY_WORK" 2>/dev/null
  heavy_cleanup
}
trap heavy_cleanup_nix EXIT

REG="nix-$HEAVY_RUN"

# The store path to substitute. A *doc* output on purpose: it is small and its
# closure is other doc outputs, so the suite moves kilobytes rather than a
# toolchain. Overridable, because a path is only in a cache for as long as the
# channel that built it is alive — and a stale default has to fail with a
# sentence, not as an unexplained client error, which is what the pre-flight
# below is for.
STORE_PATH="${HEAVY_NIX_PATH:-/nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hslua-aeson-2.3.2-doc}"
STORE_HASH="$(basename "$STORE_PATH" | cut -c1-32)"
STORE_NAME="$(basename "$STORE_PATH" | cut -c34-)"
# The coordinate Nix itself parses out of that name: everything up to the first
# dash not followed by a letter is the package, the rest is the version.
PKG="$(python3 -c '
import sys
name = sys.argv[1]
for i, c in enumerate(name):
    if c == "-" and i + 1 < len(name) and not name[i + 1].isalpha():
        print(name[:i]); print(name[i + 1:]); break
else:
    print(name); print("-")
' "$STORE_NAME")"
PKG_NAME="$(printf '%s\n' "$PKG" | sed -n 1p)"
PKG_VERSION="$(printf '%s\n' "$PKG" | sed -n 2p)"

heavy_log "store path: $STORE_PATH"
heavy_log "coordinate: $PKG_NAME @ $PKG_VERSION (artifact $STORE_HASH)"

# Pre-flight: the default path has to still be in the upstream cache, or every
# assertion below fails for a reason that has nothing to do with this server.
curl -fsS -o /dev/null "https://cache.nixos.org/$STORE_HASH.narinfo" \
  || heavy_fail "cache.nixos.org no longer has $STORE_HASH.narinfo — set HEAVY_NIX_PATH to a store path it does have"

heavy_start_server tests/heavy/config.nix.toml
heavy_start_tap

SUBSTITUTER="$HEAVY_TAP_BASE/proxy/$REG/nix"

# nix_copy <store-root> — copy $STORE_PATH and its closure out of the proxy into
# a chroot store of its own. Each call gets a fresh root, so a phase starts from
# a client that has never seen the path — except where the Cache axis reuses
# nothing on purpose.
#
# `--from <url>` rather than a `substituters` line in `nix.conf`: an
# unprivileged user's `substituters` is ignored unless the daemon also lists the
# URL in `trusted-substituters`, and `--from` names the source store directly
# with no daemon in the way. It is the same HTTP binary-cache store either way,
# which is what makes the wire transcript below the same evidence.
nix_copy() {
  local root="$1"
  mkdir -p "$HEAVY_WORK/$root"
  # No `--no-check-sigs`: signature checking is on by default and it is half
  # of what this suite is proving. Passing the flag at all — in either
  # spelling — would turn the Substitute phase into a test of nothing.
  nix_run copy \
    --from "$SUBSTITUTER" \
    --to "local?root=/work/$root" \
    "$STORE_PATH" >"$RUN_OUT" 2>&1
  return $?
}

# ── Substitute ───────────────────────────────────────────────────────────────

heavy_log "Substitute — the closure comes through the proxy"
heavy_mark substitute
RUN_OUT="$HEAVY_WORK/substitute.txt"
nix_copy store-a \
  || { cat "$RUN_OUT" >&2; heavy_fail "nix copy --from the proxy failed"; }

# The narinfo, then the NAR at the URL *this instance* rewrote it to. The second
# assertion is the one that matters: `nar/{hash}/…` is a path no upstream ever
# serves, so seeing it proves the client read our rewritten `URL:` and not a
# cached one.
heavy_wire_after substitute "GET /proxy/$REG/nix/$STORE_HASH.narinfo -> 200" \
  "the client did not ask for the narinfo, so it did not resolve through this instance"
heavy_wire_re_after substitute "GET /proxy/$REG/nix/nar/$STORE_HASH/[^ ]+ -> 200" \
  "the NAR was not fetched at the rewritten URL — the client used upstream's own URL, which means the rewrite did not reach it"

# **The provenance claim.** The signature Nix verified is the upstream's, over a
# document one line of which this instance changed.
heavy_mark sigs
nix_run path-info --store "local?root=/work/store-a" --sigs "$STORE_PATH" \
  >"$HEAVY_WORK/sigs.txt" 2>&1 \
  || { cat "$HEAVY_WORK/sigs.txt" >&2; heavy_fail "nix path-info --sigs failed"; }
heavy_client_must_say "$HEAVY_WORK/sigs.txt" "cache\.nixos\.org-1:" \
  "the copied path does not carry cache.nixos.org's own signature — the relay did not preserve it, or this server re-signed something it had no business re-signing"
heavy_log "signature relayed intact:"
cat "$HEAVY_WORK/sigs.txt" >&2

# ── Cache ────────────────────────────────────────────────────────────────────
#
# Before the block, because a blocked coordinate serves nothing to be a hit on.

heavy_log "Cache — a second cold store is served from storage"
BEFORE="$(curl -fsS "$HEAVY_BASE/metrics" \
  | awk '/^batlehub_artifact_cache_hits_total/ { total += $2 } END { print total + 0 }')"
heavy_mark cache
RUN_OUT="$HEAVY_WORK/cache.txt"
nix_copy store-b \
  || { cat "$RUN_OUT" >&2; heavy_fail "the second copy into a fresh store failed"; }
AFTER="$(curl -fsS "$HEAVY_BASE/metrics" \
  | awk '/^batlehub_artifact_cache_hits_total/ { total += $2 } END { print total + 0 }')"
[[ "$AFTER" -gt "$BEFORE" ]] \
  || heavy_fail "batlehub_artifact_cache_hits_total did not move ($BEFORE → $AFTER): the second copy went upstream"
heavy_log "cache hits: $BEFORE → $AFTER"

# ── Refuse ───────────────────────────────────────────────────────────────────

heavy_log "Blocking $PKG_NAME@$PKG_VERSION"
heavy_block "$REG" "$PKG_NAME" "$PKG_VERSION"

heavy_mark blocked
RUN_OUT="$HEAVY_WORK/blocked.txt"
# The path is already in store-a and store-b, so a third, empty root is what
# makes the client want it again — but an empty *store* is not a cold client.
# Nix caches the narinfo itself in `$HOME/.cache/nix/binary-cache-v6.sqlite`
# for `narinfo-cache-positive-ttl` (30 days by default), and `$HOME` is the one
# directory every `nix_run` here shares. With that cache warm the client never
# re-asked: it went straight to the NAR it already had a `URL:` for, and the
# phase asserted a narinfo request that the run had no reason to make. Both
# TTLs are pinned to 0 in `nix_run` for that reason — the client's own cache
# must not decide what this suite reads off the wire.
if nix_copy store-c; then
  cat "$RUN_OUT" >&2
  heavy_fail "nix copy succeeded with $PKG_NAME@$PKG_VERSION blocked — the block did not reach the narinfo"
fi

# The narinfo is a `404`: the protocol's own "not in this cache".
#
# **`GET` or `HEAD`, because the refusal lands on whichever comes first.** Nix
# probes a binary cache with `fileExists` — a `HEAD` — before it reads, and a
# `404` there ends the substitution: the `GET` this assertion used to name never
# happens, so it asserted a request the run had no reason to make. The method is
# not the claim; the status on this path is.
heavy_wire_re_after blocked "(GET|HEAD) /proxy/$REG/nix/$STORE_HASH[.]narinfo -> 404" \
  "the blocked narinfo did not answer 404"

# **The absent request is the assertion.** With no narinfo, the client has no
# `URL:` to follow, so nothing may be asked for under this path's NAR prefix
# after the block. A block that only refused the NAR would leave every client
# holding a cached narinfo fetching bytes for up to 30 days. Counted over both
# methods for the reason above — a `HEAD` on a NAR is `nix copy`'s own probe,
# and it would be just as much of a leak as a `GET`.
NAR_AFTER_BLOCK="$(heavy_wire_count_after blocked "(GET|HEAD) /proxy/$REG/nix/nar/$STORE_HASH/")"
[[ "$NAR_AFTER_BLOCK" == 0 ]] \
  || heavy_fail "$NAR_AFTER_BLOCK request(s) reached the NAR route after the block — the narinfo 404 did not stop the download"

# And Nix's own words for it — *"there is no substituter that can build it"*,
# measured against 2.35.2 rather than assumed. The alternation carries the older
# spellings too, because the claim is the class of refusal and not one release's
# wording.
#
# It reads that way only because `nix_run` empties `substituters`. With the
# image's default list in place a `404` here sent the client on to
# cache.nixos.org, which then answered and was discarded for an unrelated reason
# — *"ignoring substitute … as it's not signed by any of the keys in
# 'trusted-public-keys'"*. The run still went red, but for the runner's key
# configuration rather than for the block, and on a machine that trusted that
# key it would have gone green with the path fetched from upstream: a false
# negative for the one claim this phase exists to make.
heavy_client_must_say "$RUN_OUT" \
  "does not exist|cannot be (built|realis)|not valid|no substituter that can build" \
  "nix did not report the missing path in its own terms"
heavy_log "nix reported the refusal as:"
tail -5 "$RUN_OUT" >&2

# ── Recover ──────────────────────────────────────────────────────────────────

heavy_log "Unblocking $PKG_NAME@$PKG_VERSION"
heavy_unblock "$REG" "$PKG_NAME" "$PKG_VERSION"

heavy_mark recovered
RUN_OUT="$HEAVY_WORK/recovered.txt"
# The *same* store root that saw the refusal: a recovery that needs a fresh
# client is not a recovery.
nix_copy store-c \
  || { cat "$RUN_OUT" >&2; heavy_fail "the copy did not recover after the block was lifted"; }
heavy_wire_after recovered "GET /proxy/$REG/nix/$STORE_HASH.narinfo -> 200" \
  "the narinfo is still refused after the block was lifted"

# ── The upstream-shaped NAR URL ───────────────────────────────────────────────
#
# Not driven by the client — it would need a narinfo cached from before this
# registry existed, which a fresh run cannot have. Driven by `curl`, because the
# request shape is the whole point and a client is not needed to send it
# (RFC 0028 §4.4).

heavy_log "The upstream-shaped NAR route"
UPSTREAM_URL="$(curl -fsS "https://cache.nixos.org/$STORE_HASH.narinfo" | awk '/^URL: /{print $2}')"
UPSTREAM_NAR="$(basename "$UPSTREAM_URL")"
heavy_mark upstream-shape
curl -fsS -o /dev/null "$HEAVY_TAP_BASE/proxy/$REG/nix/nar/$UPSTREAM_NAR" \
  || heavy_fail "a NAR asked for in the upstream's own shape was not resolved through the reverse index — a client whose narinfo predates this registry would be stuck for 30 days"
heavy_wire_after upstream-shape "GET /proxy/$REG/nix/nar/$UPSTREAM_NAR -> 200" \
  "the upstream-shape route did not answer"

# And an unknown one is a 404 rather than a pass-through, which is what makes
# the client refetch its narinfo instead of receiving bytes outside a coordinate.
heavy_mark upstream-shape-miss
UNKNOWN="$(curl -sS -o /dev/null -w '%{http_code}' \
  "$HEAVY_TAP_BASE/proxy/$REG/nix/nar/0000000000000000000000000000000000000000000000000000.nar.zst")"
[[ "$UNKNOWN" == 404 ]] \
  || heavy_fail "an unindexed upstream-shaped NAR answered $UNKNOWN, not 404 — a NAR must never be served outside a coordinate"

# ── Publish (RFC 0028 §6.10 items 4–5) ───────────────────────────────────────
#
# `nix copy --to` against the local registry. Everything before this point was
# reads; this is the half RFC 0031 §13's lesson is about, and the half whose
# facts were re-read from `libstore` before any of it was written.

LOCAL="nix-local-$HEAVY_RUN"
PUBLISH_TO="$HEAVY_TAP_BASE/proxy/$LOCAL/nix"

heavy_log "Publish — nix copy --to a local registry"

# A path to publish. `nix store add-path` on a file we create makes one without
# needing a build, an evaluation or nixpkgs — the suite is testing this server,
# not Nix's evaluator.
mkdir -p "$HEAVY_WORK/topublish"
date > "$HEAVY_WORK/topublish/stamp.txt"
MINE="$(nix_run store add-path --store "local?root=/work/store-pub" \
  --name heavy-probe-1.0 /work/topublish 2>"$HEAVY_WORK/addpath.err")" \
  || { cat "$HEAVY_WORK/addpath.err" >&2; heavy_fail "could not create a store path to publish"; }
heavy_log "publishing $MINE"
MINE_HASH="$(basename "$MINE" | cut -c1-32)"

heavy_mark publish
RUN_OUT="$HEAVY_WORK/publish.txt"
nix_run copy --to "$PUBLISH_TO" --from "local?root=/work/store-pub" "$MINE" \
  >"$RUN_OUT" 2>&1 \
  || { cat "$RUN_OUT" >&2; heavy_fail "nix copy --to the local registry failed"; }

# The two requests, in the order `addToStoreCommon` and `uploadNarInfo` send
# them — and the HEAD, which is the one RFC 0028 §4.4 puts on the wrong path.
heavy_wire_re_after publish "PUT /proxy/$LOCAL/nix/nar/[^ ]+ -> 200" \
  "the NAR was not PUT — the publish did not reach this server"
heavy_wire_after publish "PUT /proxy/$LOCAL/nix/$MINE_HASH.narinfo -> 200" \
  "the narinfo was not PUT, so nothing claimed the NAR"

# **The signature, verified by the client rather than asserted by us.** A fresh
# store that trusts only this registry's key copies the path back: if the
# narinfo were unsigned, or signed over the wrong fingerprint, `nix` refuses it
# with "lacks a signature by a trusted key" and this fails.
PUBKEY="$(curl -fsS "$HEAVY_TAP_BASE/proxy/$LOCAL/nix/public-key")" \
  || heavy_fail "the registry serves no public key, so nothing can trust what it publishes"
heavy_log "registry key: $PUBKEY"

heavy_mark readback
RUN_OUT="$HEAVY_WORK/readback.txt"
nix_run copy --from "$PUBLISH_TO" --to "local?root=/work/store-verify" \
  --option trusted-public-keys "$PUBKEY" \
  --option require-sigs true \
  "$MINE" >"$RUN_OUT" 2>&1 \
  || { cat "$RUN_OUT" >&2; heavy_fail "a client trusting only this registry's key could not copy back what it published — the signature is missing or over the wrong bytes"; }
heavy_log "the published path verifies against the registry's own key"

# …and the same client with the key **absent** must refuse it, or the assertion
# above proves nothing: a client that accepts everything would have passed it.
heavy_mark unsigned
RUN_OUT="$HEAVY_WORK/untrusted.txt"
if nix_run copy --from "$PUBLISH_TO" --to "local?root=/work/store-untrusted" \
  --option trusted-public-keys "someone-else-1:0000000000000000000000000000000000000000000=" \
  --option require-sigs true \
  "$MINE" >"$RUN_OUT" 2>&1; then
  cat "$RUN_OUT" >&2
  heavy_fail "a client trusting a *different* key accepted the path — require-sigs is not being enforced, and the positive control above is worthless"
fi
heavy_client_must_say "$RUN_OUT" "signature|trusted" \
  "nix refused the path but not for the signature — the control is measuring the wrong thing"
heavy_log "and a client without the key refuses it, which is what makes the check above mean something"

# ── A narinfo the bytes disagree with is refused ─────────────────────────────
#
# Driven by curl: `nix` will not send a document that disagrees with what it
# just uploaded, and that is exactly the case a hostile or broken publisher is.

heavy_log "A narinfo whose NarHash does not match the bytes"
heavy_mark badhash
GOOD_NARINFO="$(curl -fsS "$HEAVY_TAP_BASE/proxy/$LOCAL/nix/$MINE_HASH.narinfo")"
BAD_NARINFO="$(printf '%s\n' "$GOOD_NARINFO" \
  | sed 's|^NarHash: .*|NarHash: sha256:0000000000000000000000000000000000000000000000000000|')"
STATUS="$(printf '%s\n' "$BAD_NARINFO" \
  | curl -sS -o "$HEAVY_WORK/badhash.out" -w '%{http_code}' \
      -X PUT --data-binary @- \
      -H "Authorization: Bearer $ADMIN_TOKEN" \
      "$HEAVY_TAP_BASE/proxy/$LOCAL/nix/$MINE_HASH.narinfo")"
[[ "$STATUS" == 400 ]] \
  || heavy_fail "a narinfo disagreeing with its bytes answered $STATUS, not 400 — this server would have signed a hash it never checked"
grep -qiE "NarHash|FileHash|nar" "$HEAVY_WORK/badhash.out" \
  || heavy_fail "the refusal does not say which field disagreed"
heavy_log "refused, naming the field: $(head -c 200 "$HEAVY_WORK/badhash.out")"

# And the path it names is still the *good* one — a refused publish must not
# replace what was already there.
heavy_mark still-good
NOW="$(curl -fsS "$HEAVY_TAP_BASE/proxy/$LOCAL/nix/$MINE_HASH.narinfo")"
[[ "$NOW" == "$GOOD_NARINFO" ]] \
  || heavy_fail "the refused publish changed the served narinfo"

heavy_done "nix heavy suite passed"
