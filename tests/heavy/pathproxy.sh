#!/usr/bin/env bash
# Heavy path-proxy integration test — the real `apt` (and `dnf`) against the
# deb and rpm registries, measuring the RFC 0018 §4.4 axes for the family
# whose listings cannot be filtered.
#
# The `Packages` / `repomd.xml` indexes are signed upstream and BatleHub does
# not hold the key, so a held version stays listed and the download refusal is
# the *entire* contract for this family. That makes these the least measured
# rejections of all and the reason the RFC recommends `mode = "warn"` here.
#
#   Hide     None possible: the index keeps naming the version. Measured, so
#            the doc page's claim is observed rather than reasoned.
#   Refuse   What the client prints on the native block status and on the
#            other one, whether it retries, whether Retry-After matters.
#   Recover  Same state directory (apt's lists, dnf's cache) after the block
#            lifts: does the next download succeed.
#   Publish  None: these clients do not publish.
#
# apt runs unprivileged with its state, cache and sources redirected into the
# run's work directory; the archive's signature is verified with the keyring
# the host already has. The dnf half needs the `dnf` package (Ubuntu ships one)
# and is refused, not skipped, when it is absent — unless
# HEAVY_PATHPROXY_SKIP_DNF=1 says so explicitly, in which case the banner
# records that the rpm rows were not measured.
#
# Run via `task test:pathproxy-heavy` or directly. Needs network:
# archive.ubuntu.com and dl.fedoraproject.org. Environment knobs: DATABASE_URL
# (required), HEAVY_PORT (8105), HEAVY_TAP_PORT (8115), COVERAGE,
# HEAVY_APT_SUITE (noble), HEAVY_APT_PACKAGE (hello), HEAVY_APT_KEYRING
# (/usr/share/keyrings/ubuntu-archive-keyring.gpg), HEAVY_DNF_PACKAGE (htop).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init pathproxy 8105 8115
heavy_need apt-get "apt"
heavy_need python3 "python3 (the wire tap)"
SKIP_DNF="${HEAVY_PATHPROXY_SKIP_DNF:-0}"
[[ "$SKIP_DNF" == "1" ]] || heavy_need dnf "the dnf package (apt-get install dnf), or set HEAVY_PATHPROXY_SKIP_DNF=1 to leave the rpm rows unmeasured"
# `download` is a plugin subcommand, and the base package ships without it. The
# rpm rows are built on it, so probe here rather than discovering it after the
# server is already built, started and three requests in.
[[ "$SKIP_DNF" == "1" ]] || dnf download --help >/dev/null 2>&1 || heavy_fail \
  "dnf has no 'download' subcommand — install the plugins (apt-get install dnf-plugins-core), \
or set HEAVY_PATHPROXY_SKIP_DNF=1 to leave the rpm rows unmeasured"

DEB="deb-$HEAVY_RUN"
RPM="rpm-$HEAVY_RUN"
SUITE="${HEAVY_APT_SUITE:-noble}"
PKG="${HEAVY_APT_PACKAGE:-hello}"
KEYRING="${HEAVY_APT_KEYRING:-/usr/share/keyrings/ubuntu-archive-keyring.gpg}"
[[ -r "$KEYRING" ]] || heavy_fail "no archive keyring at $KEYRING — set HEAVY_APT_KEYRING"
DNF_PKG="${HEAVY_DNF_PACKAGE:-htop}"

heavy_start_server tests/heavy/config.pathproxy.toml
heavy_start_tap

# ── apt ──────────────────────────────────────────────────────────────────────

APT_URL="$HEAVY_TAP_BASE/proxy/$DEB/deb"
APT_ARCH=$(dpkg --print-architecture)

# apt_state <name> — an apt state/cache tree pointing at the proxy; echoes it.
apt_state() {
  local root="$HEAVY_WORK/apt-$1"
  mkdir -p "$root/state/lists/partial" "$root/cache/archives/partial" "$root/out"
  echo "deb [arch=$APT_ARCH signed-by=$KEYRING] $APT_URL $SUITE main" >"$root/sources.list"
  echo "$root"
  return $?
}

