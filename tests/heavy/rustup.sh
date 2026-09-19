#!/usr/bin/env bash
# Heavy rustup integration test — a closed world, in which the only host a
# Rust build can reach is this BatleHub, and what it has to do there is
# install a compiler, resolve a dependency, compile and run.
#
# RFC 0024 §6.10 calls this the load-bearing part of the plan, and says why:
# everything the RFC states about rustup's behaviour is read from rustup 1.29's
# source, and read is not observed. RFC 0009 §12 is the record of what skipping
# that step costs — seven ecosystems driven by their real clients found twelve
# bugs every route test had passed.
#
# The world is closed the way `mise.sh` §4 closes it: the client processes run
# with HTTP(S)_PROXY pointed at a closed port and only the loopback exempted, so
# every request that is not addressed to the tap fails at once. That denial is
# itself asserted (§1 below) before anything depends on it — a phase that passes
# because egress was open is the failure mode this suite exists to avoid. The
# *server* keeps its egress: it is the one process allowed to reach upstream,
# which is the whole point of a proxy.
#
# What this proves, on the wire (the tap sits between rustup/cargo and
# BatleHub; it records what the *client* asked for, which is the only evidence
# that counts):
#
#   1. Egress is denied: a direct fetch of static.rust-lang.org from a client
#      process fails, and every phase below therefore ran through the proxy.
#   2. `rustup toolchain install <ver> --profile minimal` reads the manifest's
#      `.sha256`, then the manifest, then exactly the `minimal` components for
#      the host triple — all through the proxy, all off an `upstream` manifest
#      whose checksum rustup itself accepted.
#   3. **The build.** A crate with a dependency, a `Cargo.lock` generated
#      through the proxy's cargo registry, compiled by the compiler installed in
#      §2 and *run* — with the same egress denial in force. The index, the
#      config document and every `.crate` come off the transcript, and the
#      binary's own output is the assertion.
#   4. With the current stable blocked through the admin API, `rustup toolchain
#      install <that version>` exits non-zero on rustup's own *"could not
#      download nonexistent rust version"* and requests **nothing** under that
#      release's dated directory. The block stops the request, not the install.
#   5. With the same block in force, `rustup toolchain install stable` installs
#      the *previous* release: the manifest answered `X-BatleHub-Manifest:
#      repaired`, and `rustc --version` agrees. That install re-fetches every
#      component from a fresh `RUSTUP_HOME`, so the server's cache-hit counter
#      moves and upstream is not asked again.
#   6. On a registry with `deny_components = ["rust-docs"]`, the same install
#      succeeds — rustup verified the checksum of a *filtered* manifest, which
#      is the sentence RFC 0024 exists for — and `rustup component add
#      rust-docs` is refused on rustup's own unavailable-component path.
#   7. `rustup-init` comes through `RUSTUP_UPDATE_ROOT`: the installer's own
#      tree is served from the same instance, so even bootstrapping rustup
#      stays inside the closed world.
#
# `RUSTUP_HOME` and `CARGO_HOME` are redirected into the run's temp directory
# throughout, so this can never touch the runner's own toolchain, and the
# registry names carry `$HEAVY_RUN` so a previous run's block cannot make this
# one green for the wrong reason.
#
# Run via `task test:rustup-heavy` or directly. Needs network *for the server*:
# the upstreams are static.rust-lang.org and crates.io. Environment knobs:
# DATABASE_URL (required), HEAVY_PORT (8152), HEAVY_TAP_PORT (8153), COVERAGE,
# HEAVY_RUSTUP_VERSION and HEAVY_RUSTUP_BLOCKED (by default the two newest
# stable releases `manifests.txt` names, the older one installed and the newer
# one blocked), and HEAVY_RUSTUP_CRATE_REQ (0.14 — the requirement on
# `itertools`, which is the dependency because it has a transitive one of its
# own, so the resolve is a walk and not a single lookup; the program the build
# phase compiles calls into it by name, so this knob moves the version and not
# the crate).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init rustup 8152 8153
heavy_need curl "curl"
heavy_need python3 "python3 (the wire tap)"
heavy_need rustup "the Rust toolchain (rustup itself is the client under test)"
heavy_need rustc "the Rust toolchain"

