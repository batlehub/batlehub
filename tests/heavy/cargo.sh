#!/usr/bin/env bash
# Heavy cargo integration test — the real `cargo` against a cargo registry,
# measuring the four RFC 0018 §4.4 axes of a rejection.
#
# Nothing here is written from cargo's documentation; every row of the RFC's
# §4.2 tables for cargo is what this transcript shows. The tap sits between
# cargo and BatleHub, records what the *client* asked for, and — through the
# rewrite file (`heavy_tap_rewrite`) — answers with the statuses phase 2 will
# emit, so the client's reaction is measured before the server can produce it.
#
#   Hide     A blocked version is *marked yanked* in the sparse index (§11
#            decision 26, RFC 0006), not omitted: a fresh resolve picks the
#            previous version, and a Cargo.lock that pins the blocked one still
#            resolves and reaches the download gate.
#   Refuse   On the download path: what cargo prints for the native block
#            status and for the other one, whether it retries, and whether
#            `Retry-After` changes anything.
#   Recover  With the *same* CARGO_HOME that saw the refusal, the next
#            `cargo fetch` after the block lifts must succeed.
#   Publish  `cargo publish` to a local registry: the native status, then the
#            same upload with the tap answering `202 Accepted`.
#
# Run via `task test:cargo-heavy` or directly. Needs network: crates.io and
# index.crates.io. Environment knobs: DATABASE_URL (required), HEAVY_PORT
# (8102), HEAVY_TAP_PORT (8112), COVERAGE, HEAVY_CARGO_CRATE (unicode-xid),
# HEAVY_CARGO_BLOCKED (0.2.6, the newest), HEAVY_CARGO_PREVIOUS (0.2.5).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init cargo 8102 8112
heavy_need cargo "the Rust toolchain"
heavy_need python3 "python3 (the wire tap)"

REG="cargo-$HEAVY_RUN"
LOCAL="cargo-local-$HEAVY_RUN"
CRATE="${HEAVY_CARGO_CRATE:-unicode-xid}"
BLOCKED="${HEAVY_CARGO_BLOCKED:-0.2.6}"
PREVIOUS="${HEAVY_CARGO_PREVIOUS:-0.2.5}"

heavy_start_server tests/heavy/config.cargo.toml
heavy_start_tap

INDEX="sparse+$HEAVY_TAP_BASE/proxy/$REG/registry/"
LOCAL_INDEX="sparse+$HEAVY_TAP_BASE/proxy/$LOCAL/registry/"
DOWNLOAD="/proxy/$REG/$CRATE/$BLOCKED/download"

# consumer <dir> — a crate depending on `$CRATE = "0.2"` with crates.io
# replaced by the proxy. Each call gets its own CARGO_HOME (the registry cache
# and the index cache live there), so a phase starts from a client that has
# never seen the registry — except where the Recover axis reuses one on purpose.
consumer() {
  local dir="$1"
  mkdir -p "$dir/src" "$dir/.cargo"
  cat >"$dir/Cargo.toml" <<EOF
[package]
name = "heavy-consumer"
version = "0.0.0"
edition = "2021"

[dependencies]
$CRATE = "0.2"
EOF
  echo 'fn main() {}' >"$dir/src/main.rs"
  cat >"$dir/.cargo/config.toml" <<EOF
[source.crates-io]
replace-with = "heavy"

[source.heavy]
registry = "$INDEX"
EOF
  return $?
}

# run_cargo <home> <dir> <args...> — cargo with its own home, output captured
# in the file named by RUN_OUT; the exit code is returned.
run_cargo() {
  local home="$1" dir="$2"
  shift 2
  mkdir -p "$home"
  (cd "$dir" && CARGO_HOME="$home" CARGO_TERM_COLOR=never CARGO_NET_RETRY=2 \
    cargo "$@") >"$RUN_OUT" 2>&1
  return $?
}

# ── Hide ─────────────────────────────────────────────────────────────────────

