#!/usr/bin/env bash
# Heavy Ansible Galaxy integration test — the real `ansible-galaxy`, against
# galaxy.ansible.com through the proxy (RFC 0031 §6.10).
#
# Why a heavy suite, when 26 in-process tests already cover the routes: every
# registry defect this project has shipped was found by a client and not by a
# test double — the ovsx download URL, the GitLab release document, the
# JetBrains numeric-id spelling. A route test proves routing. Only the real
# client proves that `ansible-galaxy` accepts what this server composes.
#
# Four facts under test, each of which a test double cannot check:
#
#   1. **The resolver reads the versions list and nothing else.** A blocked
#      version has to be absent from that document *and* the install has to
#      succeed at the version below — asserted on the wire, not on the exit
#      code: a phase that passes because the client reached the upstream proves
#      nothing.
#   2. **`download_url` is followed to this instance.** The tap sees the
#      tarball request arrive here; upstream's signed pulp URL never appears.
#   3. **The tarball is byte-exact.** `_download_file` hashes the body and
#      compares it with `artifact.sha256` from the version document; a single
#      rewritten byte is *"Mismatch artifact hash with downloaded file"*.
#   4. **The publish is synchronous.** `publish_collection` posts and then
#      polls the import task; here it is already `completed` on the first poll,
#      so the install straight afterwards cannot race it.
#
# Run via `task test:galaxy-heavy` or directly, optionally with one phase name:
#
#     bash tests/heavy/galaxy.sh            # every phase
#     bash tests/heavy/galaxy.sh publish    # just that one
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8160),
# HEAVY_TAP_PORT (8161), COVERAGE, ANSIBLE_CORE_VERSION (2.19.3).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init galaxy 8160 8161
heavy_need python3 "python3"
heavy_need curl "curl"
heavy_need uv "uv (https://docs.astral.sh/uv/)"

# Pinned, and quoted in the conformance fixture beside the request lines it
# was read from: "the client sends X" is a claim about a version.
ANSIBLE_CORE_VERSION="${ANSIBLE_CORE_VERSION:-2.19.3}"

REG="galaxy-$HEAVY_RUN"
REG_INDEX="galaxy-index-$HEAVY_RUN"
REG_LOCAL="galaxy-local-$HEAVY_RUN"

# A collection with a long version history and no heavy dependency tree.
COLLECTION="community.docker"
COLL_NS="community"
COLL_NAME="docker"
ROLE="geerlingguy.docker"

# ── ansible-core in a run-local virtualenv ───────────────────────────────────

VENV="$HEAVY_WORK/venv"
uv venv --quiet "$VENV" || heavy_fail "creating the virtualenv failed"
uv pip install --quiet --python "$VENV/bin/python" "ansible-core==$ANSIBLE_CORE_VERSION" \
  || heavy_fail "installing ansible-core==$ANSIBLE_CORE_VERSION failed"
GALAXY="$VENV/bin/ansible-galaxy"
[[ -x "$GALAXY" ]] || heavy_fail "ansible-galaxy is not in the virtualenv"

heavy_start_server tests/heavy/config.galaxy.toml
heavy_start_tap

heavy_log "$("$GALAXY" --version | head -1)"

# ── ansible.cfg, and the caches this suite must not share ────────────────────
#
# `ansible-galaxy` caches listing responses for 24 hours under
# ANSIBLE_GALAXY_CACHE_DIR and installs into ANSIBLE_HOME. Both are redirected
# into the run's own directory, and every install step gets a *fresh*
# ANSIBLE_HOME: the first version of this script reused one and measured the
# client's cache rather than the server's.

# **The token goes in the config, not on the command line.** `--api-key`
# attaches nothing when the server comes from `server_list`: captured against
# ansible-core 2.19.3, the publish request carried no `Authorization` header at
# all. A `token =` line in the `[galaxy_server.*]` block is what the client
# actually sends — as `Authorization: Token <token>`, which is
# `GalaxyToken.token_type` and not the `Bearer` RFC 0031 §4.2 recorded.
write_cfg() {  # <registry> <cfg path> [token]
  local registry="$1" cfg="$2" token="${3:-}"
  {
    printf '[galaxy]\nserver_list = batlehub\n\n'
    printf '[galaxy_server.batlehub]\nurl = %s/proxy/%s/galaxy/api/\n' \
      "$HEAVY_TAP_BASE" "$registry"
    [[ -z "$token" ]] || printf 'token = %s\n' "$token"
  } > "$cfg"
}