REG="rust-$HEAVY_RUN"
LEAN="rustlean-$HEAVY_RUN"
CRATES="crates-$HEAVY_RUN"
CRATE="itertools"
CRATE_REQ="${HEAVY_RUSTUP_CRATE_REQ:-0.14}"

# The triple rustup will install for, read from the runner's own rustc rather
# than guessed from `uname`: it is the string that appears in every component
# file name this suite asserts on, and a guess that is close ("x86_64-linux")
# would make every one of those assertions match nothing.
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "$TRIPLE" ]] || heavy_fail "could not read the host triple from 'rustc -vV'"

# The rustup under test, recorded in the transcript: §4 of the RFC is a claim
# about a specific client, and a transcript that does not say which one is not
# evidence for it.
RUSTUP_VERSION="$(rustup --version 2>/dev/null | head -1)"

heavy_start_server tests/heavy/config.rustup.toml
heavy_start_tap

DIST="$HEAVY_TAP_BASE/proxy/$REG/rustup"
LEAN_DIST="$HEAVY_TAP_BASE/proxy/$LEAN/rustup"
INDEX="sparse+$HEAVY_TAP_BASE/proxy/$CRATES/registry/"

heavy_log "client: $RUSTUP_VERSION; host triple $TRIPLE"

# The client-side egress denial, as in mise.sh §4 and airgap.sh: a proxy on a
# closed port, with the loopback exempted so the tap is reachable. Named once
# because all six variables have to name the *same* closed port — a typo in one
# of them leaves that scheme reaching the real internet, and the phase still
# passes.
# `RUSTUP_TOOLCHAIN` is unset along with them, and that is not tidiness: a
# runner whose shell was set up by mise (or by a `rust-toolchain.toml` further
# up the tree) exports it, every `rustc` the build calls is then the rustup
# *shim*, and the shim tries to sync a toolchain this run never installed —
# from the default dist server, because a shim invoked by cargo inherits no
# `RUSTUP_DIST_SERVER`. The build then fails on the egress denial, which reads
# as a broken closed world and is a leaked variable. `RUSTC` and `CARGO` are
# stripped for the same reason: either one would silently replace the compiler
# under test with the runner's own.
CLOSED_PROXY="http://127.0.0.1:1"
LOOPBACK_DIRECT="127.0.0.1,localhost"
DENY=(env -u RUSTUP_TOOLCHAIN -u RUSTC -u CARGO
      HTTP_PROXY="$CLOSED_PROXY" HTTPS_PROXY="$CLOSED_PROXY"
      http_proxy="$CLOSED_PROXY" https_proxy="$CLOSED_PROXY"
      NO_PROXY="$LOOPBACK_DIRECT" no_proxy="$LOOPBACK_DIRECT")

# ── 0. Which releases ────────────────────────────────────────────────────────
#
# Read from the instance's own `manifests.txt` rather than pinned in this file:
# the two claims below are "the newest stable" and "the one before it", and a
# pinned pair stops being that pair six weeks after it is written. Fetching it
# through the tap also exercises the filtered-listing route before anything
# depends on it.

heavy_mark discovery
curl -fsS "$DIST/manifests.txt" -o "$HEAVY_WORK/manifests.txt" \
  || heavy_fail "manifests.txt is not servable through the proxy"
heavy_wire_after discovery "GET /proxy/$REG/rustup/manifests.txt -> 200" \
  "manifests.txt did not come through the proxy"

