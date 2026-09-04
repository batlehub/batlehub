#!/usr/bin/env bash
# Heavy nvm integration test — the real nvm against a `nodedist` registry.
#
# RFC 0010 §10 calls this the load-bearing part of the plan, and says why:
# everything else about `nodedist` is written from our own reading of nvm.sh.
# The fact the whole enforcement design rests on — that a release absent from
# `index.tab` makes `nvm install <exact-version>` stop with nvm's own
# "not found", before any download — is *read, not observed* until this script
# runs. RFC 0009 §12 is the record of what skipping that costs: seven ecosystems
# driven by their real clients found twelve bugs every route test had passed.
#
# What this proves, on the wire (the tap sits between nvm and BatleHub; it
# records what the *client* asked for, which is the only evidence that counts):
#
#   1. `nvm ls-remote` lists releases through the proxy's `index.tab`.
#   2. A release blocked through the admin API is absent from `nvm ls-remote`,
#      and `nvm install <that exact version>` prints nvm's *"Version … not
#      found"* and requests **nothing** under that release's directory. The
#      block stops the request, not merely the install.
#   3. `nvm install <allowed version>` fetches `SHASUMS256.txt` and the tarball
#      through the proxy, nvm reports the checksum matched (so the checksum
#      file came through byte-exact), and the installed `node -v` answers.
#   4. A second install of the same release, from a fresh `$NVM_DIR`, is served
#      from the proxy's cache: the client asks again, and the server's cache-hit
#      counter moves.
#
# nvm is a shell function, not a binary, so `heavy_need` cannot ask for it. It
# is taken from the release tarball (not the `curl | bash` installer — RFC 0010
# decision 9), sourced into a fresh bash per phase with `$NVM_DIR` redirected
# into the run's temp directory, so a suite can never install a Node over the
# runner's own and two matrix jobs cannot collide.
#
# Run via `task test:nvm-heavy` or directly. Needs network: the upstream is
# nodejs.org. Environment knobs: DATABASE_URL (required), HEAVY_PORT (8090),
# HEAVY_TAP_PORT (8100), COVERAGE, HEAVY_NVM_VERSION (0.40.3),
# HEAVY_NVM_PROBE (22.11.0 — the release installed), HEAVY_NVM_BLOCKED
# (22.10.0 — the release blocked; must have a linux-x64 binary too).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init nvm 8090 8100
heavy_need curl "curl"
heavy_need tar "tar"
heavy_need python3 "python3 (the wire tap)"

REG="node-$HEAVY_RUN"
NVM_VERSION="${HEAVY_NVM_VERSION:-0.40.3}"
PROBE="${HEAVY_NVM_PROBE:-22.11.0}"
BLOCKED="${HEAVY_NVM_BLOCKED:-22.10.0}"
[[ "$PROBE" != "$BLOCKED" ]] || heavy_fail "HEAVY_NVM_PROBE and HEAVY_NVM_BLOCKED must differ"

# The compression nvm will pick for a modern release: `.tar.xz` when `xz` is on
# PATH (nvm_supports_xz), `.tar.gz` otherwise. Asserted on the wire below, so
# it has to be known here rather than guessed at from the transcript.
if command -v xz >/dev/null 2>&1; then TARBALL_EXT="tar.xz"; else TARBALL_EXT="tar.gz"; fi

heavy_start_server tests/heavy/config.nvm.toml
heavy_start_tap

MIRROR="$HEAVY_TAP_BASE/proxy/$REG/nodedist"

# ── 0. nvm itself ────────────────────────────────────────────────────────────

NVM_SRC="$(heavy_cached_dir "nvm-$NVM_VERSION" \
  "https://github.com/nvm-sh/nvm/archive/refs/tags/v$NVM_VERSION.tar.gz")"
NVM_SH="$NVM_SRC/nvm-$NVM_VERSION/nvm.sh"
[[ -f "$NVM_SH" ]] || heavy_fail "nvm.sh not found under $NVM_SRC"