heavy_mark fresh-before
RUN_OUT="$HEAVY_WORK/fresh-before.txt"
consumer "$HEAVY_WORK/c0"
run_cargo "$HEAVY_WORK/home0" "$HEAVY_WORK/c0" generate-lockfile \
  || { cat "$RUN_OUT" >&2; heavy_fail "cargo generate-lockfile through the proxy failed"; }
grep -q "name = \"$CRATE\"" "$HEAVY_WORK/c0/Cargo.lock" || heavy_fail "the lockfile does not name $CRATE"
grep -A1 "name = \"$CRATE\"" "$HEAVY_WORK/c0/Cargo.lock" | grep -q "version = \"$BLOCKED\"" \
  || heavy_fail "before the block, $CRATE resolves to something other than $BLOCKED — set HEAVY_CARGO_BLOCKED to the newest 0.2.x"
heavy_wire_after fresh-before "GET /proxy/$REG/registry/config.json -> 200"
cp "$HEAVY_WORK/c0/Cargo.lock" "$HEAVY_WORK/pinned.lock"

heavy_log "Blocking $CRATE@$BLOCKED"
heavy_block "$REG" "$CRATE" "$BLOCKED"

heavy_mark fresh-after
RUN_OUT="$HEAVY_WORK/fresh-after.txt"
consumer "$HEAVY_WORK/c1"
run_cargo "$HEAVY_WORK/home1" "$HEAVY_WORK/c1" generate-lockfile \
  || { cat "$RUN_OUT" >&2; heavy_fail "a fresh resolve with $BLOCKED blocked failed instead of picking the previous version"; }
grep -A1 "name = \"$CRATE\"" "$HEAVY_WORK/c1/Cargo.lock" | grep -q "version = \"$PREVIOUS\"" \
  || { cat "$HEAVY_WORK/c1/Cargo.lock" >&2; heavy_fail "Hide: a fresh resolve did not pick $PREVIOUS"; }
heavy_log "Hide/fresh: $CRATE resolves to $PREVIOUS with $BLOCKED yanked"

heavy_mark pinned-native
RUN_OUT="$HEAVY_WORK/pinned-native.txt"
consumer "$HEAVY_WORK/c2"
cp "$HEAVY_WORK/pinned.lock" "$HEAVY_WORK/c2/Cargo.lock"
if run_cargo "$HEAVY_WORK/home2" "$HEAVY_WORK/c2" fetch --locked; then
  cat "$RUN_OUT" >&2
  heavy_fail "Hide/pinned: cargo fetch --locked of the blocked $BLOCKED succeeded"
fi
heavy_wire_after pinned-native "GET $DOWNLOAD -> " \
  "Hide/pinned: the pinned build never reached the download gate"
NATIVE=$(awk -v n="GET $DOWNLOAD -> " 'index($0, n) == 1 { s = substr($0, length(n) + 1); sub(/ .*/, "", s); print s }' "$HEAVY_LOG" | tail -1)
heavy_log "Refuse/native: the block answers $NATIVE on the download; cargo said:"
heavy_client_said "$RUN_OUT" 'error|failed|status' 5
NATIVE_TRIES=$(grep -c "GET $DOWNLOAD -> " "$HEAVY_LOG")

# ── Refuse: the other status, with Retry-After ───────────────────────────────

case "$NATIVE" in
  403) OTHER=404 ;;
  404) OTHER=403 ;;
  *) heavy_fail "the block answered $NATIVE, neither 403 nor 404" ;;
esac
heavy_tap_rewrite GET "$DOWNLOAD" "$NATIVE" "$OTHER" "Retry-After: 30"
heavy_mark pinned-other
RUN_OUT="$HEAVY_WORK/pinned-other.txt"
consumer "$HEAVY_WORK/c3"
cp "$HEAVY_WORK/pinned.lock" "$HEAVY_WORK/c3/Cargo.lock"
START=$(date +%s)
if run_cargo "$HEAVY_WORK/home3" "$HEAVY_WORK/c3" fetch --locked; then
  heavy_fail "Refuse: cargo fetch succeeded on a $OTHER"