# `static.rust-lang.org/dist/{date}/channel-rust-{name}.toml`, one per line.
# Only the three-part stable names, in file order — which is publication order.
mapfile -t STABLE_ROWS < <(awk -F/ '
  $NF ~ /^channel-rust-[0-9]+\.[0-9]+\.[0-9]+\.toml$/ {
    v = $NF; sub(/^channel-rust-/, "", v); sub(/\.toml$/, "", v)
    print v, $(NF - 1)
  }' "$HEAVY_WORK/manifests.txt" | tail -2)
[[ "${#STABLE_ROWS[@]}" == 2 ]] \
  || heavy_fail "manifests.txt named fewer than two stable releases — the listing filter ate the wrong rows"

VERSION="${HEAVY_RUSTUP_VERSION:-$(echo "${STABLE_ROWS[0]}" | cut -d' ' -f1)}"
VERSION_DATE="$(echo "${STABLE_ROWS[0]}" | cut -d' ' -f2)"
BLOCKED="${HEAVY_RUSTUP_BLOCKED:-$(echo "${STABLE_ROWS[1]}" | cut -d' ' -f1)}"
BLOCKED_DATE="$(echo "${STABLE_ROWS[1]}" | cut -d' ' -f2)"
[[ "$VERSION" != "$BLOCKED" ]] \
  || heavy_fail "HEAVY_RUSTUP_VERSION and HEAVY_RUSTUP_BLOCKED must differ"
heavy_log "installing $VERSION ($VERSION_DATE); blocking $BLOCKED ($BLOCKED_DATE), which is what 'stable' names"

# ── 1. The world is closed ───────────────────────────────────────────────────
#
# Asserted first and on its own, because every phase after this one is only
# evidence if it holds. A client that can still reach static.rust-lang.org
# would install a toolchain, compile and run with the proxy barely involved,
# and the suite would be green for the wrong reason.

heavy_log "Egress control: a client process reaching upstream directly"
if "${DENY[@]}" curl -sS --max-time 20 -o /dev/null "https://static.rust-lang.org/manifests.txt" 2>"$HEAVY_WORK/egress.txt"; then
  heavy_fail "a client process reached static.rust-lang.org — the world is not closed, and every phase below would pass for the wrong reason"
fi
heavy_client_said "$HEAVY_WORK/egress.txt" 'proxy|refused|connect|error' 2
if "${DENY[@]}" curl -sS --max-time 20 -o /dev/null "https://index.crates.io/config.json" 2>"$HEAVY_WORK/egress-crates.txt"; then
  heavy_fail "a client process reached index.crates.io — the build phase would resolve past the proxy"
fi
heavy_log "RUSTUP-CLOSED-WORLD-OK (upstream unreachable from the client side)"

# run_rustup <rustup-home> <args...> — rustup with its own home and cargo home,
# pointed at the proxy, with egress denied. Output lands in the file named by
# RUN_OUT; the exit code is returned.
RUN_OUT="$HEAVY_WORK/run.txt"
run_rustup() {
  local home="$1" dist="${DIST_OVERRIDE:-$DIST}"
  shift
  mkdir -p "$home" "$home-cargo"
  "${DENY[@]}" RUSTUP_HOME="$home" CARGO_HOME="$home-cargo" \
    RUSTUP_DIST_SERVER="$dist" RUSTUP_UPDATE_ROOT="$dist/rustup" \
    rustup "$@" >"$RUN_OUT" 2>&1
  return $?
}

# The toolchain directory `rustup` installs <version> into, under <home>.
toolchain_dir() { echo "$1/toolchains/$2-$TRIPLE"; return $?; }

HOME_MAIN="$HEAVY_WORK/rustup-main"

# ── 2. Install a compiler, through the proxy and nothing else ────────────────

heavy_mark install
heavy_log "rustup toolchain install $VERSION --profile minimal"
RUN_OUT="$HEAVY_WORK/install.txt"
run_rustup "$HOME_MAIN" toolchain install "$VERSION" --profile minimal --no-self-update \
  || { cat "$RUN_OUT" >&2; heavy_fail "rustup toolchain install $VERSION failed through the proxy"; }

heavy_wire_after install "GET /proxy/$REG/rustup/dist/channel-rust-$VERSION.toml.sha256 -> 200" \
  "rustup did not read the manifest's checksum sidecar — the sidecar rule is untested"
heavy_wire_after install "GET /proxy/$REG/rustup/dist/channel-rust-$VERSION.toml -> 200" \
  "rustup did not read the channel manifest through the proxy"
heavy_wire_re_after install "GET /proxy/$REG/rustup/dist/channel-rust-$VERSION[.]toml -> 200 .*X-BatleHub-Manifest: upstream" \
  "the manifest was not served as 'upstream' — nothing is blocked or denied on this registry yet"
for component in rustc cargo rust-std; do
  heavy_wire_after install \
    "GET /proxy/$REG/rustup/dist/$VERSION_DATE/$component-$VERSION-$TRIPLE.tar.xz -> 200" \
    "the $component tarball of $VERSION was not fetched through the proxy"
done
# `minimal` is three components and no more: a profile that quietly pulled the
# documentation would still install, and the deny phase below would then be
# measuring a component nobody asked for.
heavy_wire_not "GET /proxy/$REG/rustup/dist/$VERSION_DATE/rust-docs-$VERSION-$TRIPLE.tar.xz" \
  "rustup fetched rust-docs for a minimal profile"

TC="$(toolchain_dir "$HOME_MAIN" "$VERSION")"
[[ -x "$TC/bin/rustc" && -x "$TC/bin/cargo" ]] \
  || heavy_fail "no rustc/cargo under $TC after the install"
INSTALLED="$("$TC/bin/rustc" --version 2>&1 | head -1)"
[[ "$INSTALLED" == *"$VERSION"* ]] \
  || heavy_fail "the installed rustc answered '$INSTALLED', expected $VERSION"
heavy_log "RUSTUP-INSTALL-OK ($INSTALLED, from $DIST)"

# ── 3. Compile and run, with the same world closed ───────────────────────────
#
# The point of the suite. The compiler came from the proxy in §2; the crate it
# compiles comes from the proxy here, resolved through a sparse index that
# replaces crates.io, and the binary is executed. Every one of those steps runs
# under the same egress denial, so a single reachable upstream anywhere in the
# chain fails the phase rather than rescuing it.

PROJECT="$HEAVY_WORK/build"
mkdir -p "$PROJECT/src" "$PROJECT/.cargo"
cat >"$PROJECT/Cargo.toml" <<EOF
[package]
name = "heavy-closed-world"
version = "0.1.0"
edition = "2021"

[dependencies]
$CRATE = "$CRATE_REQ"
EOF
# Uses the dependency at runtime, not only at compile time: a program that
# merely links against a crate proves the download, and this one proves the
# code in it ran.
cat >"$PROJECT/src/main.rs" <<'EOF'
use itertools::Itertools;

fn main() {
    let joined = (1..=4).map(|n| n * n).join("-");
    println!("CLOSED-WORLD-RAN {joined}");
}
EOF
cat >"$PROJECT/.cargo/config.toml" <<EOF
[source.crates-io]
replace-with = "batlehub"

[source.batlehub]
registry = "$INDEX"
EOF

# Its own CARGO_HOME: the registry cache and the index cache live there, so
# this starts from a client that has never seen crates.io — the alternative is
# a build that "succeeds" off the runner's warm cache with the proxy untouched.
BUILD_CARGO_HOME="$HEAVY_WORK/build-cargo-home"
mkdir -p "$BUILD_CARGO_HOME"

# run_cargo <args...> — the cargo *installed in §2*, by absolute path and with
# that toolchain's `bin` first on PATH, so the `rustc` cargo goes on to call is
# the one that came from the proxy and not the runner's shim. That is the whole
# claim of this phase: a compiler this instance served, compiling.
run_cargo() {
  (cd "$PROJECT" && "${DENY[@]}" CARGO_HOME="$BUILD_CARGO_HOME" \
    CARGO_TERM_COLOR=never CARGO_NET_RETRY=1 RUSTUP_HOME="$HOME_MAIN" \
    RUSTUP_DIST_SERVER="$DIST" PATH="$TC/bin:$PATH" \
    "$TC/bin/cargo" "$@") >"$RUN_OUT" 2>&1
  return $?
}

heavy_mark resolve
heavy_log "cargo generate-lockfile through $INDEX"
RUN_OUT="$HEAVY_WORK/resolve.txt"
run_cargo generate-lockfile \
  || { cat "$RUN_OUT" >&2; heavy_fail "cargo could not resolve $CRATE through the proxy with egress denied"; }
grep -q "name = \"$CRATE\"" "$PROJECT/Cargo.lock" \
  || heavy_fail "the lockfile does not name $CRATE"
heavy_wire_after resolve "GET /proxy/$CRATES/registry/config.json -> 200" \
  "cargo did not read the sparse index's config document through the proxy"
# `it/er/itertools`, the sparse index's own layout for a name of four or more
# characters. Asserted by regex because the prefix is derived, not chosen.
heavy_wire_re_after resolve \
  "GET /proxy/$CRATES/registry/.*/$CRATE -> 200" \
  "cargo did not read $CRATE's index entry through the proxy"

heavy_mark build
heavy_log "cargo build with the toolchain installed in §2"
RUN_OUT="$HEAVY_WORK/build.txt"
run_cargo build --locked \
  || { cat "$RUN_OUT" >&2; heavy_fail "the build failed inside the closed world"; }
heavy_wire_re_after build "GET /proxy/$CRATES/$CRATE/[0-9][^ ]*/download -> 200" \
  "the $CRATE .crate file was not downloaded through the proxy"
DEPS_DOWNLOADED="$(heavy_wire_count_after build "GET /proxy/$CRATES/[^ ]*/download -> 200")"
[[ "$DEPS_DOWNLOADED" -ge 2 ]] \
  || heavy_fail "only $DEPS_DOWNLOADED crate file(s) came through the proxy — the transitive dependency did not, so the resolve was a single lookup and not a walk"

heavy_mark run
heavy_log "running the binary the closed world produced"
BIN="$PROJECT/target/debug/heavy-closed-world"
[[ -x "$BIN" ]] || heavy_fail "no binary at $BIN after a successful build"
OUTPUT="$("${DENY[@]}" "$BIN" 2>&1 | tail -1)"
[[ "$OUTPUT" == "CLOSED-WORLD-RAN 1-4-9-16" ]] \
  || heavy_fail "the binary printed '$OUTPUT', expected 'CLOSED-WORLD-RAN 1-4-9-16' — the dependency's code did not run"
heavy_log "RUSTUP-BUILD-OK ($OUTPUT, $DEPS_DOWNLOADED crate files through the proxy)"

# ── 4. A blocked release is not downloadable, and not askable ────────────────

heavy_log "Blocking rust $BLOCKED through the admin API"
heavy_block "$REG" rust "$BLOCKED"

heavy_mark refusal
heavy_log "rustup toolchain install $BLOCKED (blocked)"
RUN_OUT="$HEAVY_WORK/refusal.txt"
HOME_BLOCKED="$HEAVY_WORK/rustup-blocked"
if run_rustup "$HOME_BLOCKED" toolchain install "$BLOCKED" --profile minimal --no-self-update; then
  cat "$RUN_OUT" >&2
  heavy_fail "rustup installed $BLOCKED after it was blocked"
fi
grep -qi "nonexistent rust version\|not available\|no release found" "$RUN_OUT" || {
  cat "$RUN_OUT" >&2
  heavy_fail "rustup failed on $BLOCKED, but not on its own missing-release path (RFC 0024 §4.4)"
}
heavy_log "Refuse: rustup said:"
heavy_client_said "$RUN_OUT" 'error|nonexistent|warn' 4
heavy_wire_after refusal "GET /proxy/$REG/rustup/dist/channel-rust-$BLOCKED.toml.sha256 -> 404" \
  "the blocked release's checksum sidecar did not answer 404 — rustup reaches its missing-release path through that document"
# The second door, and the one rustup names in the error it prints: with the v2
# manifest gone it tries the v1 installer package, `dist/rust-{ver}-{triple}.tar.gz`,
# and only gives up when that 404s too. Measured here rather than assumed,
# because a registry that refused the manifest and served the v1 package would
# still install the release this block forbids.
heavy_wire_after refusal "GET /proxy/$REG/rustup/dist/rust-$BLOCKED-$TRIPLE.tar.gz.sha256 -> 404" \
  "rustup's v1 fallback for the blocked release did not answer 404"
# The row is the block: not one byte of the release may be requested.
heavy_wire_not "GET /proxy/$REG/rustup/dist/$BLOCKED_DATE/" \
  "rustup requested a component of the blocked release — the block stopped the install, not the request"
[[ -d "$(toolchain_dir "$HOME_BLOCKED" "$BLOCKED")" ]] \
  && heavy_fail "a blocked release was installed into $HOME_BLOCKED"
heavy_log "RUSTUP-REFUSAL-OK (rustup's own missing-release path, nothing requested under $BLOCKED_DATE)"

# ── 5. The alias moves, and the second install is served from the cache ──────
#
# `stable` names the blocked release, so the manifest for it is repaired to the
# newest release the alias could still denote. The install that follows fetches
# every component again from a fresh RUSTUP_HOME, which is what makes the
# cache-hit counter the right instrument here: the tap cannot see the proxy's
# upstream side, so "upstream was not asked" is read from the server's own
# counter, while the transcript proves the client did ask.

hits_for() {
  local reg="$1"
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk -v reg="$reg" '$1 ~ /^batlehub_artifact_cache_hits_total\{/ && index($1, "registry=\"" reg "\"") { print $2 }'
  return $?
}
HITS_BEFORE="$(hits_for "$REG")"; HITS_BEFORE="${HITS_BEFORE:-0}"

heavy_mark repair
heavy_log "rustup toolchain install stable, with $BLOCKED (the current stable) blocked"
RUN_OUT="$HEAVY_WORK/repair.txt"
HOME_STABLE="$HEAVY_WORK/rustup-stable"
run_rustup "$HOME_STABLE" toolchain install stable --profile minimal --no-self-update \
  || { cat "$RUN_OUT" >&2; heavy_fail "rustup toolchain install stable failed while the current stable was blocked — the alias was not repaired"; }
heavy_wire_re_after repair \
  "GET /proxy/$REG/rustup/dist/channel-rust-stable[.]toml -> 200 .*X-BatleHub-Manifest: repaired" \
  "the 'stable' manifest was not marked repaired"
heavy_wire_re_after repair \
  "GET /proxy/$REG/rustup/dist/channel-rust-stable[.]toml -> 200 .*X-BatleHub-Version: $VERSION" \
  "the repaired manifest did not name $VERSION as the release it served"
REPAIRED_RUSTC="$("$(toolchain_dir "$HOME_STABLE" stable)/bin/rustc" --version 2>&1 | head -1)"
[[ "$REPAIRED_RUSTC" == *"$VERSION"* ]] \
  || heavy_fail "'stable' installed '$REPAIRED_RUSTC', expected the repaired $VERSION"
heavy_wire_after repair "GET /proxy/$REG/rustup/dist/$VERSION_DATE/rustc-$VERSION-$TRIPLE.tar.xz -> 200" \
  "the repaired install did not ask the proxy for a component, so nothing about the cache was exercised"

HITS_AFTER="$(hits_for "$REG")"; HITS_AFTER="${HITS_AFTER:-0}"
[[ "${HITS_AFTER%.*}" -gt "${HITS_BEFORE%.*}" ]] \
  || heavy_fail "batlehub_artifact_cache_hits_total for $REG did not move ($HITS_BEFORE -> $HITS_AFTER): the second install went upstream"
heavy_log "RUSTUP-REPAIR-OK (stable -> $REPAIRED_RUSTC, cache hits $HITS_BEFORE -> $HITS_AFTER)"

# ── 6. A denied component, and the checksum of the document that says so ─────
#
# The lean registry serves the same tree with `rust-docs` edited out of every
# manifest. The install below therefore verifies a *filtered* manifest against a
# sidecar this instance computed — if that were wrong, rustup would stop on
# "checksum failed" and install nothing, which is the failure this phase is
# shaped to catch.

heavy_mark deny
heavy_log "rustup toolchain install $VERSION against the registry that denies rust-docs"
RUN_OUT="$HEAVY_WORK/deny-install.txt"
HOME_LEAN="$HEAVY_WORK/rustup-lean"
DIST_OVERRIDE="$LEAN_DIST" run_rustup "$HOME_LEAN" toolchain install "$VERSION" --profile minimal --no-self-update \
  || { cat "$RUN_OUT" >&2; heavy_fail "the install off a filtered manifest failed — rustup rejected a document this instance edited and re-hashed"; }
heavy_wire_re_after deny \
  "GET /proxy/$LEAN/rustup/dist/channel-rust-$VERSION[.]toml -> 200 .*X-BatleHub-Manifest: filtered" \
  "the lean registry did not report its manifest as filtered"
heavy_wire_after deny "GET /proxy/$LEAN/rustup/dist/channel-rust-$VERSION.toml.sha256 -> 200" \
  "rustup did not read the sidecar of the filtered manifest"
grep -qi "checksum" "$RUN_OUT" \
  && { cat "$RUN_OUT" >&2; heavy_fail "rustup mentioned a checksum — the sidecar of the filtered manifest did not match its body"; }

heavy_mark deny-add
heavy_log "rustup component add rust-docs, on the registry that denies it"
RUN_OUT="$HEAVY_WORK/deny-add.txt"
if DIST_OVERRIDE="$LEAN_DIST" run_rustup "$HOME_LEAN" component add rust-docs --toolchain "$VERSION"; then
  cat "$RUN_OUT" >&2
  heavy_fail "rustup added rust-docs from a registry that denies it"
fi
# rustup 1.29's wording, measured rather than guessed: because the filter takes
# the component *out of the target's component list* (and not merely marks it
# `available = false`), rustup answers from the manifest it already holds —
#
#   error: toolchain '1.98.0-x86_64-unknown-linux-gnu' does not contain
#   component 'rust-docs' for target 'x86_64-unknown-linux-gnu'
#   help: did you mean 'rustc-docs'?
#
# — which is its not-in-this-toolchain path, with its own suggestion, and not
# the "unavailable for download" an `available = false` alone would produce.
grep -qi "does not contain component\|unavailable\|not available\|no such component" "$RUN_OUT" || {
  cat "$RUN_OUT" >&2
  heavy_fail "the denied component was refused, but not on a rustup path that names the component"
}
heavy_log "Deny: rustup said:"
heavy_client_said "$RUN_OUT" 'error|unavailable|component' 4
# Nothing at all is requested for this phase: the component is absent from the
# manifest rustup already read, so the refusal costs no round trip.
heavy_wire_not "GET /proxy/$LEAN/rustup/dist/$VERSION_DATE/rust-docs-$VERSION-$TRIPLE.tar.xz" \
  "rustup requested the denied component's tarball — the deny edits the manifest, so the request should never be made"
heavy_log "RUSTUP-DENY-OK (filtered manifest installed, rust-docs refused by rustup)"

# ── 7. The installer's own tree ──────────────────────────────────────────────
#
# `RUSTUP_UPDATE_ROOT` is the second and last client switch: with it pointed at
# this instance, even a machine that has no rustup yet gets one without leaving
# the closed world. The version is resolved server-side through
# `release-stable.toml`, so the client asks for one path and the transcript
# shows which archive answered.

heavy_mark bootstrap
heavy_log "fetching rustup-init through $DIST/rustup"
# The file name is load-bearing: `rustup-init` is a multi-call binary that
# dispatches on its own argv[0], and saved under any other name it answers
# "unknown proxy name" instead of running. Saving it as anything else would
# make this phase fail for a reason that has nothing to do with the proxy.
INIT="$HEAVY_WORK/rustup-init"
"${DENY[@]}" curl -fsS "$DIST/rustup/dist/$TRIPLE/rustup-init" -o "$INIT" \
  || heavy_fail "rustup-init did not come through the proxy"
chmod +x "$INIT"
heavy_wire_after bootstrap "GET /proxy/$REG/rustup/rustup/dist/$TRIPLE/rustup-init -> 200" \
  "the installer was not served through the proxy's rustup tree"
INIT_VERSION="$("${DENY[@]}" RUSTUP_HOME="$HEAVY_WORK/rustup-init-home" \
  CARGO_HOME="$HEAVY_WORK/rustup-init-cargo" "$INIT" --version 2>&1 | head -1)"
[[ "$INIT_VERSION" == rustup* ]] \
  || heavy_fail "the proxied rustup-init answered '$INIT_VERSION' — what came through is not the installer"
heavy_log "RUSTUP-BOOTSTRAP-OK ($INIT_VERSION)"

heavy_done "RUSTUP-HEAVY-OK: closed world, $INSTALLED installed and $OUTPUT; blocked=$BLOCKED repaired=stable->$VERSION denied=rust-docs init=$INIT_VERSION"
