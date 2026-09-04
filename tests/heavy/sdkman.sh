#!/usr/bin/env bash
# Heavy SDKMAN integration test — the real `sdk` against an `sdkman` registry.
#
# RFC 0010 §10 calls this the load-bearing part of the plan, and says why:
# everything else about `sdkman` is written from our own reading of the
# sdkman-cli bash sources. The fact the whole enforcement design rests on —
# that `candidates/validate` answering `invalid` makes `sdk install` stop on
# its own "Stop! … is not a valid java version." before any download — is
# *read, not observed* until this script runs.
#
# What this proves, on the wire (the tap sits between `sdk` and BatleHub; it
# records what the *client* asked for, which is the only evidence that counts):
#
#   1. `sdk list` and `sdk list java` render through the proxy's
#      `candidates/list` and the rendered `versions/list`, with the query the
#      client always sends; every `sdk` invocation reads `healthcheck`.
#   2. A version blocked through the admin API is refused: `sdk install java
#      <that version>` prints SDKMAN's own "is not a valid java version" and
#      requests **nothing** from the broker. The block stops the request, not
#      merely the install. It is absent from `sdk list java`'s vendor table,
#      and a blocked Maven version is blanked from `sdk list maven`'s grid with
#      the table still the same shape.
#   3. `sdk install java <allowed version>` downloads through the broker route
#      (the server follows the 302 to the vendor's CDN), runs the relayed
#      post-install hook (tar + zip, so the hook came through byte-exact), and
#      the installed `java -version` answers.
#   4. A second install of the same JDK, from a fresh `$SDKMAN_DIR`, is served
#      from the proxy's cache: the client asks again, and the server's
#      cache-hit counter moves.
#
# `sdk` is a shell function, not a binary, so `heavy_need` cannot ask for it.
# It is taken from the release zip the broker serves (not the `curl | bash`
# installer — RFC 0010 decision 9), and `$SDKMAN_DIR` is assembled by hand
# with the two files `sdkman-init.sh` reads at startup — `var/platform` and
# `var/candidates`, the latter fetched through the proxy — so a suite can
# never install a JDK over the runner's own and two matrix jobs cannot collide.
#
# Run via `task test:sdkman-heavy` or directly. Needs network: the upstreams
# are api.sdkman.io, broker.sdkman.io and whichever CDN the broker names.
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8106), HEAVY_TAP_PORT
# (8116), COVERAGE, HEAVY_SDKMAN_VERSION (5.23.0, the sdkman-cli release),
# HEAVY_SDKMAN_PROBE (the java version installed; default: the API's default),
# HEAVY_SDKMAN_BLOCKED (the java version blocked; default: another `-tem`
# build), HEAVY_SDKMAN_GRID (maven — the candidate whose grid layout is
# checked).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init sdkman 8106 8116
heavy_need curl "curl"
heavy_need unzip "unzip (sdkman-cli ships as a zip, and every install unzips)"
heavy_need zip "zip (the Java post-install hook repackages the tarball with it)"
heavy_need tar "tar"
heavy_need python3 "python3 (the wire tap)"

REG="jvm-$HEAVY_RUN"
CLI_VERSION="${HEAVY_SDKMAN_VERSION:-5.23.0}"
GRID="${HEAVY_SDKMAN_GRID:-maven}"

heavy_start_server tests/heavy/config.sdkman.toml
heavy_start_tap

API="$HEAVY_TAP_BASE/proxy/$REG/sdkman"
BROKER="$API/broker"

# ── 0. sdkman-cli itself ─────────────────────────────────────────────────────

CLI_SRC="$(heavy_cached_dir "sdkman-cli-$CLI_VERSION" \
  "https://github.com/sdkman/sdkman-cli/releases/download/$CLI_VERSION/sdkman-cli-$CLI_VERSION.zip" zip)"