# run_nvm <nvm-dir> <shell snippet> — a fresh bash with nvm sourced, `$NVM_DIR`
# pointed at <nvm-dir> and the mirror pointed at the tap. `--no-use` keeps the
# source from activating anything on its own. The client's existence is
# asserted *after* sourcing, inside the same shell, because that is the only
# place a shell function exists.
run_nvm() {
  local dir="$1" snippet="$2"
  mkdir -p "$dir"
  NVM_DIR="$dir" NVM_NODEJS_ORG_MIRROR="$MIRROR" bash -c '
    set -o pipefail
    export NVM_DIR NVM_NODEJS_ORG_MIRROR
    # shellcheck disable=SC1090
    source "$1" --no-use
    [[ "$(type -t nvm)" == "function" ]] || { echo "nvm.sh did not define nvm" >&2; exit 97; }
    eval "$2"
  ' _ "$NVM_SH" "$snippet"
}

DIR1="$HEAVY_WORK/nvm-1"
DIR2="$HEAVY_WORK/nvm-2"

# ── 1. The listing ───────────────────────────────────────────────────────────

heavy_mark "ls-remote"
heavy_log "nvm ls-remote through $MIRROR"
run_nvm "$DIR1" 'nvm ls-remote --no-colors' >"$HEAVY_WORK/ls-remote-before.txt" 2>&1 \
  || { cat "$HEAVY_WORK/ls-remote-before.txt" >&2; heavy_fail "nvm ls-remote failed"; }
grep -q "v$PROBE" "$HEAVY_WORK/ls-remote-before.txt" \
  || heavy_fail "v$PROBE is not in nvm ls-remote — pick a HEAVY_NVM_PROBE the tree still lists"
grep -q "v$BLOCKED" "$HEAVY_WORK/ls-remote-before.txt" \
  || heavy_fail "v$BLOCKED is not in nvm ls-remote before the block — pick a HEAVY_NVM_BLOCKED the tree still lists"
heavy_wire_after "ls-remote" "GET /proxy/$REG/nodedist/index.tab -> 200" \
  "nvm did not read index.tab through the proxy"
heavy_log "NVM-LS-REMOTE-OK ($(grep -c '^ *v' "$HEAVY_WORK/ls-remote-before.txt") releases listed)"

# ── 2. Block a release, then ask nvm for it by exact version ─────────────────
#
# Straight to the server rather than through the tap: the admin call is ours,
# and the transcript should hold the client's requests and nothing else.

heavy_log "Blocking node v$BLOCKED through the admin API"
curl -fsS -X POST "$HEAVY_BASE/api/v1/admin/packages/block" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
  -d "{\"registry\":\"$REG\",\"name\":\"node\",\"version\":\"v$BLOCKED\",\"reason\":\"heavy: administratively blocked\"}" \
  >"$HEAVY_WORK/block.json" || { cat "$HEAVY_WORK/block.json" >&2; heavy_fail "the block request failed"; }

heavy_mark "refusal"
heavy_log "nvm install $BLOCKED (blocked) — expecting nvm's own not-found"
if run_nvm "$DIR1" "nvm install $BLOCKED" >"$HEAVY_WORK/install-blocked.txt" 2>&1; then
  cat "$HEAVY_WORK/install-blocked.txt" >&2
  heavy_fail "nvm install $BLOCKED succeeded — the block did not reach index.tab"
fi
grep -q "not found" "$HEAVY_WORK/install-blocked.txt" || {
  cat "$HEAVY_WORK/install-blocked.txt" >&2
  heavy_fail "nvm install $BLOCKED failed, but not on nvm's own not-found path (RFC 0010 §4.4)"
}
# The row is the block: nothing under the release directory may be requested.
heavy_wire_not "GET /proxy/$REG/nodedist/v$BLOCKED/" \
  "nvm requested a file of the blocked release — the block stopped the install, not the request"
heavy_wire_after "refusal" "GET /proxy/$REG/nodedist/index.tab -> 200" \
  "nvm resolved the exact version without reading index.tab, so the listing is not the chokepoint"