CFG="$HEAVY_WORK/ansible.cfg"
CFG_INDEX="$HEAVY_WORK/ansible-index.cfg"
CFG_LOCAL="$HEAVY_WORK/ansible-local.cfg"
write_cfg "$REG" "$CFG"
write_cfg "$REG_INDEX" "$CFG_INDEX"
# The local registry is the one that is published to, so it is the one that
# needs a credential.
write_cfg "$REG_LOCAL" "$CFG_LOCAL" "$ADMIN_TOKEN"

galaxy_run() {  # <cfg> <fresh-home-label> <args…>
  local cfg="$1" label="$2"; shift 2
  local home="$HEAVY_WORK/home-$label"
  rm -rf "$home"
  mkdir -p "$home"
  ANSIBLE_CONFIG="$cfg" \
  ANSIBLE_HOME="$home" \
  ANSIBLE_GALAXY_CACHE_DIR="$home/cache" \
  ANSIBLE_LOCAL_TEMP="$home/tmp" \
    "$GALAXY" "$@" > "$HEAVY_WORK/$label.out" 2>&1
  local rc=$?
  cat "$HEAVY_WORK/$label.out"
  return $rc
}

# said <file> <ere> <explanation> — the client's own output must match.
#
# **Not `heavy_client_said`**, which only *reports*: it prints matching lines or
# a tail and always returns 0, and its third argument is a line count. Using it
# as an assertion is a phase that passes whatever the client printed — the
# "green for the wrong reason" failure this suite exists to avoid.
said() {
  local file="$1" ere="$2" explanation="$3"
  grep -qiE -- "$ere" "$file" 2>/dev/null && return 0
  echo "--- the client's output ---" >&2
  tail -n 40 "$file" >&2
  heavy_fail "$explanation (nothing matching /$ere/ in $(basename "$file"))"
}

# The versions the proxy currently serves, newest first.
#
# Sorted here rather than trusting the document's order: nothing in the
# protocol fixes it, and a phase that read `data[0]` and got the *oldest*
# version would block something the resolver was never going to choose and then
# report that nothing changed — a pass for the wrong reason.
versions_json() {
  curl -fsS "$HEAVY_TAP_BASE/proxy/$REG/galaxy/api/v3/collections/$COLL_NS/$COLL_NAME/versions/"
}

# newest_versions <n> — the n newest *stable* versions, one per line.
newest_versions() {
  versions_json | python3 -c '
import json, sys

def key(v):
    core = v.split("-")[0]
    return tuple(int(p) for p in core.split(".") if p.isdigit())

versions = [e["version"] for e in json.load(sys.stdin)["data"]]
versions = [v for v in versions if "-" not in v] or versions
for v in sorted(versions, key=key, reverse=True)[: int(sys.argv[1])]:
    print(v)
' "$1"
}

# ── phase: install ───────────────────────────────────────────────────────────