CLI_ROOT="$CLI_SRC/sdkman-$CLI_VERSION"
[[ -f "$CLI_ROOT/bin/sdkman-init.sh" ]] || heavy_fail "sdkman-init.sh not found under $CLI_SRC"

# make_sdkman_dir <dir> — what the installer would have left behind, minus the
# installer: the scripts, the platform file, the candidate cache (read through
# the proxy), and a config that answers every prompt and disables the parts
# that would phone home on their own.
make_sdkman_dir() {
  local dir="$1"
  mkdir -p "$dir"/{bin,src,contrib,var,tmp,etc,ext,candidates}
  cp -R "$CLI_ROOT/bin/." "$dir/bin/"
  cp -R "$CLI_ROOT/src/." "$dir/src/"
  cp -R "$CLI_ROOT/contrib/." "$dir/contrib/"
  echo "linuxx64" > "$dir/var/platform"
  touch "$dir/var/delay_upgrade"
  curl -fsS "$API/candidates/all" > "$dir/var/candidates" \
    || heavy_fail "could not read candidates/all through the proxy"
  [[ -s "$dir/var/candidates" ]] || heavy_fail "candidates/all answered an empty body"
  cat > "$dir/etc/config" <<'EOF'
sdkman_auto_answer=true
sdkman_auto_selfupdate=false
sdkman_selfupdate_feature=false
sdkman_insecure_ssl=false
sdkman_curl_connect_timeout=7
sdkman_curl_max_time=30
sdkman_beta_channel=false
sdkman_debug_mode=false
sdkman_colour_enable=false
sdkman_auto_env=false
sdkman_auto_complete=false
sdkman_checksum_enable=true
sdkman_native_enable=false
sdkman_healthcheck_enable=true
EOF
}

# run_sdk <sdkman-dir> <shell snippet> — a fresh bash with sdkman-init.sh
# sourced, `$SDKMAN_DIR` pointed at <sdkman-dir> and both API variables at the
# tap. The variables are exported *before* sourcing, which is the only place
# they take effect: sdkman-init.sh sets each one only when it is empty. The
# client's existence is asserted after sourcing, inside the same shell,
# because that is the only place a shell function exists.
run_sdk() {
  local dir="$1" snippet="$2"
  SDKMAN_DIR="$dir" SDKMAN_CANDIDATES_API="$API" SDKMAN_BROKER_API="$BROKER" bash -c '
    set -o pipefail
    export SDKMAN_DIR SDKMAN_CANDIDATES_API SDKMAN_BROKER_API
    # shellcheck disable=SC1090
    source "$SDKMAN_DIR/bin/sdkman-init.sh"
    [[ "$(type -t sdk)" == "function" ]] || { echo "sdkman-init.sh did not define sdk" >&2; exit 97; }
    eval "$1"
  ' _ "$snippet"
}

DIR1="$HEAVY_WORK/sdkman-1"
DIR2="$HEAVY_WORK/sdkman-2"

heavy_mark "bootstrap"
make_sdkman_dir "$DIR1"
heavy_wire_after "bootstrap" "GET /proxy/$REG/sdkman/candidates/all -> 200" \
  "the candidate cache was not read through the proxy"
grep -qw java "$DIR1/var/candidates" || heavy_fail "java is not among the candidates the proxy relayed"
grep -qw "$GRID" "$DIR1/var/candidates" || heavy_fail "$GRID is not among the candidates the proxy relayed"