[[ -e "$DIR1/versions/node/v$BLOCKED" ]] && heavy_fail "a blocked release was installed into $DIR1"

heavy_log "nvm ls-remote after the block"
run_nvm "$DIR1" 'nvm ls-remote --no-colors' >"$HEAVY_WORK/ls-remote-after.txt" 2>&1 \
  || heavy_fail "nvm ls-remote failed after the block"
grep -q "v$BLOCKED" "$HEAVY_WORK/ls-remote-after.txt" \
  && heavy_fail "v$BLOCKED is still listed by nvm ls-remote after being blocked"
grep -q "v$PROBE" "$HEAVY_WORK/ls-remote-after.txt" \
  || heavy_fail "v$PROBE disappeared from nvm ls-remote — the filter removed the wrong row"
heavy_log "NVM-REFUSAL-OK (nvm's own not-found, nothing requested under v$BLOCKED)"

# ── 3. Install an allowed release ────────────────────────────────────────────

heavy_mark "install"
heavy_log "nvm install $PROBE"
run_nvm "$DIR1" "nvm install $PROBE" >"$HEAVY_WORK/install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/install.txt" >&2; heavy_fail "nvm install $PROBE failed"; }
grep -qi "checksums matched" "$HEAVY_WORK/install.txt" || {
  cat "$HEAVY_WORK/install.txt" >&2
  heavy_fail "nvm did not report a checksum match — SHASUMS256.txt did not come through byte-exact (RFC 0010 §7)"
}
heavy_wire_after "install" "GET /proxy/$REG/nodedist/v$PROBE/SHASUMS256.txt -> 200" \
  "nvm did not read SHASUMS256.txt through the proxy"
heavy_wire_after "install" "GET /proxy/$REG/nodedist/v$PROBE/node-v$PROBE-linux-x64.$TARBALL_EXT -> 200" \
  "the tarball was not fetched through the proxy"

INSTALLED="$(run_nvm "$DIR1" "nvm use $PROBE >/dev/null && node -v" 2>&1 | tail -1)"
[[ "$INSTALLED" == "v$PROBE" ]] || heavy_fail "node -v answered '$INSTALLED', expected v$PROBE"
heavy_log "NVM-INSTALL-OK (node $INSTALLED, checksum matched)"

# ── 4. The cache: a second agent asks, upstream is not asked ─────────────────
#
# The tap cannot see the proxy's upstream side, so the negative is read from
# the server's own cache-hit counter: it is the number the dashboard shows and
# it is exact. The transcript still proves the *client* asked the proxy again,
# which is what distinguishes a cache hit from nvm simply reusing its own copy.

hits_for() {
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk -v reg="$REG" '$1 ~ /^batlehub_artifact_cache_hits_total\{/ && index($1, "registry=\"" reg "\"") { print $2 }'
}
HITS_BEFORE="$(hits_for)"; HITS_BEFORE="${HITS_BEFORE:-0}"

heavy_mark "cache"
heavy_log "nvm install $PROBE again, from a fresh NVM_DIR"
run_nvm "$DIR2" "nvm install $PROBE" >"$HEAVY_WORK/install-2.txt" 2>&1 \
  || { cat "$HEAVY_WORK/install-2.txt" >&2; heavy_fail "the second nvm install $PROBE failed"; }
heavy_wire_after "cache" "GET /proxy/$REG/nodedist/v$PROBE/node-v$PROBE-linux-x64.$TARBALL_EXT -> 200" \
  "the second install did not ask the proxy for the tarball, so nothing about the cache was exercised"
HITS_AFTER="$(hits_for)"; HITS_AFTER="${HITS_AFTER:-0}"
[[ "${HITS_AFTER%.*}" -gt "${HITS_BEFORE%.*}" ]] \
  || heavy_fail "batlehub_artifact_cache_hits_total for $REG did not move ($HITS_BEFORE -> $HITS_AFTER): the second install went upstream"
heavy_log "NVM-CACHE-OK (cache hits $HITS_BEFORE -> $HITS_AFTER)"

heavy_done NVM-HEAVY-OK