# run_apt <root> <args...> — apt-get, unprivileged, output in RUN_OUT.
run_apt() {
  local root="$1"
  shift
  (cd "$root/out" && apt-get -q \
    -o "Dir::State=$root/state" -o "Dir::Cache=$root/cache" \
    -o "Dir::Etc::SourceList=$root/sources.list" -o "Dir::Etc::SourceParts=/dev/null" \
    -o "Dir::Etc::Preferences=/dev/null" -o "Dir::Etc::PreferencesParts=/dev/null" \
    -o "Dir::Etc::Main=/dev/null" -o "Dir::Etc::Parts=/dev/null" \
    -o "Acquire::Languages=none" -o "Debug::NoLocking=true" \
    "$@") >"$RUN_OUT" 2>&1
  return $?
}

heavy_mark apt-update
RUN_OUT="$HEAVY_WORK/apt-update.txt"
ROOT0=$(apt_state 0)
run_apt "$ROOT0" update || { cat "$RUN_OUT" >&2; heavy_fail "apt-get update through the proxy failed"; }
heavy_wire_after apt-update "GET /proxy/$DEB/deb/dists/$SUITE/InRelease -> 200" \
  "apt did not fetch the signed InRelease through the proxy"
heavy_wire_after apt-update "GET /proxy/$DEB/deb/dists/$SUITE/main/binary-$APT_ARCH/"

# The file apt will ask for, from apt itself rather than a guess.
RUN_OUT="$HEAVY_WORK/apt-uris.txt"
run_apt "$ROOT0" download --print-uris "$PKG" || { cat "$RUN_OUT" >&2; heavy_fail "apt-get download --print-uris $PKG failed"; }
DEB_URL=$(grep -o "'[^']*\.deb'" "$RUN_OUT" | head -1 | tr -d "'")
[[ -n "$DEB_URL" ]] || { cat "$RUN_OUT" >&2; heavy_fail "apt printed no .deb URI for $PKG"; }
DEB_PATH="${DEB_URL#"$APT_URL"/}"
DEB_ROUTE="/proxy/$DEB/deb/$DEB_PATH"
heavy_log "apt will fetch $DEB_PATH"

heavy_mark apt-before
RUN_OUT="$HEAVY_WORK/apt-before.txt"
run_apt "$ROOT0" download "$PKG" || { cat "$RUN_OUT" >&2; heavy_fail "apt-get download $PKG through the proxy failed"; }
heavy_wire_after apt-before "GET $DEB_ROUTE -> 200"
ls "$ROOT0/out"/*.deb >/dev/null 2>&1 || heavy_fail "apt reported success but wrote no .deb"

heavy_log "Blocking the artifact $DEB_PATH"
heavy_block "$DEB" repo _ "$DEB_PATH"

# Hide: the index is signed; the candidate is still there.
heavy_mark apt-hide
RUN_OUT="$HEAVY_WORK/apt-hide.txt"
ROOT1=$(apt_state 1)
run_apt "$ROOT1" update || { cat "$RUN_OUT" >&2; heavy_fail "apt-get update after the block failed"; }
RUN_OUT="$HEAVY_WORK/apt-policy.txt"
(apt-cache -o "Dir::State=$ROOT1/state" -o "Dir::Cache=$ROOT1/cache" -o "Dir::Etc::SourceList=$ROOT1/sources.list" \
  -o "Dir::Etc::SourceParts=/dev/null" policy "$PKG") >"$RUN_OUT" 2>&1
grep -q "Candidate: [0-9]" "$RUN_OUT" || { cat "$RUN_OUT" >&2; heavy_fail "Hide: the blocked package lost its candidate — the signed index was altered?"; }
heavy_log "Hide: none — the signed index still names the candidate ($(grep Candidate "$RUN_OUT" | tr -s ' '))"

heavy_mark apt-native
RUN_OUT="$HEAVY_WORK/apt-native.txt"
rm -f "$ROOT1/out"/*.deb
if run_apt "$ROOT1" download "$PKG"; then
  cat "$RUN_OUT" >&2
  heavy_fail "Refuse: apt-get download of the blocked file succeeded"
fi
FIRST=$(awk -v mark="### apt-native" -v p="GET $DEB_ROUTE -> " '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { print; exit }' "$HEAVY_LOG")
[[ -n "$FIRST" ]] || heavy_fail "Refuse: apt never asked for the blocked file"
NATIVE=$(echo "$FIRST" | sed 's/.* -> \([0-9]*\).*/\1/')
NATIVE_TRIES=$(awk -v mark="### apt-native" -v p="GET $DEB_ROUTE -> " '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { c++ } END { print c + 0 }' "$HEAVY_LOG")
heavy_log "Refuse/native: the block answers $NATIVE ($NATIVE_TRIES request(s)); apt said:"
grep -i "err\|fail\|E:" "$RUN_OUT" | head -3 >&2