# The versions under test come from the API rather than from a pin: SDKMAN
# removes older JDK builds as vendors retire them, so a pinned version would
# make this suite fail on a calendar rather than on a regression.
PROBE="${HEAVY_SDKMAN_PROBE:-$(curl -fsS "$API/candidates/default/java")}"
[[ -n "$PROBE" ]] || heavy_fail "candidates/default/java answered nothing"
ALL_JAVA="$(curl -fsS "$API/candidates/java/linuxx64/versions/all")"
BLOCKED="${HEAVY_SDKMAN_BLOCKED:-$(tr ',' '\n' <<<"$ALL_JAVA" | grep -- '-tem$' | grep -vx "$PROBE" | sort -V | tail -1)}"
[[ -n "$BLOCKED" ]] || heavy_fail "no second Temurin build to block in versions/all"
[[ "$PROBE" != "$BLOCKED" ]] || heavy_fail "HEAVY_SDKMAN_PROBE and HEAVY_SDKMAN_BLOCKED must differ"
ALL_GRID="$(curl -fsS "$API/candidates/$GRID/linuxx64/versions/all")"
GRID_DEFAULT="$(curl -fsS "$API/candidates/default/$GRID")"
GRID_BLOCKED="$(tr ',' '\n' <<<"$ALL_GRID" | grep -vx "$GRID_DEFAULT" | grep -v -- '-' | tail -1)"
[[ -n "$GRID_BLOCKED" ]] || heavy_fail "no $GRID version to block in versions/all"
heavy_log "java: install $PROBE, block $BLOCKED; $GRID: block $GRID_BLOCKED (default $GRID_DEFAULT)"

# ── 1. The listings ──────────────────────────────────────────────────────────

heavy_mark "list"
heavy_log "sdk list (candidates) through $API"
run_sdk "$DIR1" 'PAGER=cat sdk list' >"$HEAVY_WORK/list-candidates.txt" 2>&1 \
  || { cat "$HEAVY_WORK/list-candidates.txt" >&2; heavy_fail "sdk list failed"; }
grep -q "Java" "$HEAVY_WORK/list-candidates.txt" \
  || { cat "$HEAVY_WORK/list-candidates.txt" >&2; heavy_fail "sdk list did not render the candidate table"; }
heavy_wire_after "list" "GET /proxy/$REG/sdkman/healthcheck -> 200" \
  "sdk did not read healthcheck through the proxy (every invocation does)"
heavy_wire_after "list" "GET /proxy/$REG/sdkman/candidates/list -> 200" \
  "sdk list did not read candidates/list through the proxy"

heavy_log "sdk list java"
run_sdk "$DIR1" 'PAGER=cat sdk list java' >"$HEAVY_WORK/list-java-before.txt" 2>&1 \
  || { cat "$HEAVY_WORK/list-java-before.txt" >&2; heavy_fail "sdk list java failed"; }
grep -qF "| $BLOCKED" "$HEAVY_WORK/list-java-before.txt" \
  || heavy_fail "$BLOCKED is not in sdk list java before the block — pick a HEAVY_SDKMAN_BLOCKED the API still lists"
grep -qF "| $PROBE" "$HEAVY_WORK/list-java-before.txt" \
  || heavy_fail "$PROBE is not in sdk list java — pick a HEAVY_SDKMAN_PROBE the API still lists"
heavy_wire_after "list" "GET /proxy/$REG/sdkman/candidates/java/linuxx64/versions/list?current=&installed= -> 200" \
  "sdk list java did not read the rendered list through the proxy with the query it always sends"

run_sdk "$DIR1" "PAGER=cat sdk list $GRID" >"$HEAVY_WORK/list-grid-before.txt" 2>&1 \
  || { cat "$HEAVY_WORK/list-grid-before.txt" >&2; heavy_fail "sdk list $GRID failed"; }
grep -qE "(^|[[:space:]])${GRID_BLOCKED//./\\.}([[:space:]]|$)" "$HEAVY_WORK/list-grid-before.txt" \
  || heavy_fail "$GRID_BLOCKED is not in sdk list $GRID before the block"
heavy_log "SDKMAN-LIST-OK ($(grep -c '|' "$HEAVY_WORK/list-java-before.txt") java rows rendered)"

# ── 2. Block, then ask sdk for the blocked version by name ───────────────────
#
# Straight to the server rather than through the tap: the admin calls are
# ours, and the transcript should hold the client's requests and nothing else.