phase_install() {
  heavy_log "1. A plain install resolves and downloads through the proxy"
  heavy_mark install

  galaxy_run "$CFG" install collection install "$COLLECTION" \
    || heavy_fail "ansible-galaxy collection install $COLLECTION failed"
  said "$HEAVY_WORK/install.out" "was installed successfully" \
    "the client did not report a successful install"

  # The four documents of one install, each on the wire and each here.
  heavy_wire_re_after install "GET /proxy/$REG/galaxy/api/ " \
    "g_connect's discovery read did not reach the proxy"
  heavy_wire_re_after install \
    "GET /proxy/$REG/galaxy/api/v3/collections/$COLL_NS/$COLL_NAME/ " \
    "the collection document was not read through the proxy"
  heavy_wire_re_after install \
    "GET /proxy/$REG/galaxy/api/v3/collections/$COLL_NS/$COLL_NAME/versions/" \
    "the versions list — the chokepoint — was not read through the proxy"
  heavy_wire_re_after install \
    "GET /proxy/$REG/galaxy/api/v3/artifacts/collections/$COLL_NS-$COLL_NAME-.*[.]tar[.]gz" \
    "the tarball was not fetched from this instance: download_url was not rewritten"

  # **The negative is not asserted here, and saying so is the point.** "The
  # client never talked to the upstream itself" is the claim that matters — an
  # unrewritten `download_url` would make the install succeed anyway — and this
  # suite cannot make it: the tap sits in front of *this server*, so a request
  # the client sent straight to galaxy.ansible.com never appears in the
  # transcript. A `heavy_wire_not "galaxy.ansible.com"` here would pass
  # unconditionally, which is worse than no assertion.
  #
  # What proves it is `closed_world.sh`'s `ansible` phase, where every client
  # process runs with egress denied by a proxy that answers `403`: an install
  # that reached upstream cannot complete there. The positive assertion above
  # — the tarball arrived *here* — is what this suite can honestly claim.

  # The byte-exactness assertion is the *first* one, and it is worth saying why:
  # `_download_file` hashes the body as it streams and compares it with
  # `artifact.sha256`, so a single rewritten byte is "Mismatch artifact hash
  # with downloaded file" and the install above would not have succeeded.
  said "$HEAVY_WORK/install.out" "was installed successfully" \
    "the install did not succeed, so the tarball's digest is unproven"
}

# ── phase: blocked ───────────────────────────────────────────────────────────

phase_blocked() {
  heavy_log "2. A blocked version is gone from the list, and a range steps down"

  local newest below two
  two="$(newest_versions 2)"
  newest="$(echo "$two" | sed -n 1p)"
  below="$(echo "$two" | sed -n 2p)"
  [[ -n "$newest" && -n "$below" ]] \
    || heavy_fail "the listing named fewer than two versions of $COLLECTION"
  heavy_log "newest=$newest, the one below=$below"

  heavy_block "$REG" "$COLLECTION" "$newest"

  heavy_mark blocked
  galaxy_run "$CFG" blocked collection install "$COLLECTION" \
    || heavy_fail "the install should have resolved to $below, not failed"
  said "$HEAVY_WORK/blocked.out" "$below" \
    "the resolver did not step down to $below"

  # The whole point: nothing under the blocked version's paths was requested.
  #
  # The version is interpolated into an ERE, so its dots are escaped as `[.]`
  # — the one spelling that survives awk's own `-v` processing (see
  # `heavy_wire_re_after` in lib.sh). Unescaped, `4.8.0` matches `4x8y0` and the
  # count could be a false positive.
  local newest_re hits
  newest_re="${newest//./[.]}"
  hits="$(heavy_wire_count_after blocked "versions/$newest_re/")"
  [[ "$hits" == "0" ]] \
    || heavy_fail "the resolver asked for the blocked version's document $hits times"
  hits="$(heavy_wire_count_after blocked "$COLL_NS-$COLL_NAME-$newest_re[.]tar[.]gz")"
  [[ "$hits" == "0" ]] \
    || heavy_fail "the resolver asked for the blocked version's tarball $hits times"

  # The listing document itself no longer names it.
  versions_json | grep -qF "\"$newest\"" \
    && heavy_fail "the blocked version is still in the served versions list"

  heavy_unblock "$REG" "$COLLECTION" "$newest"
  return 0
}

# ── phase: stale ─────────────────────────────────────────────────────────────

phase_stale() {
  heavy_log "3. A block set after a warm install invalidates the client's cached list"

  # One ANSIBLE_HOME across both runs, on purpose: this phase is about the
  # client's *own* 24-hour response cache, which every other phase avoids.
  local home="$HEAVY_WORK/home-stale"
  rm -rf "$home"; mkdir -p "$home"
  run_warm() {
    ANSIBLE_CONFIG="$CFG" ANSIBLE_HOME="$home" \
    ANSIBLE_GALAXY_CACHE_DIR="$home/cache" ANSIBLE_LOCAL_TEMP="$home/tmp" \
      "$GALAXY" "$@" > "$HEAVY_WORK/stale.out" 2>&1
  }

  run_warm collection install "$COLLECTION" || heavy_fail "the warming install failed"
  local newest
  newest="$(newest_versions 1)"

  heavy_block "$REG" "$COLLECTION" "$newest"
  heavy_mark stale
  # A second install from the same ANSIBLE_HOME. Without the `updated_at` bump
  # the client would serve its cached list, pick the blocked version, and fail
  # on a 404 at the version document; with it, it re-reads and resolves down.
  run_warm collection install "$COLLECTION" --force \
    || heavy_fail "the second install failed: the client served its stale listing"
  heavy_wire_re_after stale \
    "GET /proxy/$REG/galaxy/api/v3/collections/$COLL_NS/$COLL_NAME/versions/" \
    "the client did not re-read the versions list, so updated_at did not move"

  heavy_unblock "$REG" "$COLLECTION" "$newest"
  return 0
}