case "$NATIVE" in
  403) OTHER=404 ;;
  404) OTHER=403 ;;
  *) heavy_fail "the block answered $NATIVE, neither 403 nor 404" ;;
esac
heavy_tap_rewrite GET "$DEB_ROUTE" "$NATIVE" "$OTHER" "Retry-After: 30"
heavy_mark apt-other
RUN_OUT="$HEAVY_WORK/apt-other.txt"
ROOT2=$(apt_state 2)
run_apt "$ROOT2" update >/dev/null 2>&1 || true
START=$(date +%s)
if run_apt "$ROOT2" download "$PKG"; then
  heavy_fail "Refuse: apt-get download succeeded on a $OTHER"
fi
ELAPSED=$(( $(date +%s) - START ))
heavy_wire_after apt-other "GET $DEB_ROUTE -> $NATIVE=>$OTHER"
OTHER_TRIES=$(awk -v mark="### apt-other" -v p="GET $DEB_ROUTE -> " '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { c++ } END { print c + 0 }' "$HEAVY_LOG")
heavy_log "Refuse/$OTHER: apt asked $OTHER_TRIES time(s), took ${ELAPSED}s with Retry-After: 30, and said:"
grep -i "err\|fail\|E:" "$RUN_OUT" | head -3 >&2
[[ "$ELAPSED" -lt 25 ]] || heavy_fail "apt waited on Retry-After — the CI contract assumes it does not"
heavy_tap_rewrite_clear

heavy_log "Unblocking the artifact"
heavy_unblock "$DEB" repo _ "$DEB_PATH"
heavy_mark apt-recover
RUN_OUT="$HEAVY_WORK/apt-recover.txt"
run_apt "$ROOT1" download "$PKG" \
  || { cat "$RUN_OUT" >&2; heavy_fail "Recover: apt-get download after the unblock failed — the client remembered the refusal"; }
heavy_wire_after apt-recover "GET $DEB_ROUTE -> 200"
heavy_log "Recover: the same apt state downloaded $PKG after the unblock"

APT_SUMMARY="apt: hide=none refuse=$NATIVE(x$NATIVE_TRIES)/$OTHER(x$OTHER_TRIES) recover=ok"

# ── dnf ──────────────────────────────────────────────────────────────────────

if [[ "$SKIP_DNF" == "1" ]]; then
  DNF_SUMMARY="dnf: NOT MEASURED (HEAVY_PATHPROXY_SKIP_DNF=1)"