heavy_log "Blocking java $BLOCKED and $GRID $GRID_BLOCKED through the admin API"
heavy_block "$REG" java "$BLOCKED"
heavy_block "$REG" "$GRID" "$GRID_BLOCKED"

heavy_mark "refusal"
heavy_log "sdk install java $BLOCKED (blocked) — expecting SDKMAN's own refusal"
if run_sdk "$DIR1" "sdk install java $BLOCKED" >"$HEAVY_WORK/install-blocked.txt" 2>&1; then
  cat "$HEAVY_WORK/install-blocked.txt" >&2
  heavy_fail "sdk install java $BLOCKED succeeded — the block did not reach candidates/validate"
fi
# Observed, not read: 5.23.0's `__sdkman_determine_version` (sdkman-env-helpers.sh)
# refuses an `invalid` answer with "Stop! java X is not available. Possible
# causes: * X is an invalid version …" — the "is not a valid java version" line
# RFC 0010 §4.4 quotes is sdkman-install.sh's fallback, which is never reached.
grep -qE "is not a valid java version|is an invalid version" "$HEAVY_WORK/install-blocked.txt" || {
  cat "$HEAVY_WORK/install-blocked.txt" >&2
  heavy_fail "sdk install java $BLOCKED failed, but not on SDKMAN's own invalid-version path (RFC 0010 §4.4)"
}
heavy_wire_after "refusal" "GET /proxy/$REG/sdkman/candidates/validate/java/$BLOCKED/linuxx64 -> 200" \
  "sdk did not ask candidates/validate through the proxy, so validate is not the chokepoint"
# The `invalid` is the block: nothing may be requested from the broker.
heavy_wire_not "GET /proxy/$REG/sdkman/broker/download/java/$BLOCKED/" \
  "sdk requested the blocked JDK from the broker — the block stopped the install, not the request"
heavy_wire_not "GET /proxy/$REG/sdkman/hooks/post/java/$BLOCKED/" \
  "sdk fetched the post-install hook of a blocked version — it got past validate"
[[ -e "$DIR1/candidates/java/$BLOCKED" ]] && heavy_fail "a blocked JDK was installed into $DIR1"

heavy_log "sdk list java after the block"
run_sdk "$DIR1" 'PAGER=cat sdk list java' >"$HEAVY_WORK/list-java-after.txt" 2>&1 \
  || heavy_fail "sdk list java failed after the block"
grep -qF "| $BLOCKED" "$HEAVY_WORK/list-java-after.txt" \
  && heavy_fail "$BLOCKED is still listed by sdk list java after being blocked"
grep -qF "| $PROBE" "$HEAVY_WORK/list-java-after.txt" \
  || heavy_fail "$PROBE disappeared from sdk list java — the filter removed the wrong row"
# Every vendor block still has its name: the table's first column is
# populated on the same rows as before, minus the one that went.
[[ "$(grep -c '^ [A-Za-z]' "$HEAVY_WORK/list-java-after.txt")" -ge "$(( $(grep -c '^ [A-Za-z]' "$HEAVY_WORK/list-java-before.txt") - 1 ))" ]] \
  || heavy_fail "a vendor block lost its name after the block (RFC 0010 §4.4: the vendor cell is promoted)"

heavy_log "sdk list $GRID after the block"
run_sdk "$DIR1" "PAGER=cat sdk list $GRID" >"$HEAVY_WORK/list-grid-after.txt" 2>&1 \
  || heavy_fail "sdk list $GRID failed after the block"
grep -qE "(^|[[:space:]])${GRID_BLOCKED//./\\.}([[:space:]]|$)" "$HEAVY_WORK/list-grid-after.txt" \
  && heavy_fail "$GRID_BLOCKED is still in sdk list $GRID after being blocked"