fi
ELAPSED=$(( $(date +%s) - START ))
heavy_wire_after pinned-other "GET $DOWNLOAD -> $NATIVE=>$OTHER"
OTHER_TRIES=$(awk -v mark="### pinned-other" -v n="GET $DOWNLOAD -> " '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, n) == 1 { c++ } END { print c + 0 }' "$HEAVY_LOG")
heavy_log "Refuse/$OTHER: cargo asked $OTHER_TRIES time(s), took ${ELAPSED}s with Retry-After: 30, and said:"
heavy_client_said "$RUN_OUT" 'error|failed|status' 5
[[ "$ELAPSED" -lt 25 ]] || heavy_fail "cargo waited on Retry-After — the CI contract assumes it does not"
heavy_tap_rewrite_clear

# ── Recover ──────────────────────────────────────────────────────────────────

heavy_log "Unblocking $CRATE@$BLOCKED"
heavy_unblock "$REG" "$CRATE" "$BLOCKED"
heavy_mark recover
RUN_OUT="$HEAVY_WORK/recover.txt"
# The same CARGO_HOME and project that were refused: this is the axis.
run_cargo "$HEAVY_WORK/home2" "$HEAVY_WORK/c2" fetch --locked \
  || { cat "$RUN_OUT" >&2; heavy_fail "Recover: cargo fetch after the unblock failed — the client remembered the refusal"; }
heavy_wire_after recover "GET $DOWNLOAD -> 200" "Recover: the download was not re-requested after the unblock"
heavy_log "Recover: the same client fetched $CRATE@$BLOCKED after the unblock"

# ── Publish ──────────────────────────────────────────────────────────────────

# publishable <dir> <version>
publishable() {
  local dir="$1" version="$2"
  mkdir -p "$dir/src" "$dir/.cargo"
  cat >"$dir/Cargo.toml" <<EOF
[package]
name = "heavy-crate"
version = "$version"
edition = "2021"
description = "heavy suite fixture"
license = "MIT"
publish = ["heavy"]
EOF
  echo 'pub fn heavy() {}' >"$dir/src/lib.rs"
  cat >"$dir/.cargo/config.toml" <<EOF
[registries.heavy]
index = "$LOCAL_INDEX"
credential-provider = ["cargo:token"]
EOF
  return $?
}

export CARGO_REGISTRIES_HEAVY_TOKEN="$ADMIN_TOKEN"
PUBLISH="/proxy/$LOCAL/api/v1/crates/new"

heavy_mark publish-native
RUN_OUT="$HEAVY_WORK/publish-native.txt"
publishable "$HEAVY_WORK/p1" 0.1.0
run_cargo "$HEAVY_WORK/homep1" "$HEAVY_WORK/p1" publish --registry heavy --allow-dirty --no-verify \
  || { cat "$RUN_OUT" >&2; heavy_fail "Publish/native: cargo publish failed"; }
heavy_wire_after publish-native "PUT $PUBLISH -> 200"
grep -qi "uploaded\|uploading" "$RUN_OUT" || { cat "$RUN_OUT" >&2; heavy_fail "cargo did not report the upload"; }
heavy_log "Publish/native (200): cargo said:"
heavy_client_said "$RUN_OUT" 'uploaded|waiting|published|warning' 4

heavy_tap_rewrite PUT "$PUBLISH" 200 202
heavy_mark publish-202
RUN_OUT="$HEAVY_WORK/publish-202.txt"
publishable "$HEAVY_WORK/p2" 0.2.0
if run_cargo "$HEAVY_WORK/homep2" "$HEAVY_WORK/p2" publish --registry heavy --allow-dirty --no-verify; then
  PUBLISH_202="accepted"
else
  PUBLISH_202="rejected"
fi
heavy_wire_after publish-202 "PUT $PUBLISH -> 200=>202"
heavy_log "Publish/202: cargo $PUBLISH_202 a 202 Accepted, and said:"
heavy_client_said "$RUN_OUT" 'error|uploaded|waiting|published|warning' 5
heavy_tap_rewrite_clear

heavy_done "cargo heavy test passed: hide=yanked/$PREVIOUS refuse=$NATIVE(x$NATIVE_TRIES)/$OTHER(x$OTHER_TRIES) recover=ok publish-202=$PUBLISH_202"