# ── phase: pinned ────────────────────────────────────────────────────────────

phase_pinned() {
  heavy_log "4. An exact pin on a blocked version stops before any download"

  local newest
  newest="$(newest_versions 1)"
  heavy_block "$REG" "$COLLECTION" "$newest"

  heavy_mark pinned
  if galaxy_run "$CFG" pinned collection install "$COLLECTION:==$newest"; then
    heavy_unblock "$REG" "$COLLECTION" "$newest"
    heavy_fail "installing a blocked version by exact pin should have failed"
  fi
  said "$HEAVY_WORK/pinned.out" "Failed to resolve the requested dependencies map" \
    "the client did not stop on its own unsatisfiable-requirements error"

  local hits newest_re
  newest_re="${newest//./[.]}"
  hits="$(heavy_wire_count_after pinned "$COLL_NS-$COLL_NAME-$newest_re[.]tar[.]gz")"
  [[ "$hits" == "0" ]] || heavy_fail "a pinned blocked version still issued $hits artifact requests"

  heavy_unblock "$REG" "$COLLECTION" "$newest"
  return 0
}

# ── phase: publish ───────────────────────────────────────────────────────────

# build_collection <ns> <name> <version> [dependency…] — `collection build`, with
# the tarball it produced left in `BUILT_TARBALL`.
#
# A global rather than stdout: `heavy_fail` exits, and inside `$(…)` it would
# exit the *subshell* — the caller would carry on with an empty path and fail
# somewhere else, which is the "green for the wrong reason" shape one step
# removed.
#
# Each `dependency` is a `ns.name: range` line for galaxy.yml's `dependencies`
# map, which is where the resolver's edges come from.
BUILT_TARBALL=""
build_collection() {
  local ns="$1" name="$2" version="$3"; shift 3
  local src="$HEAVY_WORK/collection/$ns/$name"
  mkdir -p "$src/plugins/modules"
  {
    cat <<YML
namespace: $ns
name: $name
version: $version
readme: README.md
authors:
  - heavy suite
description: a two-file collection built by tests/heavy/galaxy.sh
license:
  - MIT
YML
    if [[ $# -gt 0 ]]; then
      printf 'dependencies:\n'
      printf '  %s\n' "$@"
    fi
  } > "$src/galaxy.yml"
  echo "# $ns.$name" > "$src/README.md"
  echo "# a plugin, so the build has something to put in FILES.json" \
    > "$src/plugins/modules/noop.py"

  ( cd "$src" && ANSIBLE_CONFIG="$CFG_LOCAL" "$GALAXY" collection build --force \
      --output-path "$HEAVY_WORK" ) > "$HEAVY_WORK/build-$name.out" 2>&1 \
    || { cat "$HEAVY_WORK/build-$name.out"; heavy_fail "collection build failed for $ns.$name"; }
  BUILT_TARBALL="$HEAVY_WORK/$ns-$name-$version.tar.gz"
  [[ -f "$BUILT_TARBALL" ]] || heavy_fail "collection build did not produce $BUILT_TARBALL"
}

# publish_collection <label> <tarball> — publish it into the local registry.
publish_collection() {
  local label="$1" tarball="$2"
  ANSIBLE_CONFIG="$CFG_LOCAL" ANSIBLE_HOME="$HEAVY_WORK/home-$label" \
    "$GALAXY" collection publish "$tarball" --server batlehub \
    > "$HEAVY_WORK/$label.out" 2>&1 \
    || { cat "$HEAVY_WORK/$label.out"; heavy_fail "collection publish failed ($label)"; }
}

phase_publish() {
  heavy_log "5. collection publish, its import task, and an install of what was published"

  local ns="acme$HEAVY_RUN" name="util" version="1.0.0"
  build_collection "$ns" "$name" "$version"
  local tarball="$BUILT_TARBALL"

  heavy_mark publish
  publish_collection publish "$tarball"
  cat "$HEAVY_WORK/publish.out"

  heavy_wire_re_after publish \
    "POST /proxy/$REG_LOCAL/galaxy/api/v3/artifacts/collections/" \
    "the publish did not reach the multipart endpoint"
  # `imports/collections/{bare id}/` — the route the client *builds*, not the
  # one the server hands it (RFC 0031 §13).
  heavy_wire_re_after publish \
    "GET /proxy/$REG_LOCAL/galaxy/api/v3/imports/collections/" \
    "the client did not poll the import task at the route it builds"
  # **Bounded, not exact.** RFC 0031 §5.4 claims one poll — the task is
  # finished before it exists, so `wait_import_task` sees `finished_at` on its
  # first read and stops. What this asserts is the property that matters: the
  # client is not *looping*. A third poll means it saw no `finished_at`, which
  # is the race §5.4 says cannot happen; exactly-one would also fail on a
  # client that reads the task once more to collect `messages[]`, which is a
  # detail of a version rather than of this server.
  local polls
  polls="$(heavy_wire_count_after publish "GET /proxy/$REG_LOCAL/galaxy/api/v3/imports/collections/")"
  [[ "$polls" -ge 1 && "$polls" -le 2 ]] \
    || heavy_fail "the import task was polled $polls times; it is finished before it exists, \
so the client should stop on its first or second read"

  heavy_mark reinstall
  galaxy_run "$CFG_LOCAL" reinstall collection install "$ns.$name" \
    || heavy_fail "installing the collection we just published failed"
  said "$HEAVY_WORK/reinstall.out" "was installed successfully" \
    "the published collection did not install back"
  heavy_wire_re_after reinstall \
    "GET /proxy/$REG_LOCAL/galaxy/api/v3/artifacts/collections/$ns-$name-$version[.]tar[.]gz" \
    "the tarball was not served from the local registry"

  # A duplicate publish is a 409 the client renders as its own GalaxyError.
  heavy_mark duplicate
  if ANSIBLE_CONFIG="$CFG_LOCAL" ANSIBLE_HOME="$HEAVY_WORK/home-dup" \
      "$GALAXY" collection publish "$tarball" --server batlehub \
      > "$HEAVY_WORK/duplicate.out" 2>&1; then
    heavy_fail "publishing the same version twice should have been refused"
  fi
  said "$HEAVY_WORK/duplicate.out" "409" \
    "the duplicate publish did not surface as a 409"

  # ── a dependency edge between two locally published collections ────────────
  #
  # Everything above is one leaf collection, and a leaf is the one shape of
  # install that never reads `metadata.dependencies`. That field is *composed*
  # here for a locally published version — upstream fills it in for a proxied
  # one — so this is the only way to prove the resolver follows an edge out of a
  # document this server wrote: publish a dependency, publish something that
  # requires it, install only the latter.
  local dep="dep$HEAVY_RUN" app="app$HEAVY_RUN" dep_version="1.2.0"
  local dep_tarball app_tarball
  build_collection "$ns" "$dep" "$dep_version"
  dep_tarball="$BUILT_TARBALL"
  build_collection "$ns" "$app" "1.0.0" "$ns.$dep: \">=1.0.0\""
  app_tarball="$BUILT_TARBALL"
  publish_collection publish-dep "$dep_tarball"
  publish_collection publish-app "$app_tarball"

  heavy_mark depinstall
  galaxy_run "$CFG_LOCAL" depinstall collection install "$ns.$app" \
    || heavy_fail "installing a collection whose dependency is published here failed"
  said "$HEAVY_WORK/depinstall.out" "$ns[.]$dep" \
    "the client did not report installing the dependency"
  heavy_wire_re_after depinstall \
    "GET /proxy/$REG_LOCAL/galaxy/api/v3/collections/$ns/$dep/versions/" \
    "the dependency's listing was never read: the edge in metadata.dependencies was not followed"
  heavy_wire_re_after depinstall \
    "GET /proxy/$REG_LOCAL/galaxy/api/v3/artifacts/collections/$ns-$dep-${dep_version//./[.]}[.]tar[.]gz" \
    "the dependency's tarball was not served from the local registry"
  return 0
}

# ── phase: roles ─────────────────────────────────────────────────────────────

phase_roles() {
  heavy_log "6. roles = proxy routes the archive here; roles = index leaves it on GitHub"

  heavy_mark roleproxy
  galaxy_run "$CFG" roleproxy role install "$ROLE" \
    || heavy_fail "ansible-galaxy role install $ROLE failed through the proxy"
  heavy_wire_re_after roleproxy "GET /proxy/$REG/galaxy/api/v1/roles/[?]" \
    "the role lookup did not reach the v1 search route"
  heavy_wire_re_after roleproxy "GET /proxy/$REG/galaxy/api/v1/roles/[0-9]+/versions/" \
    "the role versions listing was not read through the proxy"
  heavy_wire_re_after roleproxy "GET /proxy/$REG/galaxy/api/v1/roles/[0-9]+/download/" \
    "the role archive was not fetched from this instance: download_url was not rewritten"

  heavy_mark roleindex
  galaxy_run "$CFG_INDEX" roleindex role install "$ROLE" \
    || heavy_fail "ansible-galaxy role install $ROLE failed against roles = index"
  heavy_wire_re_after roleindex "GET /proxy/$REG_INDEX/galaxy/api/v1/roles/[0-9]+/versions/" \
    "the listing should still be served under roles = index"
  local hits
  hits="$(heavy_wire_count_after roleindex "/proxy/$REG_INDEX/galaxy/api/v1/roles/[0-9]+/download/")"
  [[ "$hits" == "0" ]] \
    || heavy_fail "roles = index served $hits role downloads; it must relay download_url"

  # roles = off removes v1 from the discovery document, so the client stops on
  # its own version check rather than on a 404.
  heavy_mark roleoff
  if galaxy_run "$CFG_LOCAL" roleoff role install "$ROLE"; then
    heavy_fail "roles = off should have refused a role install"
  fi
  said "$HEAVY_WORK/roleoff.out" "requires API versions" \
    "the client did not stop on its own API-version check — a bare 404 would read as a proxy fault"
  return 0
}

# ── phase: cache ─────────────────────────────────────────────────────────────

phase_cache() {
  heavy_log "7. A second install is a cache hit"

  # Self-contained: a warm install first, so this phase measures a cache *hit*
  # whether or not an earlier phase ran. `bash tests/heavy/galaxy.sh cache`
  # would otherwise take the first fetch and report that nothing moved.
  galaxy_run "$CFG" cachewarm collection install "$COLLECTION" \
    || heavy_fail "the warming install failed"

  local before after
  before="$(curl -fsS "$HEAVY_BASE/metrics" | awk '/^batlehub_artifact_cache_hits_total/ { n += $2 } END { print n + 0 }')"
  heavy_mark cache
  # A fresh ANSIBLE_HOME, so the *client* has nothing cached and the hit is the
  # server's: `galaxy_run` wipes the home it is given.
  galaxy_run "$CFG" cache collection install "$COLLECTION" \
    || heavy_fail "the second install failed"
  after="$(curl -fsS "$HEAVY_BASE/metrics" | awk '/^batlehub_artifact_cache_hits_total/ { n += $2 } END { print n + 0 }')"
  [[ "$after" -gt "$before" ]] \
    || heavy_fail "batlehub_artifact_cache_hits_total did not move ($before → $after)"
  return 0
}

PHASES=(install blocked stale pinned publish roles cache)

if [[ $# -gt 0 ]]; then
  for name in "$@"; do
    declare -F "phase_$name" > /dev/null || heavy_fail "no phase named '$name'"
    "phase_$name"
  done
else
  for name in "${PHASES[@]}"; do
    "phase_$name"
  done
fi

heavy_done "GALAXY HEAVY SUITE PASSED"