[[ "$(wc -l <"$HEAVY_WORK/list-grid-after.txt")" == "$(wc -l <"$HEAVY_WORK/list-grid-before.txt")" ]] \
  || heavy_fail "sdk list $GRID changed shape after the block — a cell is blanked, never re-packed (RFC 0010 §4.4)"
heavy_log "SDKMAN-REFUSAL-OK (SDKMAN's own not-valid, nothing requested from the broker)"

# ── 3. Install an allowed JDK ────────────────────────────────────────────────

heavy_mark "install"
heavy_log "sdk install java $PROBE"
run_sdk "$DIR1" "sdk install java $PROBE" >"$HEAVY_WORK/install.txt" 2>&1 \
  || { cat "$HEAVY_WORK/install.txt" >&2; heavy_fail "sdk install java $PROBE failed"; }
grep -q "Done installing" "$HEAVY_WORK/install.txt" || {
  cat "$HEAVY_WORK/install.txt" >&2
  heavy_fail "sdk did not report the install done"
}
heavy_wire_after "install" "GET /proxy/$REG/sdkman/candidates/validate/java/$PROBE/linuxx64 -> 200" \
  "sdk did not validate the version through the proxy"
heavy_wire_after "install" "GET /proxy/$REG/sdkman/broker/download/java/$PROBE/linuxx64 -> 200" \
  "the JDK was not downloaded through the proxy's broker route"
heavy_wire_after "install" "GET /proxy/$REG/sdkman/hooks/post/java/$PROBE/linuxx64 -> 200" \
  "the post-install hook was not read through the proxy"
JAVA_BIN="$DIR1/candidates/java/$PROBE/bin/java"
[[ -x "$JAVA_BIN" ]] || { ls -la "$DIR1/candidates/java/" >&2; heavy_fail "no java binary under $DIR1/candidates/java/$PROBE — the relayed hook did not repackage the tarball"; }
INSTALLED="$("$JAVA_BIN" -version 2>&1 | head -1)"
[[ -n "$INSTALLED" ]] || heavy_fail "the installed java -version answered nothing"
heavy_log "SDKMAN-INSTALL-OK ($INSTALLED)"

# ── 4. The cache: a second agent asks, upstream is not asked ─────────────────
#
# The tap cannot see the proxy's upstream side, so the negative is read from
# the server's own cache-hit counter. The transcript still proves the *client*
# asked the proxy again, which is what distinguishes a cache hit from sdk
# reusing its own copy.

hits_for() {
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk -v reg="$REG" '$1 ~ /^batlehub_artifact_cache_hits_total\{/ && index($1, "registry=\"" reg "\"") { print $2 }'
}
HITS_BEFORE="$(hits_for)"; HITS_BEFORE="${HITS_BEFORE:-0}"

heavy_mark "cache"
heavy_log "sdk install java $PROBE again, from a fresh SDKMAN_DIR"
make_sdkman_dir "$DIR2"
run_sdk "$DIR2" "sdk install java $PROBE" >"$HEAVY_WORK/install-2.txt" 2>&1 \
  || { cat "$HEAVY_WORK/install-2.txt" >&2; heavy_fail "the second sdk install java $PROBE failed"; }
heavy_wire_after "cache" "GET /proxy/$REG/sdkman/broker/download/java/$PROBE/linuxx64 -> 200" \
  "the second install did not ask the proxy for the JDK, so nothing about the cache was exercised"
HITS_AFTER="$(hits_for)"; HITS_AFTER="${HITS_AFTER:-0}"
[[ "${HITS_AFTER%.*}" -gt "${HITS_BEFORE%.*}" ]] \
  || heavy_fail "batlehub_artifact_cache_hits_total for $REG did not move ($HITS_BEFORE -> $HITS_AFTER): the second install went upstream"
heavy_log "SDKMAN-CACHE-OK (cache hits $HITS_BEFORE -> $HITS_AFTER)"

heavy_done SDKMAN-HEAVY-OK