else
  RPM_URL="$HEAVY_TAP_BASE/proxy/$RPM/rpm"

  # dnf_state <name>
  dnf_state() {
    local root="$HEAVY_WORK/dnf-$1"
    mkdir -p "$root/repos" "$root/cache" "$root/log" "$root/persist" "$root/out"
    cat >"$root/repos/heavy.repo" <<EOF
[heavy]
name=heavy
baseurl=$RPM_URL/
enabled=1
gpgcheck=0
EOF
    echo "$root"
    return $?
  }

  # run_dnf <root> <args...> — dnf, unprivileged, every directory redirected.
  run_dnf() {
    local root="$1"
    shift
    dnf -q -y --releasever=9 \
      --setopt="reposdir=$root/repos" --setopt="cachedir=$root/cache" \
      --setopt="logdir=$root/log" --setopt="persistdir=$root/persist" \
      --disablerepo='*' --enablerepo=heavy "$@" >"$RUN_OUT" 2>&1
    return $?
  }

  heavy_mark dnf-before
  RUN_OUT="$HEAVY_WORK/dnf-before.txt"
  DROOT0=$(dnf_state 0)
  run_dnf "$DROOT0" makecache || { cat "$RUN_OUT" >&2; heavy_fail "dnf makecache through the proxy failed"; }
  heavy_wire_after dnf-before "GET /proxy/$RPM/rpm/repodata/repomd.xml -> 200"
  RUN_OUT="$HEAVY_WORK/dnf-location.txt"
  run_dnf "$DROOT0" repoquery --location "$DNF_PKG" || { cat "$RUN_OUT" >&2; heavy_fail "dnf repoquery --location $DNF_PKG failed"; }
  RPM_FILE_URL=$(grep -o "http[^ ]*\.rpm" "$RUN_OUT" | head -1)
  [[ -n "$RPM_FILE_URL" ]] || { cat "$RUN_OUT" >&2; heavy_fail "dnf printed no .rpm location for $DNF_PKG"; }
  RPM_PATH="${RPM_FILE_URL#"$RPM_URL"/}"
  RPM_ROUTE="/proxy/$RPM/rpm/$RPM_PATH"
  RUN_OUT="$HEAVY_WORK/dnf-download.txt"
  run_dnf "$DROOT0" download --destdir="$DROOT0/out" "$DNF_PKG" \
    || { cat "$RUN_OUT" >&2; heavy_fail "dnf download $DNF_PKG through the proxy failed"; }
  heavy_wire_after dnf-before "GET $RPM_ROUTE -> 200"

  heavy_block "$RPM" repo _ "$RPM_PATH"
  heavy_mark dnf-native
  RUN_OUT="$HEAVY_WORK/dnf-native.txt"
  DROOT1=$(dnf_state 1)
  run_dnf "$DROOT1" makecache >/dev/null 2>&1 || true
  if run_dnf "$DROOT1" download --destdir="$DROOT1/out" "$DNF_PKG"; then
    cat "$RUN_OUT" >&2
    heavy_fail "Refuse: dnf download of the blocked file succeeded"
  fi
  FIRST=$(awk -v mark="### dnf-native" -v p="GET $RPM_ROUTE -> " '
    index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { print; exit }' "$HEAVY_LOG")
  [[ -n "$FIRST" ]] || heavy_fail "Refuse: dnf never asked for the blocked file"
  DNF_NATIVE=$(echo "$FIRST" | sed 's/.* -> \([0-9]*\).*/\1/')
  DNF_TRIES=$(awk -v mark="### dnf-native" -v p="GET $RPM_ROUTE -> " '
    index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { c++ } END { print c + 0 }' "$HEAVY_LOG")
  heavy_log "Refuse/native: the block answers $DNF_NATIVE ($DNF_TRIES request(s)); dnf said:"
  grep -i "error\|fail" "$RUN_OUT" | head -3 >&2

  case "$DNF_NATIVE" in 403) DNF_OTHER=404 ;; *) DNF_OTHER=403 ;; esac
  heavy_tap_rewrite GET "$RPM_ROUTE" "$DNF_NATIVE" "$DNF_OTHER" "Retry-After: 30"
  heavy_mark dnf-other
  RUN_OUT="$HEAVY_WORK/dnf-other.txt"
  DROOT2=$(dnf_state 2)
  run_dnf "$DROOT2" makecache >/dev/null 2>&1 || true
  START=$(date +%s)
  if run_dnf "$DROOT2" download --destdir="$DROOT2/out" "$DNF_PKG"; then
    heavy_fail "Refuse: dnf download succeeded on a $DNF_OTHER"
  fi
  ELAPSED=$(( $(date +%s) - START ))
  heavy_wire_after dnf-other "GET $RPM_ROUTE -> $DNF_NATIVE=>$DNF_OTHER"
  heavy_log "Refuse/$DNF_OTHER: took ${ELAPSED}s with Retry-After: 30; dnf said:"
  grep -i "error\|fail" "$RUN_OUT" | head -3 >&2
  heavy_tap_rewrite_clear

  heavy_unblock "$RPM" repo _ "$RPM_PATH"
  heavy_mark dnf-recover
  RUN_OUT="$HEAVY_WORK/dnf-recover.txt"
  run_dnf "$DROOT1" download --destdir="$DROOT1/out" "$DNF_PKG" \
    || { cat "$RUN_OUT" >&2; heavy_fail "Recover: dnf download after the unblock failed — the client remembered the refusal"; }
  heavy_wire_after dnf-recover "GET $RPM_ROUTE -> 200"
  DNF_SUMMARY="dnf: hide=none refuse=$DNF_NATIVE(x$DNF_TRIES)/$DNF_OTHER recover=ok"
fi

heavy_done "pathproxy heavy test passed: $APT_SUMMARY; $DNF_SUMMARY"
